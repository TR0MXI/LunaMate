//! 通过数据库恢复和串行提交单会话快照。

use std::{
    collections::BTreeSet,
    error::Error,
    fmt,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
};

use parking_lot::Mutex as SyncMutex;
use tokio::sync::{Mutex as AsyncMutex, Notify};

use crate::{
    config::DEFAULT_PERSONA_ID,
    database::{Database, DatabaseError, StoredDocument},
};

use super::{
    memory::{ContextMessage, ContextUsage},
    session::{ChatError, ChatLimits, ChatSession, ChatSessionSnapshot},
};

const DOCUMENT_SCOPE: &str = "agent";
const DOCUMENT_KEY_PREFIX: &str = "chat-session/";
const DOCUMENT_FORMAT_VERSION: u32 = 1;
// 必须低于 database 文档的 8 MiB 总上限，才能在读取路径独立诊断会话超限。
pub(super) const MAX_SESSION_BYTES: usize = 7 * 1024 * 1024;

/// 同一数据库实例内所有会话文档的有序读改写协调器。
pub(super) type SessionDocumentLock = Arc<SessionDocumentCoordinator>;

#[derive(Default)]
pub(super) struct SessionDocumentCoordinator {
    next_sequence: AtomicU64,
    state: SyncMutex<SessionCoordinatorState>,
    notify: Notify,
}

struct SessionCoordinatorState {
    next_to_run: u64,
    running: bool,
    cancelled: BTreeSet<u64>,
}

impl Default for SessionCoordinatorState {
    fn default() -> Self {
        Self {
            next_to_run: 1,
            running: false,
            cancelled: BTreeSet::new(),
        }
    }
}

impl SessionCoordinatorState {
    fn skip_cancelled(&mut self) {
        while self.cancelled.remove(&self.next_to_run) {
            self.next_to_run = self.next_to_run.wrapping_add(1).max(1);
        }
    }
}

/// 一个已经取得全局顺序、但可能尚未开始执行的会话文档操作。
///
/// 任务在等待期间被取消时，析构会把该序号标记为跳过，避免后续操作永久阻塞。
pub(super) struct SessionOperationReservation {
    coordinator: SessionDocumentLock,
    sequence: u64,
    claimed: bool,
}

impl SessionOperationReservation {
    async fn wait_turn(mut self) -> Option<SessionOperationGuard> {
        loop {
            let notified = self.coordinator.notify.notified();
            {
                let mut state = self.coordinator.state.lock();
                if self.sequence < state.next_to_run {
                    return None;
                }
                if self.sequence == state.next_to_run && !state.running {
                    state.running = true;
                    self.claimed = true;
                    return Some(SessionOperationGuard {
                        coordinator: self.coordinator.clone(),
                    });
                }
            }
            notified.await;
        }
    }
}

impl Drop for SessionOperationReservation {
    fn drop(&mut self) {
        if self.claimed {
            return;
        }
        let mut state = self.coordinator.state.lock();
        if self.sequence >= state.next_to_run {
            state.cancelled.insert(self.sequence);
            if !state.running {
                state.skip_cancelled();
            }
        }
        drop(state);
        self.coordinator.notify.notify_waiters();
    }
}

struct SessionOperationGuard {
    coordinator: SessionDocumentLock,
}

impl Drop for SessionOperationGuard {
    fn drop(&mut self) {
        let mut state = self.coordinator.state.lock();
        state.running = false;
        state.next_to_run = state.next_to_run.wrapping_add(1).max(1);
        state.skip_cancelled();
        drop(state);
        self.coordinator.notify.notify_waiters();
    }
}

impl SessionDocumentCoordinator {
    pub(super) fn new() -> Self {
        Self {
            next_sequence: AtomicU64::new(0),
            state: SyncMutex::new(SessionCoordinatorState::default()),
            notify: Notify::new(),
        }
    }

    pub(super) fn reserve(self: &Arc<Self>) -> SessionOperationReservation {
        let sequence = self
            .next_sequence
            .fetch_add(1, Ordering::AcqRel)
            .wrapping_add(1)
            .max(1);
        SessionOperationReservation {
            coordinator: self.clone(),
            sequence,
            claimed: false,
        }
    }
}

/// 返回某个人格的会话文档键；人格 ID 已在配置层限制为安全字符集。
fn document_key(persona_id: &str) -> String {
    format!("{DOCUMENT_KEY_PREFIX}{persona_id}")
}

