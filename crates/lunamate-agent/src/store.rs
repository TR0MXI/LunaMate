//! 通过宿主持久化回调恢复和串行提交单会话快照。

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
use tokio::sync::Notify;

use crate::{
    config::DEFAULT_PERSONA_ID,
    persistence::{AgentPersistenceCallbacks, PersistenceError, SessionDocument},
};

use super::{
    memory::{ContextMessage, ContextUsage},
    session::{ChatError, ChatLimits, ChatSession, ChatSessionSnapshot},
};

const DOCUMENT_FORMAT_VERSION: u32 = 1;
// 宿主后端还应设置更高的硬上限，使 Agent 能在读取路径独立诊断会话超限。
pub(super) const MAX_SESSION_BYTES: usize = 7 * 1024 * 1024;

/// 同一 Agent 句柄内所有会话文档的有序读改写协调器。
pub type SessionDocumentLock = Arc<SessionDocumentCoordinator>;

#[derive(Default)]
pub struct SessionDocumentCoordinator {
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
pub struct SessionOperationReservation {
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
    pub fn new() -> Self {
        Self {
            next_sequence: AtomicU64::new(0),
            state: SyncMutex::new(SessionCoordinatorState::default()),
            notify: Notify::new(),
        }
    }

    pub fn reserve(self: &Arc<Self>) -> SessionOperationReservation {
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

/// 返回某个人格已落盘的短期上下文占用；没有记录时返回零。
///
/// 只用于该人格当前未被加载的情况；已加载人格的实时占用由视图直接发布。
///
/// # Errors
///
/// 持久化读取、版本校验或快照解析失败时返回错误。
pub(super) async fn persona_context_usage(
    persistence: &AgentPersistenceCallbacks,
    document_lock: &SessionDocumentLock,
    persona_id: &str,
    limits: ContextUsage,
) -> Result<ContextUsage, ChatStoreError> {
    Ok(
        stored_persona_session(persistence, document_lock, persona_id, limits)
            .await?
            .usage(),
    )
}

/// 返回某个人格已落盘且符合当前限制的可编辑消息。
pub(super) async fn persona_context_messages(
    persistence: &AgentPersistenceCallbacks,
    document_lock: &SessionDocumentLock,
    persona_id: &str,
    limits: ContextUsage,
) -> Result<Vec<ContextMessage>, ChatStoreError> {
    Ok(
        stored_persona_session(persistence, document_lock, persona_id, limits)
            .await?
            .editable_messages(),
    )
}

async fn stored_persona_session(
    persistence: &AgentPersistenceCallbacks,
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
    let Some(document) = persistence
        .load_session(persona_id)
        .await
        .map_err(ChatStoreError::Persistence)?
    else {
        return ChatSession::new(limits).map_err(ChatStoreError::Session);
    };
    session_from_document(&document, limits)
}

/// 删除某个非当前人格的会话文档；当前人格由持有会话的视图直接写入空快照。
///
/// # Errors
///
/// 持久化删除失败时返回错误。
#[cfg(test)]
pub(super) async fn delete_persona_session(
    persistence: &AgentPersistenceCallbacks,
    persona_id: &str,
) -> Result<(), ChatStoreError> {
    delete_persona_session_with_lock(
        persistence,
        &Arc::new(SessionDocumentCoordinator::new()),
        persona_id,
    )
    .await
}

/// 在共享屏障内删除会话，避免与活动 Store 或设置页读改写交错。
#[cfg(test)]
pub(super) async fn delete_persona_session_with_lock(
    persistence: &AgentPersistenceCallbacks,
    document_lock: &SessionDocumentLock,
    persona_id: &str,
) -> Result<(), ChatStoreError> {
    let operation = document_lock.reserve();
    delete_persona_session_reserved(persistence, persona_id, operation).await
}

pub async fn delete_persona_session_reserved(
    persistence: &AgentPersistenceCallbacks,
    persona_id: &str,
    operation: SessionOperationReservation,
) -> Result<(), ChatStoreError> {
    let Some(_operation) = operation.wait_turn().await else {
        return Ok(());
    };
    persistence
        .delete_session(persona_id)
        .await
        .map_err(ChatStoreError::Persistence)
}

/// 序列化后台任务共享的单人格会话存储。
///
/// 每个人格拥有独立的短期上下文文档，切换人格等同于换一个存储实例，
/// 因此迟到的写任务不可能把上一个人格的对话写进新人格的记录。
pub struct ChatSessionStore {
    persistence: Option<AgentPersistenceCallbacks>,
    document_lock: SessionDocumentLock,
    persona_id: String,
    latest_revision: AtomicU64,
    highest_attempted_revision: AtomicU64,
}

impl ChatSessionStore {
    /// 从宿主持久化恢复指定人格的会话；没有记录时返回空会话。
    ///
    /// # Errors
    ///
    /// 持久化查询、快照解析或会话内容校验失败时返回错误。
    #[cfg(test)]
    pub(super) async fn load(
        persistence: AgentPersistenceCallbacks,
        persona_id: &str,
        limits: ChatLimits,
    ) -> Result<(ChatSession, Arc<Self>), ChatStoreError> {
        Self::load_with_lock(
            persistence,
            persona_id,
            limits,
            Arc::new(SessionDocumentCoordinator::new()),
        )
        .await
    }

    /// 使用 Agent 共享的会话文档屏障恢复人格上下文。
    pub async fn load_with_lock(
        persistence: AgentPersistenceCallbacks,
        persona_id: &str,
        limits: ChatLimits,
        document_lock: SessionDocumentLock,
    ) -> Result<(ChatSession, Arc<Self>), ChatStoreError> {
        let operation = document_lock.reserve();
        Self::load_reserved(persistence, persona_id, limits, document_lock, operation).await
    }

    pub fn unavailable() -> Arc<Self> {
        Arc::new(Self::new(
            None,
            DEFAULT_PERSONA_ID.to_owned(),
            Arc::new(SessionDocumentCoordinator::new()),
        ))
    }

    /// 为被拒绝的活动会话文档创建空的可写存储，不读取或改写原文档。
    pub fn empty_with_lock(
        persistence: AgentPersistenceCallbacks,
        persona_id: &str,
        document_lock: SessionDocumentLock,
    ) -> Arc<Self> {
        Arc::new(Self::new(
            Some(persistence),
            persona_id.to_owned(),
            document_lock,
        ))
    }

    async fn load_reserved(
        persistence: AgentPersistenceCallbacks,
        persona_id: &str,
        limits: ChatLimits,
        document_lock: SessionDocumentLock,
        operation: SessionOperationReservation,
    ) -> Result<(ChatSession, Arc<Self>), ChatStoreError> {
        let session = {
            let Some(_operation) = operation.wait_turn().await else {
                return Err(ChatStoreError::Superseded);
            };
            if let Some(document) = persistence
                .load_session(persona_id)
                .await
                .map_err(ChatStoreError::Persistence)?
            {
                session_from_document(&document, limits)?
            } else {
                ChatSession::new(limits).map_err(ChatStoreError::Session)?
            }
        };
        Ok((
            session,
            Arc::new(Self::new(
                Some(persistence),
                persona_id.to_owned(),
                document_lock,
            )),
        ))
    }

    /// 创建测试使用的可写空存储。
    #[cfg(test)]
    pub(super) fn empty(persistence: AgentPersistenceCallbacks) -> Arc<Self> {
        Self::empty_with_lock(
            persistence,
            DEFAULT_PERSONA_ID,
            Arc::new(SessionDocumentCoordinator::new()),
        )
    }

    fn new(
        persistence: Option<AgentPersistenceCallbacks>,
        persona_id: String,
        document_lock: SessionDocumentLock,
    ) -> Self {
        Self {
            persistence,
            document_lock,
            persona_id,
            // revision 只隔离当前进程的后台任务；重启后不能信任持久化值作为新起点。
            latest_revision: AtomicU64::new(0),
            highest_attempted_revision: AtomicU64::new(0),
        }
    }

    /// 返回当前进程最近成功写入的 revision。
    pub fn latest_revision(&self) -> u64 {
        self.latest_revision.load(Ordering::Acquire)
    }

    /// 持久化不可用时无需构造快照或派发写任务。
    pub fn is_available(&self) -> bool {
        self.persistence.is_some()
    }

    /// 仅当 revision 更新时提交快照，避免迟到后台任务覆盖新状态。
    ///
    /// # Errors
    ///
    /// JSON 序列化、大小校验或宿主提交失败时返回错误。
    #[cfg(test)]
    pub(super) async fn save(&self, snapshot: ChatSessionSnapshot) -> Result<(), ChatStoreError> {
        let operation = self.reserve_document_operation();
        self.save_reserved(snapshot, operation).await
    }

    pub fn reserve_document_operation(&self) -> SessionOperationReservation {
        self.document_lock.reserve()
    }

    pub async fn save_reserved(
        &self,
        snapshot: ChatSessionSnapshot,
        operation: SessionOperationReservation,
    ) -> Result<(), ChatStoreError> {
        let Some(_operation) = operation.wait_turn().await else {
            return Ok(());
        };
        if !self.should_attempt(snapshot.revision) {
            return Ok(());
        }
        let persistence = self
            .persistence
            .as_ref()
            .ok_or(ChatStoreError::Unavailable)?;
        let contents = serialize_snapshot(&snapshot)?;
        persistence
            .save_session(
                &self.persona_id,
                SessionDocument::new(DOCUMENT_FORMAT_VERSION, contents),
            )
            .await
            .map_err(ChatStoreError::Persistence)?;
        self.latest_revision
            .store(snapshot.revision, Ordering::Release);
        Ok(())
    }

    /// 调用方持有文档操作序号时，拒绝已成功或低于最高尝试值的迟到 revision。
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
    persistence: &AgentPersistenceCallbacks,
    document_lock: &SessionDocumentLock,
    persona_id: &str,
    limits: ChatLimits,
    mutation: impl FnOnce(&mut ChatSession) -> Result<bool, ChatError>,
) -> Result<bool, ChatStoreError> {
    let operation = document_lock.reserve();
    mutate_persona_session_reserved(persistence, persona_id, limits, operation, mutation).await
}

pub async fn mutate_persona_session_reserved(
    persistence: &AgentPersistenceCallbacks,
    persona_id: &str,
    limits: ChatLimits,
    operation: SessionOperationReservation,
    mutation: impl FnOnce(&mut ChatSession) -> Result<bool, ChatError>,
) -> Result<bool, ChatStoreError> {
    let Some(_operation) = operation.wait_turn().await else {
        return Ok(false);
    };
    let mut session = if let Some(document) = persistence
        .load_session(persona_id)
        .await
        .map_err(ChatStoreError::Persistence)?
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
    persistence
        .save_session(
            persona_id,
            SessionDocument::new(DOCUMENT_FORMAT_VERSION, contents),
        )
        .await
        .map_err(ChatStoreError::Persistence)?;
    Ok(true)
}

fn session_from_document(
    document: &SessionDocument,
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
pub enum ChatStoreError {
    Format(serde_json::Error),
    Serialize(serde_json::Error),
    TooLarge,
    UnsupportedDocumentVersion(u32),
    Superseded,
    Session(ChatError),
    Persistence(PersistenceError),
    Unavailable,
}

impl ChatStoreError {
    /// 返回不会包含会话内容、人格键或宿主存储路径的稳定分类。
    pub const fn diagnostic_kind(&self) -> &'static str {
        match self {
            Self::Format(_) => "format",
            Self::Serialize(_) => "serialize",
            Self::TooLarge => "too_large",
            Self::UnsupportedDocumentVersion(_) => "unsupported_version",
            Self::Superseded => "superseded",
            Self::Session(_) => "invalid_session",
            Self::Persistence(error) => error.diagnostic_kind(),
            Self::Unavailable => "unavailable",
        }
    }

    pub(crate) const fn is_invalid_document(&self) -> bool {
        matches!(
            self,
            Self::Format(_)
                | Self::TooLarge
                | Self::UnsupportedDocumentVersion(_)
                | Self::Session(ChatError::UnsupportedSnapshot | ChatError::InvalidSnapshot)
        ) || matches!(self, Self::Persistence(error) if error.is_invalid_document())
    }

    pub(crate) const fn is_unsupported_document(&self) -> bool {
        matches!(
            self,
            Self::UnsupportedDocumentVersion(_) | Self::Session(ChatError::UnsupportedSnapshot)
        )
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
            Self::Persistence(source) => write!(formatter, "聊天会话持久化操作失败：{source}"),
            Self::Unavailable => write!(formatter, "Agent 持久化当前不可用"),
        }
    }
}

impl Error for ChatStoreError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Format(source) | Self::Serialize(source) => Some(source),
            Self::Session(source) => Some(source),
            Self::Persistence(source) => Some(source),
            Self::TooLarge
            | Self::UnsupportedDocumentVersion(_)
            | Self::Superseded
            | Self::Unavailable => None,
        }
    }
}
