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

use crate::database::{Database, DatabaseError, StoredDocument};

use super::session::{ChatError, ChatLimits, ChatSession, ChatSessionSnapshot};

const DOCUMENT_SCOPE: &str = "agent";
const DOCUMENT_KEY: &str = "chat-session";
const DOCUMENT_FORMAT_VERSION: u32 = 1;
pub(super) const MAX_SESSION_BYTES: usize = 2 * 1024 * 1024;

/// 序列化后台任务共享的单会话存储。
pub(super) struct ChatSessionStore {
    database: Option<Arc<Database>>,
    latest_revision: AtomicU64,
    highest_attempted_revision: AtomicU64,
    write_lock: Mutex<()>,
}

impl ChatSessionStore {
    /// 从数据库恢复会话；数据库无记录时返回空会话。
    ///
    /// # Errors
    ///
    /// 数据库查询、快照解析或会话内容校验失败时返回错误。
    pub(super) async fn load(
        database: Arc<Database>,
    ) -> Result<(ChatSession, Arc<Self>), ChatStoreError> {
        let session = if let Some(document) = database
            .read_document(DOCUMENT_SCOPE, DOCUMENT_KEY)
            .await
            .map_err(ChatStoreError::Database)?
        {
            session_from_document(&document)?
        } else {
            ChatSession::default()
        };
        Ok((session, Arc::new(Self::new(Some(database)))))
    }

    /// 创建测试使用的可写空存储。
    #[cfg(test)]
    pub(super) fn empty(database: Arc<Database>) -> Arc<Self> {
        Arc::new(Self::new(Some(database)))
    }

    /// 数据库初始化失败时保留会话运行能力，但明确拒绝伪装成持久化成功。
    pub(super) fn unavailable() -> Arc<Self> {
        Arc::new(Self::new(None))
    }

    fn new(database: Option<Arc<Database>>) -> Self {
        Self {
            database,
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
                DOCUMENT_KEY,
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

fn session_from_document(document: &StoredDocument) -> Result<ChatSession, ChatStoreError> {
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
    ChatSession::from_snapshot(snapshot, ChatLimits::default()).map_err(ChatStoreError::Session)
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
