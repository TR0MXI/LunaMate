use std::{
    fs,
    path::PathBuf,
    time::{SystemTime, UNIX_EPOCH},
};

use crate::agent::{session::ChatSession, store::ChatSessionStore};

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