/// 返回某个人格已落盘的短期上下文占用；没有记录时返回零。
///
/// 只用于该人格当前未被加载的情况；已加载人格的实时占用由视图直接发布。
///
/// # Errors
///
/// 数据库读取、版本校验或快照解析失败时返回错误。
pub(super) async fn persona_context_usage(
    database: &Database,
    document_lock: &SessionDocumentLock,
    persona_id: &str,
    limits: ContextUsage,
) -> Result<ContextUsage, ChatStoreError> {
    Ok(
        stored_persona_session(database, document_lock, persona_id, limits)
            .await?
            .usage(),
    )
}

/// 返回某个人格已落盘且符合当前限制的可编辑消息。
pub(super) async fn persona_context_messages(
    database: &Database,
    document_lock: &SessionDocumentLock,
    persona_id: &str,
    limits: ContextUsage,
) -> Result<Vec<ContextMessage>, ChatStoreError> {
    Ok(
        stored_persona_session(database, document_lock, persona_id, limits)
            .await?
            .editable_messages(),
    )
}

async fn stored_persona_session(
    database: &Database,
    document_lock: &SessionDocumentLock,
    persona_id: &str,
    limits: ContextUsage,
) -> Result<ChatSession, ChatStoreError> {
    let limits = ChatLimits {
        max_messages: limits.max_messages,
        max_tokens: limits.max_tokens,
        max_request_tokens: limits.max_tokens,
    };
    let operation = document_lock.reserve();
    let Some(_operation) = operation.wait_turn().await else {
        return Err(ChatStoreError::Superseded);
    };
    let Some(document) = database
        .read_document(DOCUMENT_SCOPE, &document_key(persona_id))
        .await
        .map_err(ChatStoreError::Database)?
    else {
        return ChatSession::new(limits).map_err(ChatStoreError::Session);
    };
    session_from_document(&document, limits)
}

/// 删除某个非当前人格的会话文档；当前人格由持有会话的视图直接写入空快照。
///
/// # Errors
///
/// 数据库删除失败时返回错误。
#[cfg(test)]
pub(super) async fn delete_persona_session(
    database: &Database,
    persona_id: &str,
) -> Result<(), ChatStoreError> {
    delete_persona_session_with_lock(
        database,
        &Arc::new(SessionDocumentCoordinator::new()),
        persona_id,
    )
    .await
}

/// 在共享屏障内删除会话，避免与活动 Store 或设置页读改写交错。
#[cfg(test)]
pub(super) async fn delete_persona_session_with_lock(
    database: &Database,
    document_lock: &SessionDocumentLock,
    persona_id: &str,
) -> Result<(), ChatStoreError> {
    let operation = document_lock.reserve();
    delete_persona_session_reserved(database, persona_id, operation).await
}

pub(super) async fn delete_persona_session_reserved(
    database: &Database,
    persona_id: &str,
    operation: SessionOperationReservation,
) -> Result<(), ChatStoreError> {
    let Some(_operation) = operation.wait_turn().await else {
        return Ok(());
    };
    let key = document_key(persona_id);
    database
        .delete_document(DOCUMENT_SCOPE, &key)
        .await
        .map_err(ChatStoreError::Database)
}

/// 序列化后台任务共享的单人格会话存储。
///
/// 每个人格拥有独立的短期上下文文档，切换人格等同于换一个存储实例，
/// 因此迟到的写任务不可能把上一个人格的对话写进新人格的记录。
pub(super) struct ChatSessionStore {
    database: Option<Arc<Database>>,
    document_lock: SessionDocumentLock,
    document_key: String,
    latest_revision: AtomicU64,
    highest_attempted_revision: AtomicU64,
    write_lock: AsyncMutex<()>,
}

impl ChatSessionStore {
    /// 从数据库恢复指定人格的会话；数据库无记录时返回空会话。
    ///
    /// # Errors
    ///
    /// 数据库查询、快照解析或会话内容校验失败时返回错误。
    #[cfg(test)]
    pub(super) async fn load(
        database: Arc<Database>,
        persona_id: &str,
        limits: ChatLimits,
    ) -> Result<(ChatSession, Arc<Self>), ChatStoreError> {
        Self::load_with_lock(
            database,
            persona_id,
            limits,
            Arc::new(SessionDocumentCoordinator::new()),
        )
        .await
    }

    /// 使用 Agent 共享的会话文档屏障恢复人格上下文。
    pub(super) async fn load_with_lock(
        database: Arc<Database>,
        persona_id: &str,
        limits: ChatLimits,
        document_lock: SessionDocumentLock,
    ) -> Result<(ChatSession, Arc<Self>), ChatStoreError> {
        let operation = document_lock.reserve();
        Self::load_reserved(database, persona_id, limits, document_lock, operation).await
    }

