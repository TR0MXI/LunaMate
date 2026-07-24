//! 在独立文件中恢复和原子替换单会话快照。

use std::{
    error::Error,
    fmt, fs,
    io::{self, Read as _},
    path::PathBuf,
    sync::Arc,
};

use parking_lot::Mutex;

use crate::database::{AtomicReplaceOperation, atomic_replace};

use super::session::{ChatError, ChatLimits, ChatSession, ChatSessionSnapshot};

const MAX_SESSION_FILE_BYTES: u64 = 2 * 1024 * 1024;

/// 序列化后台任务共享的单会话存储。
pub(super) struct ChatSessionStore {
    path: PathBuf,
    latest_revision: Mutex<u64>,
}

impl ChatSessionStore {
    /// 从磁盘恢复会话并创建带 revision 防回退的共享存储。
    ///
    /// 不存在的文件会返回空会话。
    ///
    /// # Errors
    ///
    /// 文件过大、无法读取、JSON 损坏或快照不满足会话约束时返回错误。
    pub(super) fn load(path: PathBuf) -> Result<(ChatSession, Arc<Self>), ChatStoreError> {
        let file = match fs::File::open(&path) {
            Ok(file) => file,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                return Ok((ChatSession::default(), Arc::new(Self::new(path, 0))));
            }
            Err(source) => return Err(ChatStoreError::io("读取聊天会话", path, source)),
        };
        if file
            .metadata()
            .map_err(|source| ChatStoreError::io("读取聊天会话元数据", path.clone(), source))?
            .len()
            > MAX_SESSION_FILE_BYTES
        {
            return Err(ChatStoreError::TooLarge(path));
        }
        let mut source = Vec::new();
        file.take(MAX_SESSION_FILE_BYTES + 1)
            .read_to_end(&mut source)
            .map_err(|source| ChatStoreError::io("读取聊天会话", path.clone(), source))?;
        if source.len() as u64 > MAX_SESSION_FILE_BYTES {
            return Err(ChatStoreError::TooLarge(path));
        }
        let snapshot: ChatSessionSnapshot =
            serde_json::from_slice(&source).map_err(|source| ChatStoreError::Format {
                path: path.clone(),
                source,
            })?;
        let session = ChatSession::from_snapshot(snapshot, ChatLimits::default())
            .map_err(ChatStoreError::Session)?;
        // revision 只用于当前进程内排序；重启后所有旧后台任务都已消失，不能信任磁盘值作为新的起点。
        Ok((session, Arc::new(Self::new(path, 0))))
    }

    /// 创建空存储，用于损坏快照降级后覆盖为下一份有效状态。
    pub(super) fn empty(path: PathBuf) -> Arc<Self> {
        Arc::new(Self::new(path, 0))
    }

    fn new(path: PathBuf, latest_revision: u64) -> Self {
        Self {
            path,
            latest_revision: Mutex::new(latest_revision),
        }
    }

    /// 返回恢复或最近成功写入的 revision。
    pub(super) fn latest_revision(&self) -> u64 {
        *self.latest_revision.lock()
    }

    /// 仅当 revision 更新时写入快照，避免迟到后台任务覆盖新状态。
    ///
    /// # Errors
    ///
    /// JSON 序列化或文件系统操作失败时返回错误。
    pub(super) fn save(&self, snapshot: ChatSessionSnapshot) -> Result<(), ChatStoreError> {
        let mut latest_revision = self.latest_revision.lock();
        if snapshot.revision <= *latest_revision {
            return Ok(());
        }
        let source = serde_json::to_vec(&snapshot).map_err(ChatStoreError::Serialize)?;
        let parent = self
            .path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty());
        if let Some(parent) = parent {
            fs::create_dir_all(parent).map_err(|source| {
                ChatStoreError::io("创建聊天会话目录", parent.to_path_buf(), source)
            })?;
        }

        atomic_replace(&self.path, &source, snapshot.revision).map_err(chat_atomic_error)?;
        *latest_revision = snapshot.revision;
        Ok(())
    }
}

fn chat_atomic_error(error: crate::database::AtomicReplaceError) -> ChatStoreError {
    let (operation, path, source) = error.into_parts();
    let operation = match operation {
        AtomicReplaceOperation::CreateTemporary => "创建聊天会话临时文件",
        #[cfg(unix)]
        AtomicReplaceOperation::SetPermissions => "设置聊天会话临时文件权限",
        AtomicReplaceOperation::WriteTemporary => "写入聊天会话临时文件",
        AtomicReplaceOperation::SyncTemporary => "同步聊天会话临时文件",
        AtomicReplaceOperation::Replace => "提交聊天会话",
        #[cfg(unix)]
        AtomicReplaceOperation::SyncParent => "同步聊天会话目录",
    };
    ChatStoreError::io(operation, path, source)
}

/// 描述单会话加载或保存失败。
#[derive(Debug)]
pub(super) enum ChatStoreError {
    Io {
        operation: &'static str,
        path: PathBuf,
        source: io::Error,
    },
    Format {
        path: PathBuf,
        source: serde_json::Error,
    },
    Serialize(serde_json::Error),
    TooLarge(PathBuf),
    Session(ChatError),
}

impl ChatStoreError {
    fn io(operation: &'static str, path: PathBuf, source: io::Error) -> Self {
        Self::Io {
            operation,
            path,
            source,
        }
    }
}

impl fmt::Display for ChatStoreError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io {
                operation,
                path,
                source,
            } => write!(formatter, "{operation} {} 失败：{source}", path.display()),
            Self::Format { path, source } => {
                write!(formatter, "聊天会话 {} 无法解析：{source}", path.display())
            }
            Self::Serialize(source) => write!(formatter, "聊天会话无法序列化：{source}"),
            Self::TooLarge(path) => write!(
                formatter,
                "聊天会话 {} 超过 {MAX_SESSION_FILE_BYTES} 字节上限",
                path.display()
            ),
            Self::Session(source) => write!(formatter, "聊天会话内容无效：{source}"),
        }
    }
}

impl Error for ChatStoreError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Io { source, .. } => Some(source),
            Self::Format { source, .. } | Self::Serialize(source) => Some(source),
            Self::Session(source) => Some(source),
            Self::TooLarge(_) => None,
        }
    }
}
