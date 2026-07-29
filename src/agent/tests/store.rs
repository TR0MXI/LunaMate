//! 验证按人格隔离的会话存储：恢复、revision 串行化与不可信文档处理。

use super::super::{
    AgentMemoryAccess, AssistantTrace,
    session::{ChatLimits, ChatSession},
    store::{
        ChatSessionStore, ChatStoreError, MAX_SESSION_BYTES, SessionDocumentCoordinator,
        SessionDocumentLock, delete_persona_session, mutate_persona_session,
        mutate_persona_session_reserved,
    },
};
use crate::{
    config::DEFAULT_PERSONA_ID,
    database::{Database, MemoryTier},
};
use std::{future::Future, sync::Arc, task::Poll, time::Duration};

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
fn tombstoned_persona_cleanup_removes_session_and_all_memory_tiers() {
    run_async(async {
        let database = Database::open_memory().await.expect("内存数据库应可打开");
        let mut session = ChatSession::default();
        let turn = session.start_turn("旧问题").expect("测试消息应有效");
        session
            .append_response(turn.response_id, "旧回答")
            .expect("测试回复应有效");
        assert!(session.finish_response(turn.response_id));
        let (_, store) = ChatSessionStore::load(database.clone(), "deleted", ChatLimits::default())
            .await
            .expect("已删除人格的测试存储应可创建");
        store
            .save(session.snapshot(1))
            .await
            .expect("测试会话应可保存");
        database
            .insert_memory_for_test("deleted", MemoryTier::Medium, true)
            .await
            .expect("测试中期记忆应可写入");
        database
            .insert_memory_for_test("deleted", MemoryTier::Long, true)
            .await
            .expect("测试长期记忆应可写入");

        let memory = AgentMemoryAccess::new(Some(database.clone()));
        assert!(memory.claim_deleted_persona_cleanup("deleted"));
        assert!(
            !memory.claim_deleted_persona_cleanup("deleted"),
            "同一 tombstone 只能有一个清理者"
        );
        memory
            .cleanup_deleted_persona("deleted")
            .await
            .expect("tombstone 清理应成功");
        memory.complete_deleted_persona_cleanup("deleted");
        assert!(!memory.claim_deleted_persona_cleanup("deleted"));

        assert!(
            database
                .read_document("agent", "chat-session/deleted")
                .await
                .expect("测试会话应可查询")
                .is_none()
        );
        assert_eq!(
            database
                .agent_memory_usage("deleted")
                .await
                .expect("测试记忆应可统计"),
            Default::default()
        );
        memory.release_deleted_persona_cleanup("deleted");
        assert!(memory.claim_deleted_persona_cleanup("deleted"));
    });
}

#[test]
fn persona_session_switching_keeps_each_trace_with_its_own_assistant_message() {
    run_async(async {
        let database = Database::open_memory().await.expect("内存数据库应可打开");
        for (persona, reasoning) in [("default", "default trace"), ("other", "other trace")] {
            let mut session = ChatSession::default();
            let started = session.start_turn(persona).expect("测试轮次应可开始");
            session
                .append_response(started.response_id, "answer")
                .expect("测试回复应可写入");
            assert!(
                session
                    .attach_response_trace(
                        started.response_id,
                        AssistantTrace::new(Some(reasoning.to_owned()), Vec::new()),
                    )
                    .expect("助手详情应可附加")
            );
            assert!(session.finish_response(started.response_id));
            let (_, store) =
                ChatSessionStore::load(database.clone(), persona, ChatLimits::default())
                    .await
                    .expect("人格会话应可加载");
            store
                .save(session.snapshot(1))
                .await
                .expect("人格会话应可保存");
        }

        for (persona, reasoning) in [("default", "default trace"), ("other", "other trace")] {
            let (restored, _) =
                ChatSessionStore::load(database.clone(), persona, ChatLimits::default())
                    .await
                    .expect("人格会话应可恢复");
            assert_eq!(restored.messages()[0].content(), persona);
            assert_eq!(
                restored.messages()[1]
                    .trace()
                    .and_then(AssistantTrace::reasoning),
                Some(reasoning)
            );
        }
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
                max_tokens: 64,
                max_request_tokens: 64,
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
        let limits = ChatSession::default().usage();
        let lock: SessionDocumentLock = Arc::new(SessionDocumentCoordinator::new());
        let usage = super::super::store::persona_context_usage(&database, &lock, "fresh", limits)
            .await
            .expect("没有记录时应返回零占用");

        assert_eq!(usage.messages, 0);
        assert_eq!(usage.tokens, 0);
        assert_eq!(usage.max_tokens, limits.max_tokens);
    });
}

#[test]
fn concurrent_non_active_edits_are_serialized_without_losing_each_other() {
    run_async(async {
        let database = Database::open_memory().await.expect("内存数据库应可打开");
        let lock: SessionDocumentLock = Arc::new(SessionDocumentCoordinator::new());
        let limits = ChatLimits::default();
        let mut session = ChatSession::default();
        let started = session.start_turn("question").expect("测试轮次应可开始");
        session
            .append_response(started.response_id, "answer")
            .expect("测试回复应可写入");
        assert!(session.finish_response(started.response_id));
        let user_id = session.messages()[0].id();
        let assistant_id = session.messages()[1].id();
        let (_, store) =
            ChatSessionStore::load_with_lock(database.clone(), PERSONA, limits, lock.clone())
                .await
                .expect("空会话应可加载");
        store
            .save(session.snapshot(1))
            .await
            .expect("初始会话应可保存");

        let first = mutate_persona_session(&database, &lock, PERSONA, limits, |session| {
            session.edit_message(user_id, "edited question")?;
            Ok(true)
        });
        let second = mutate_persona_session(&database, &lock, PERSONA, limits, |session| {
            session.edit_message(assistant_id, "edited answer")?;
            Ok(true)
        });
        let (first, second) = futures::future::join(first, second).await;
        assert!(first.expect("首条编辑应成功"));
        assert!(second.expect("第二条编辑应成功"));

        let (restored, _) = ChatSessionStore::load_with_lock(database, PERSONA, limits, lock)
            .await
            .expect("编辑后的会话应可恢复");
        assert_eq!(restored.messages()[0].id(), user_id);
        assert_eq!(restored.messages()[0].content(), "edited question");
        assert_eq!(restored.messages()[1].id(), assistant_id);
        assert_eq!(restored.messages()[1].content(), "edited answer");
    });
}

