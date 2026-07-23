//! 在独立文件中恢复和原子替换单会话快照。

use std::{
    error::Error,
    fmt, fs,
    io::{self, Read as _},
    path::PathBuf,
    sync::Arc,
};

use parking_lot::Mutex;

use crate::persistence::{AtomicReplaceOperation, atomic_replace};

use super::session::{ChatError, ChatLimits, ChatSession, ChatSessionSnapshot};

const MAX_SESSION_FILE_BYTES: u64 = 2 * 1024 * 1024;

/// 序列化后台任务共享的单会话存储。
pub(crate) struct ChatSessionStore {
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
    pub(crate) fn load(path: PathBuf) -> Result<(ChatSession, Arc<Self>), ChatStoreError> {
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
    pub(crate) fn empty(path: PathBuf) -> Arc<Self> {
        Arc::new(Self::new(path, 0))
    }

    fn new(path: PathBuf, latest_revision: u64) -> Self {
        Self {
            path,
            latest_revision: Mutex::new(latest_revision),
        }
    }

    /// 返回恢复或最近成功写入的 revision。
    pub(crate) fn latest_revision(&self) -> u64 {
        *self.latest_revision.lock()
    }

    /// 仅当 revision 更新时写入快照，避免迟到后台任务覆盖新状态。
    ///
    /// # Errors
    ///
    /// JSON 序列化或文件系统操作失败时返回错误。
    pub(crate) fn save(&self, snapshot: ChatSessionSnapshot) -> Result<(), ChatStoreError> {
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

fn chat_atomic_error(error: crate::persistence::AtomicReplaceError) -> ChatStoreError {
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
pub(crate) enum ChatStoreError {
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

#[cfg(test)]
mod tests {
    use std::time::{SystemTime, UNIX_EPOCH};

    use super::*;

    struct TestFile(PathBuf);

    impl TestFile {
        fn new() -> Self {
            let nonce = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("系统时间必须晚于 Unix 纪元")
                .as_nanos();
            let directory = std::env::temp_dir().join(format!(
                "lunamate-chat-store-{}-{nonce}",
                std::process::id()
            ));
            fs::create_dir_all(&directory).expect("测试目录应当可以创建");
            Self(directory.join("chat-session.json"))
        }
    }

    impl Drop for TestFile {
        fn drop(&mut self) {
            if let Some(parent) = self.0.parent() {
                let _ = fs::remove_dir_all(parent);
            }
        }
    }

    #[test]
    fn newer_revision_cannot_be_overwritten_by_late_snapshot() {
        let file = TestFile::new();
        let (_, store) = ChatSessionStore::load(file.0.clone()).expect("空会话应当可加载");
        let mut session = ChatSession::default();
        let turn = session.start_turn("hello").expect("测试轮次应当可开始");
        session
            .append_response(turn.response_id, "world")
            .expect("测试回复应当可写入");
        session.finish_response(turn.response_id);

        store.save(session.snapshot(2)).expect("新快照应当可保存");
        store
            .save(ChatSession::default().snapshot(1))
            .expect("旧快照应当被无害忽略");
        let (restored, _) = ChatSessionStore::load(file.0.clone()).expect("快照应当可恢复");
        assert_eq!(restored.messages().len(), 2);
        assert_eq!(restored.messages()[1].content(), "world");
    }

    #[test]
    fn persisted_revision_does_not_block_new_process_writes() {
        let file = TestFile::new();
        let (_, store) = ChatSessionStore::load(file.0.clone()).expect("空会话应当可加载");
        store
            .save(ChatSession::default().snapshot(u64::MAX))
            .expect("当前进程应当可以保存极大 revision");

        let (_, restarted_store) =
            ChatSessionStore::load(file.0.clone()).expect("新进程应当忽略旧 revision 起点");
        restarted_store
            .save(ChatSession::default().snapshot(1))
            .expect("重启后首份快照应当可以保存");
    }

    #[test]
    fn failed_save_does_not_advance_revision_and_can_be_retried() {
        let file = TestFile::new();
        let (_, store) = ChatSessionStore::load(file.0.clone()).expect("空会话应当可加载");
        fs::create_dir(&file.0).expect("冲突目标目录应当可以创建");

        assert!(store.save(ChatSession::default().snapshot(1)).is_err());
        assert_eq!(store.latest_revision(), 0);
        let temporary_files = fs::read_dir(file.0.parent().expect("会话文件必须有父目录"))
            .expect("测试目录应当可以读取")
            .filter_map(Result::ok)
            .filter(|entry| {
                entry
                    .file_name()
                    .to_string_lossy()
                    .starts_with(".chat-session.json.tmp-")
            })
            .count();
        assert_eq!(temporary_files, 0);

        fs::remove_dir(&file.0).expect("冲突目标目录应当可以移除");
        store
            .save(ChatSession::default().snapshot(1))
            .expect("失败后的同 revision 快照应当可以重试");
        assert_eq!(store.latest_revision(), 1);
        ChatSessionStore::load(file.0.clone()).expect("重试保存的快照应当可以恢复");
    }
}