    pub(super) async fn load_reserved(
        database: Arc<Database>,
        persona_id: &str,
        limits: ChatLimits,
        document_lock: SessionDocumentLock,
        operation: SessionOperationReservation,
    ) -> Result<(ChatSession, Arc<Self>), ChatStoreError> {
        let key = document_key(persona_id);
        let session = {
            let Some(_operation) = operation.wait_turn().await else {
                return Err(ChatStoreError::Superseded);
            };
            if let Some(document) = database
                .read_document(DOCUMENT_SCOPE, &key)
                .await
                .map_err(ChatStoreError::Database)?
            {
                session_from_document(&document, limits)?
            } else {
                ChatSession::new(limits).map_err(ChatStoreError::Session)?
            }
        };
        Ok((
            session,
            Arc::new(Self::new(Some(database), key, document_lock)),
        ))
    }

    /// 创建测试使用的可写空存储。
    #[cfg(test)]
    pub(super) fn empty(database: Arc<Database>) -> Arc<Self> {
        Arc::new(Self::new(
            Some(database),
            document_key(DEFAULT_PERSONA_ID),
            Arc::new(SessionDocumentCoordinator::new()),
        ))
    }

    /// 数据库初始化失败时保留会话运行能力，但明确拒绝伪装成持久化成功。
    pub(super) fn unavailable() -> Arc<Self> {
        Arc::new(Self::new(
            None,
            document_key(DEFAULT_PERSONA_ID),
            Arc::new(SessionDocumentCoordinator::new()),
        ))
    }

    fn new(
        database: Option<Arc<Database>>,
        document_key: String,
        document_lock: SessionDocumentLock,
    ) -> Self {
        Self {
            database,
            document_lock,
            document_key,
            // revision 只隔离当前进程的后台任务；重启后不能信任持久化值作为新起点。
            latest_revision: AtomicU64::new(0),
            highest_attempted_revision: AtomicU64::new(0),
            write_lock: AsyncMutex::new(()),
        }
    }

    /// 返回当前进程最近成功写入的 revision。
    pub(super) fn latest_revision(&self) -> u64 {
        self.latest_revision.load(Ordering::Acquire)
    }

    /// 数据库不可用时无需构造快照或派发写任务。
    pub(super) fn is_available(&self) -> bool {
        self.database.is_some()
    }

    /// 仅当 revision 更新时提交快照，避免迟到后台任务覆盖新状态。
    ///
    /// # Errors
    ///
    /// JSON 序列化、大小校验或数据库提交失败时返回错误。
    #[cfg(test)]
    pub(super) async fn save(&self, snapshot: ChatSessionSnapshot) -> Result<(), ChatStoreError> {
        let operation = self.reserve_document_operation();
        self.save_reserved(snapshot, operation).await
    }

    pub(super) fn reserve_document_operation(&self) -> SessionOperationReservation {
        self.document_lock.reserve()
    }

    pub(super) async fn save_reserved(
        &self,
        snapshot: ChatSessionSnapshot,
        operation: SessionOperationReservation,
    ) -> Result<(), ChatStoreError> {
        let Some(_operation) = operation.wait_turn().await else {
            return Ok(());
        };
        let _guard = self.write_lock.lock().await;
        if !self.should_attempt(snapshot.revision) {
            return Ok(());
        }
        let database = self.database.as_ref().ok_or(ChatStoreError::Unavailable)?;
        let contents = serialize_snapshot(&snapshot)?;
        database
            .write_document(
                DOCUMENT_SCOPE,
                &self.document_key,
                DOCUMENT_FORMAT_VERSION,
                &contents,
            )
            .await
            .map_err(ChatStoreError::Database)?;
        self.latest_revision
            .store(snapshot.revision, Ordering::Release);
        Ok(())
    }

    /// 调用方持有写锁时，拒绝已成功或低于最高尝试值的迟到 revision。
    fn should_attempt(&self, revision: u64) -> bool {
        if revision <= self.latest_revision() {
            return false;
        }
        let highest_attempted = self.highest_attempted_revision.load(Ordering::Acquire);
        if revision < highest_attempted {
            return false;
        }
        self.highest_attempted_revision
            .store(revision, Ordering::Release);
        true
    }
}

/// 在共享文档屏障中完成一次非活动人格的读改写，防止连续编辑互相覆盖。
#[cfg(test)]
pub(super) async fn mutate_persona_session(
    database: &Database,
    document_lock: &SessionDocumentLock,
    persona_id: &str,
    limits: ChatLimits,
    mutation: impl FnOnce(&mut ChatSession) -> Result<bool, ChatError>,
) -> Result<bool, ChatStoreError> {
    let operation = document_lock.reserve();
    mutate_persona_session_reserved(database, persona_id, limits, operation, mutation).await
}

