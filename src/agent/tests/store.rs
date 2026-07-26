//! 验证按人格隔离的会话存储：恢复、revision 串行化与不可信文档处理。

use super::super::{
    session::{ChatLimits, ChatSession},
    store::{ChatSessionStore, ChatStoreError, MAX_SESSION_BYTES, delete_persona_session},
};
use crate::{config::DEFAULT_PERSONA_ID, database::Database};
use std::future::Future;

/// 测试统一使用默认人格；文档键必须与 `ChatSessionStore` 内部拼接结果一致。
const PERSONA: &str = DEFAULT_PERSONA_ID;
const SESSION_KEY: &str = "chat-session/default";

fn run_async<F: Future>(future: F) -> F::Output {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("测试必须能创建 Tokio 运行时")
        .block_on(future)
}

#[test]
fn missing_session_returns_default() {
    run_async(async {
        let database = Database::open_memory().await.expect("内存数据库应可打开");
        let (session, store) = ChatSessionStore::load(database, PERSONA, ChatLimits::default())
            .await
            .expect("缺失会话应降级为空会话");

        assert_eq!(session.messages().len(), 0);
        assert_eq!(store.latest_revision(), 0);
    });
}

#[test]
fn session_round_trip_uses_database() {
    run_async(async {
        let database = Database::open_memory().await.expect("内存数据库应可打开");
        let mut session = ChatSession::default();
        let started = session.start_turn("你好").expect("有效用户消息应被接受");
        session
            .append_response(started.response_id, "你好，有什么可以帮你？")
            .expect("匹配请求的回复应可写入");
        assert!(session.finish_response(started.response_id));

        let store = ChatSessionStore::empty(database.clone());
        store
            .save(session.snapshot(7))
            .await
            .expect("会话应保存到数据库");

        let (restored, restored_store) =
            ChatSessionStore::load(database, PERSONA, ChatLimits::default())
                .await
                .expect("数据库会话应可恢复");
        let persisted = restored.snapshot(0);
        assert_eq!(restored.messages().len(), 2);
        assert_eq!(persisted.messages[0].content(), "你好");
        assert_eq!(persisted.messages[1].content(), "你好，有什么可以帮你？");
        assert_eq!(restored_store.latest_revision(), 0);
    });
}

#[test]
fn lower_or_equal_revisions_do_not_overwrite_latest_snapshot() {
    run_async(async {
        let database = Database::open_memory().await.expect("内存数据库应可打开");
        let store = ChatSessionStore::empty(database.clone());
        let mut session = ChatSession::default();
        let started = session.start_turn("new").expect("测试轮次应可开始");
        session
            .append_response(started.response_id, "answer")
            .expect("测试回复应可写入");
        assert!(session.finish_response(started.response_id));

        store
            .save(session.snapshot(4))
            .await
            .expect("较新 revision 应保存");
        store
            .save(session.snapshot(3))
            .await
            .expect("迟到 revision 应被无害丢弃");
        store
            .save(session.snapshot(4))
            .await
            .expect("重复 revision 应被无害丢弃");

        assert_eq!(store.latest_revision(), 4);
        let (restored, restored_store) =
            ChatSessionStore::load(database, PERSONA, ChatLimits::default())
                .await
                .expect("数据库会话应可恢复");
        assert_eq!(restored.messages()[1].content(), "answer");
        assert_eq!(restored_store.latest_revision(), 0);
    });
}

#[test]
fn failed_newer_attempt_blocks_an_older_snapshot_but_allows_retry() {
    run_async(async {
        let store = ChatSessionStore::unavailable();

        assert!(
            store
                .save(ChatSession::default().snapshot(2))
                .await
                .is_err()
        );
        assert!(store.save(ChatSession::default().snapshot(1)).await.is_ok());
        assert!(
            store
                .save(ChatSession::default().snapshot(2))
                .await
                .is_err()
        );
        assert_eq!(store.latest_revision(), 0);
    });
}

#[test]
fn persisted_revision_does_not_block_writes_after_restart() {
    run_async(async {
        let database = Database::open_memory().await.expect("内存数据库应可打开");
        let store = ChatSessionStore::empty(database.clone());
        store
            .save(ChatSession::default().snapshot(u64::MAX))
            .await
            .expect("当前进程应可保存最大 revision");

        let (_, restarted_store) = ChatSessionStore::load(database, PERSONA, ChatLimits::default())
            .await
            .expect("数据库会话应可重新加载");
        restarted_store
            .save(ChatSession::default().snapshot(1))
            .await
            .expect("重启后的首份快照不应被持久化 revision 阻止");
        assert_eq!(restarted_store.latest_revision(), 1);
    });
}

#[test]
fn corrupt_database_session_returns_error() {
    run_async(async {
        let database = Database::open_memory().await.expect("内存数据库应可打开");
        database
            .write_document("agent", SESSION_KEY, 1, b"not valid json")
            .await
            .expect("损坏数据库测试文档应可写入");

        let result = ChatSessionStore::load(database, PERSONA, ChatLimits::default()).await;
        assert!(matches!(result, Err(ChatStoreError::Format(_))));
    });
}