#[test]
fn non_active_reordering_is_persisted_with_stable_message_ids() {
    run_async(async {
        let database = Database::open_memory().await.expect("内存数据库应可打开");
        let lock: SessionDocumentLock = Arc::new(SessionDocumentCoordinator::new());
        let limits = ChatLimits::default();
        let mut session = ChatSession::default();
        let first = session.start_turn("question 1").expect("第一轮应可开始");
        session
            .append_response(first.response_id, "answer 1")
            .expect("第一轮回复应可写入");
        assert!(session.finish_response(first.response_id));
        let second = session.start_turn("question 2").expect("第二轮应可开始");
        session
            .append_response(second.response_id, "answer 2")
            .expect("第二轮回复应可写入");
        assert!(session.finish_response(second.response_id));
        let ids = session
            .messages()
            .iter()
            .map(|message| message.id())
            .collect::<Vec<_>>();
        let reordered = vec![ids[2], ids[3], ids[0], ids[1]];
        let (_, store) =
            ChatSessionStore::load_with_lock(database.clone(), PERSONA, limits, lock.clone())
                .await
                .expect("空会话应可加载");
        store
            .save(session.snapshot(1))
            .await
            .expect("初始会话应可保存");

        let changed = mutate_persona_session(&database, &lock, PERSONA, limits, |session| {
            session.reorder_messages(&reordered)
        })
        .await
        .expect("非活动人格排序应可保存");
        assert!(changed);

        let (restored, _) = ChatSessionStore::load_with_lock(database, PERSONA, limits, lock)
            .await
            .expect("排序后的会话应可恢复");
        assert_eq!(
            restored
                .messages()
                .iter()
                .map(|message| message.id())
                .collect::<Vec<_>>(),
            reordered
        );
    });
}

#[test]
fn reserved_operations_run_in_order_even_when_the_newer_future_is_polled_first() {
    run_async(async {
        let database = Database::open_memory().await.expect("内存数据库应可打开");
        let lock: SessionDocumentLock = Arc::new(SessionDocumentCoordinator::new());
        let limits = ChatLimits::default();
        let mut session = ChatSession::default();
        let started = session.start_turn("question").expect("测试轮次应可开始");
        session
            .append_response(started.response_id, "answer")
            .expect("测试回复应可写入");
        assert!(session.finish_response(started.response_id));
        let message_id = session.messages()[0].id();
        let (_, store) =
            ChatSessionStore::load_with_lock(database.clone(), PERSONA, limits, lock.clone())
                .await
                .expect("空会话应可加载");
        store
            .save(session.snapshot(1))
            .await
            .expect("初始会话应可保存");
        let older_snapshot = session.snapshot(2);
        let older_operation = store.reserve_document_operation();
        let edit_operation = lock.reserve();

        let edit = mutate_persona_session_reserved(
            &database,
            PERSONA,
            limits,
            edit_operation,
            |session| {
                session.edit_message(message_id, "edited")?;
                Ok(true)
            },
        );
        let older_save = store.save_reserved(older_snapshot, older_operation);
        // 先轮询较新的 future；协调器仍必须等待旧保存完成，再应用编辑。
        let (edit, older_save) = futures::future::join(edit, older_save).await;
        assert!(edit.expect("较新的编辑应成功"));
        older_save.expect("较旧快照应先按序保存");

        let (restored, _) = ChatSessionStore::load_with_lock(database, PERSONA, limits, lock)
            .await
            .expect("编辑后的会话应可恢复");
        assert_eq!(restored.messages()[0].content(), "edited");
    });
}

#[test]
fn cancelling_a_reserved_waiter_does_not_block_later_operations() {
    run_async(async {
        let database = Database::open_memory().await.expect("内存数据库应可打开");
        let lock: SessionDocumentLock = Arc::new(SessionDocumentCoordinator::new());
        let (_, store) =
            ChatSessionStore::load_with_lock(database, PERSONA, ChatLimits::default(), lock)
                .await
                .expect("空会话应可加载");
        let first = store.reserve_document_operation();
        let cancelled = store.reserve_document_operation();
        let later = store.reserve_document_operation();

        let mut cancelled_save =
            Box::pin(store.save_reserved(ChatSession::default().snapshot(2), cancelled));
        assert!(matches!(
            futures::poll!(cancelled_save.as_mut()),
            Poll::Pending
        ));
        drop(cancelled_save);

        store
            .save_reserved(ChatSession::default().snapshot(1), first)
            .await
            .expect("首个预留保存应完成");
        tokio::time::timeout(
            Duration::from_secs(1),
            store.save_reserved(ChatSession::default().snapshot(3), later),
        )
        .await
        .expect("取消等待任务后，后续票号不得永久阻塞")
        .expect("后续保存应成功");
        assert_eq!(store.latest_revision(), 3);
    });
}