pub(super) async fn mutate_persona_session_reserved(
    database: &Database,
    persona_id: &str,
    limits: ChatLimits,
    operation: SessionOperationReservation,
    mutation: impl FnOnce(&mut ChatSession) -> Result<bool, ChatError>,
) -> Result<bool, ChatStoreError> {
    let Some(_operation) = operation.wait_turn().await else {
        return Ok(false);
    };
    let key = document_key(persona_id);
    let mut session = if let Some(document) = database
        .read_document(DOCUMENT_SCOPE, &key)
        .await
        .map_err(ChatStoreError::Database)?
    {
        session_from_document(&document, limits)?
    } else {
        ChatSession::new(limits).map_err(ChatStoreError::Session)?
    };
    let changed = mutation(&mut session).map_err(ChatStoreError::Session)?;
    if !changed {
        return Ok(false);
    }
    let contents = serialize_snapshot(&session.snapshot(1))?;
    database
        .write_document(DOCUMENT_SCOPE, &key, DOCUMENT_FORMAT_VERSION, &contents)
        .await
        .map_err(ChatStoreError::Database)?;
    Ok(true)
}

fn session_from_document(
    document: &StoredDocument,
    limits: ChatLimits,
) -> Result<ChatSession, ChatStoreError> {
    if document.format_version() != DOCUMENT_FORMAT_VERSION {
        return Err(ChatStoreError::UnsupportedDocumentVersion(
            document.format_version(),
        ));
    }
    if document.contents().len() > MAX_SESSION_BYTES {
        return Err(ChatStoreError::TooLarge);
    }
    let snapshot: ChatSessionSnapshot =
        serde_json::from_slice(document.contents()).map_err(ChatStoreError::Format)?;
    ChatSession::from_snapshot(snapshot, limits).map_err(ChatStoreError::Session)
}

fn serialize_snapshot(snapshot: &ChatSessionSnapshot) -> Result<Vec<u8>, ChatStoreError> {
    let contents = serde_json::to_vec(snapshot).map_err(ChatStoreError::Serialize)?;
    if contents.len() > MAX_SESSION_BYTES {
        return Err(ChatStoreError::TooLarge);
    }
    Ok(contents)
}

/// 描述单会话加载或保存失败。
#[derive(Debug)]
pub(super) enum ChatStoreError {
    Format(serde_json::Error),
    Serialize(serde_json::Error),
    TooLarge,
    UnsupportedDocumentVersion(u32),
    Superseded,
    Session(ChatError),
    Database(DatabaseError),
    Unavailable,
}

impl ChatStoreError {
    /// 返回不会包含会话内容、人格键或数据库路径的稳定分类。
    pub(super) const fn diagnostic_kind(&self) -> &'static str {
        match self {
            Self::Format(_) => "format",
            Self::Serialize(_) => "serialize",
            Self::TooLarge => "too_large",
            Self::UnsupportedDocumentVersion(_) => "unsupported_version",
            Self::Superseded => "superseded",
            Self::Session(_) => "invalid_session",
            Self::Database(error) => error.diagnostic_kind(),
            Self::Unavailable => "unavailable",
        }
    }
}

impl fmt::Display for ChatStoreError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Format(source) => write!(formatter, "聊天会话无法解析：{source}"),
            Self::Serialize(source) => write!(formatter, "聊天会话无法序列化：{source}"),
            Self::TooLarge => write!(formatter, "聊天会话超过 {MAX_SESSION_BYTES} 字节上限"),
            Self::UnsupportedDocumentVersion(version) => {
                write!(formatter, "聊天会话存储版本 {version} 不受支持")
            }
            Self::Superseded => write!(formatter, "聊天会话操作已被更新状态替代"),
            Self::Session(source) => write!(formatter, "聊天会话内容无效：{source}"),
            Self::Database(source) => write!(formatter, "聊天会话数据库操作失败：{source}"),
            Self::Unavailable => write!(formatter, "嵌入式数据库当前不可用"),
        }
    }
}

impl Error for ChatStoreError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Format(source) | Self::Serialize(source) => Some(source),
            Self::Session(source) => Some(source),
            Self::Database(source) => Some(source),
            Self::TooLarge
            | Self::UnsupportedDocumentVersion(_)
            | Self::Superseded
            | Self::Unavailable => None,
        }
    }
}
