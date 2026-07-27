//! 通过数据库恢复和串行提交单会话快照。

use std::{
    error::Error,
    fmt,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
};

use tokio::sync::Mutex;

use crate::{
    config::DEFAULT_PERSONA_ID,
    database::{Database, DatabaseError, StoredDocument},
};

use super::session::{ChatError, ChatLimits, ChatSession, ChatSessionSnapshot};

const DOCUMENT_SCOPE: &str = "agent";
const DOCUMENT_KEY_PREFIX: &str = "chat-session/";
const DOCUMENT_FORMAT_VERSION: u32 = 1;
pub(super) const MAX_SESSION_BYTES: usize = 2 * 1024 * 1024;

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
    persona_id: &str,
) -> Result<(usize, usize), ChatStoreError> {
    let Some(document) = database
        .read_document(DOCUMENT_SCOPE, &document_key(persona_id))
        .await
        .map_err(ChatStoreError::Database)?
    else {
        return Ok((0, 0));
    };
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
    let bytes = snapshot
        .messages
        .iter()
        .map(|message| message.content().len())
        .sum();
    Ok((snapshot.messages.len(), bytes))
}

/// 删除某个非当前人格的会话文档；当前人格由持有会话的视图直接写入空快照。
///
/// # Errors
///
/// 数据库删除失败时返回错误。
pub(super) async fn delete_persona_session(
    database: &Database,
    persona_id: &str,
) -> Result<(), ChatStoreError> {
    database
        .delete_document(DOCUMENT_SCOPE, &document_key(persona_id))
        .await
        .map_err(ChatStoreError::Database)
}

/// 序列化后台任务共享的单人格会话存储。
///
/// 每个人格拥有独立的短期上下文文档，切换人格等同于换一个存储实例，
/// 因此迟到的写任务不可能把上一个人格的对话写进新人格的记录。
pub(super) struct ChatSessionStore {
    database: Option<Arc<Database>>,
    document_key: String,
    latest_revision: AtomicU64,
    highest_attempted_revision: AtomicU64,
    write_lock: Mutex<()>,
}

impl ChatSessionStore {
    /// 从数据库恢复指定人格的会话；数据库无记录时返回空会话。
    ///
    /// # Errors
    ///
    /// 数据库查询、快照解析或会话内容校验失败时返回错误。
    pub(super) async fn load(
        database: Arc<Database>,
        persona_id: &str,
        limits: ChatLimits,
    ) -> Result<(ChatSession, Arc<Self>), ChatStoreError> {
        let key = document_key(persona_id);
        let session = if let Some(document) = database
            .read_document(DOCUMENT_SCOPE, &key)
            .await
            .map_err(ChatStoreError::Database)?
        {
            session_from_document(&document, limits)?
        } else {
            ChatSession::new(limits).map_err(ChatStoreError::Session)?
        };
        Ok((session, Arc::new(Self::new(Some(database), key))))
    }

    /// 创建测试使用的可写空存储。
    #[cfg(test)]
    pub(super) fn empty(database: Arc<Database>) -> Arc<Self> {
        Arc::new(Self::new(Some(database), document_key(DEFAULT_PERSONA_ID)))
    }

    /// 数据库初始化失败时保留会话运行能力，但明确拒绝伪装成持久化成功。
    pub(super) fn unavailable() -> Arc<Self> {
        Arc::new(Self::new(None, document_key(DEFAULT_PERSONA_ID)))
    }

    fn new(database: Option<Arc<Database>>, document_key: String) -> Self {
        Self {
            database,
            document_key,
            // revision 只隔离当前进程的后台任务；重启后不能信任持久化值作为新起点。
            latest_revision: AtomicU64::new(0),
            highest_attempted_revision: AtomicU64::new(0),
            write_lock: Mutex::new(()),
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
    pub(super) async fn save(&self, snapshot: ChatSessionSnapshot) -> Result<(), ChatStoreError> {
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
            Self::TooLarge | Self::UnsupportedDocumentVersion(_) | Self::Unavailable => None,
        }
    }
}