#[test]
fn unsupported_database_document_is_not_replaced_during_restore() {
    run_async(async {
        let database = Database::open_memory().await.expect("内存数据库应可打开");
        database
            .write_document("agent", SESSION_KEY, 2, b"future format")
            .await
            .expect("未来格式测试文档应可写入");

        let result = ChatSessionStore::load(database.clone(), PERSONA, ChatLimits::default()).await;
        assert!(matches!(
            result,
            Err(ChatStoreError::UnsupportedDocumentVersion(2))
        ));
        let document = database
            .read_document("agent", SESSION_KEY)
            .await
            .expect("未来格式文档应可重新读取")
            .expect("恢复失败不得删除未来格式文档");
        assert_eq!(document.format_version(), 2);
        assert_eq!(document.contents(), b"future format");
    });
}

#[test]
fn oversized_session_returns_error() {
    run_async(async {
        let database = Database::open_memory().await.expect("内存数据库应可打开");
        let oversized = vec![b' '; MAX_SESSION_BYTES + 1];
        database
            .write_document("agent", SESSION_KEY, 1, &oversized)
            .await
            .expect("超限会话测试文档应可写入数据库");

        let result = ChatSessionStore::load(database, PERSONA, ChatLimits::default()).await;
        assert!(matches!(result, Err(ChatStoreError::TooLarge)));
    });
}

#[test]
fn unavailable_store_reports_save_failure() {
    run_async(async {
        let store = ChatSessionStore::unavailable();
        let result = store.save(ChatSession::default().snapshot(1)).await;

        assert!(result.is_err());
        assert_eq!(store.latest_revision(), 0);
    });
}

#[test]
fn availability_reflects_whether_a_database_was_opened() {
    run_async(async {
        let database = Database::open_memory().await.expect("内存数据库应可打开");

        assert!(ChatSessionStore::empty(database.clone()).is_available());
        assert!(!ChatSessionStore::unavailable().is_available());

        let (_, store) = ChatSessionStore::load(database, PERSONA, ChatLimits::default())
            .await
            .expect("空数据库应可加载");
        assert!(store.is_available());
    });
}

#[test]
fn store_errors_describe_the_failure_without_including_session_text() {
    let too_large = ChatStoreError::TooLarge;
    assert!(
        too_large
            .to_string()
            .contains(&MAX_SESSION_BYTES.to_string())
    );
    assert!(std::error::Error::source(&too_large).is_none());

    let unsupported = ChatStoreError::UnsupportedDocumentVersion(9);
    assert!(unsupported.to_string().contains('9'));
    assert!(std::error::Error::source(&unsupported).is_none());

    let unavailable = ChatStoreError::Unavailable;
    assert!(unavailable.to_string().contains("数据库"));
    assert!(std::error::Error::source(&unavailable).is_none());

    let format = ChatStoreError::Format(
        serde_json::from_str::<serde_json::Value>("not json").expect_err("测试需要解析错误"),
    );
    assert!(format.to_string().starts_with("聊天会话无法解析"));
    assert!(std::error::Error::source(&format).is_some());
}

#[test]
fn deleting_a_persona_session_removes_only_that_personas_document() {
    run_async(async {
        let database = Database::open_memory().await.expect("内存数据库应可打开");
        let mut session = ChatSession::default();
        let started = session.start_turn("记住我").expect("测试轮次应可开始");
        session
            .append_response(started.response_id, "好")
            .expect("测试回复应可写入");
        assert!(session.finish_response(started.response_id));

        for persona in ["default", "other"] {
            let (_, store) =
                ChatSessionStore::load(database.clone(), persona, ChatLimits::default())
                    .await
                    .expect("空数据库应可加载");
            store
                .save(session.snapshot(1))
                .await
                .expect("会话应保存到数据库");
        }

        delete_persona_session(&database, "default")
            .await
            .expect("删除人格会话应成功");

        let (cleared, _) =
            ChatSessionStore::load(database.clone(), "default", ChatLimits::default())
                .await
                .expect("删除后应降级为空会话");
        assert_eq!(cleared.messages().len(), 0);
        // 记忆按人格隔离，删除一个人格不得影响其他人格的上下文。
        let (kept, _) = ChatSessionStore::load(database, "other", ChatLimits::default())
            .await
            .expect("另一人格的会话应仍可恢复");
        assert_eq!(kept.messages().len(), 2);
    });
}

#[test]
fn lowering_the_context_limit_drops_history_instead_of_failing_to_load() {
    run_async(async {
        let database = Database::open_memory().await.expect("内存数据库应可打开");
        let mut session = ChatSession::default();
        for index in 0..3 {
            let started = session
                .start_turn(format!("问题 {index}"))
                .expect("测试轮次应可开始");
            session
                .append_response(started.response_id, "一个比较长的回答内容")
                .expect("测试回复应可写入");
            assert!(session.finish_response(started.response_id));
        }
        let store = ChatSessionStore::empty(database.clone());
        store
            .save(session.snapshot(1))
            .await
            .expect("会话应保存到数据库");

        // 用户调小上限后旧快照仍必须可用：只丢弃装不下的历史轮次。
        let (restored, _) = ChatSessionStore::load(
            database,
            PERSONA,
            ChatLimits {
                max_messages: 2,
                max_bytes: 64,
            },
        )
        .await
        .expect("调小上限后仍应成功恢复");
        assert_eq!(restored.messages().len(), 2);
        assert_eq!(restored.messages()[0].content(), "问题 2");
    });
}

#[test]
fn stored_context_usage_reports_zero_for_a_persona_without_history() {
    run_async(async {
        let database = Database::open_memory().await.expect("内存数据库应可打开");
        let usage = super::super::store::persona_context_usage(&database, "fresh")
            .await
            .expect("没有记录时应返回零占用");

        assert_eq!(usage, (0, 0));
    });
}
