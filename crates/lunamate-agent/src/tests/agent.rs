//! 验证 Agent 直接组合运行时组件后的恢复、热更新与最终保存。

use std::sync::Arc;

use crate::{
    Agent, AgentMemory, ChatLimits, ChatOptions, Client, ModelIden,
    config::AppLanguage,
    memory::ContextUsage,
    persistence::{
        AgentPersistenceCallbacks, PersistenceError, PersistentMemoryTier, PersistentMemoryUsage,
    },
};

use super::persistence::TestDatabase;

async fn load(
    memory: AgentMemory,
    language: AppLanguage,
    initial_status: Option<String>,
) -> Arc<Agent> {
    Agent::load(
        Client::default(),
        None,
        None,
        "",
        memory,
        "default",
        ChatLimits::default(),
        language,
        initial_status,
    )
    .await
}

async fn assert_other_persona_memory_is_available(
    database: &Arc<TestDatabase>,
    memory: &AgentMemory,
) {
    database
        .insert_memory_for_test("other", PersistentMemoryTier::Medium, true)
        .await
        .expect("其他人格测试记忆应可写入");
    let usage = memory
        .persona("other")
        .usage(
            memory.live_context_usage(),
            ContextUsage {
                max_messages: 64,
                max_tokens: 1_024,
                ..ContextUsage::default()
            },
        )
        .await
        .expect("其他人格的记忆访问不应受活动会话损坏影响");
    assert_eq!(usage.medium, 1);
}

#[tokio::test]
async fn invalid_current_session_recovers_and_can_be_replaced_on_shutdown() {
    let database = TestDatabase::open_memory()
        .await
        .expect("内存数据库应可打开");
    database
        .write_document("agent", "chat-session/default", 1, b"not valid json")
        .await
        .expect("无效活动会话测试文档应可写入");
    let memory = AgentMemory::new(Some(database.callbacks()));
    let agent = load(memory.clone(), AppLanguage::English, None).await;

    assert!(agent.snapshot().messages().is_empty());
    assert_eq!(
        agent.snapshot().status(),
        Some("The chat session snapshot is invalid")
    );
    assert_other_persona_memory_is_available(&database, &memory).await;

    agent.shutdown().await.expect("恢复出的空会话应当可写");
    let replaced = database
        .read_document("agent", "chat-session/default")
        .await
        .expect("替换后的活动会话文档应可读取")
        .expect("空快照应当写入活动会话文档");
    assert_eq!(replaced.format_version(), 1);
    assert!(serde_json::from_slice::<serde_json::Value>(replaced.contents()).is_ok());
}

#[tokio::test]
async fn unsupported_future_session_is_not_overwritten_on_shutdown() {
    let database = TestDatabase::open_memory()
        .await
        .expect("内存数据库应可打开");
    let contents = b"future document";
    database
        .write_document("agent", "chat-session/default", 2, contents)
        .await
        .expect("未来版本测试文档应可写入");
    let memory = AgentMemory::new(Some(database.callbacks()));
    let agent = load(memory.clone(), AppLanguage::English, None).await;

    assert!(agent.snapshot().messages().is_empty());
    assert_eq!(
        agent.snapshot().status(),
        Some("This chat session snapshot version is unsupported")
    );
    assert_other_persona_memory_is_available(&database, &memory).await;
    agent
        .shutdown()
        .await
        .expect("只读回退的退出保存应当静默跳过");

    let original = database
        .read_document("agent", "chat-session/default")
        .await
        .expect("未来版本文档应可读取")
        .expect("未来版本文档不得被退出保存删除");
    assert_eq!(original.format_version(), 2);
    assert_eq!(original.contents(), contents);
}

#[tokio::test]
async fn host_reported_invalid_document_keeps_memory_available() {
    let callbacks = AgentPersistenceCallbacks::new(
        |_| async {
            Err(PersistenceError::invalid_document(
                "invalid_stored_document",
                "invalid stored document",
            ))
        },
        |_, _| async { Ok(()) },
        |_| async { Ok(()) },
        |_| async { Ok(PersistentMemoryUsage::default()) },
        |_, _| async { Ok(()) },
    );
    let memory = AgentMemory::new(Some(callbacks));
    let agent = load(memory.clone(), AppLanguage::English, None).await;

    assert!(agent.snapshot().messages().is_empty());
    assert!(memory.is_available());
    assert_eq!(
        agent.snapshot().status(),
        Some("The chat session snapshot is invalid")
    );
}

#[tokio::test]
async fn concurrent_loads_keep_their_languages() {
    let callbacks = || {
        AgentPersistenceCallbacks::new(
            |_| async { Err(PersistenceError::new("offline", "offline")) },
            |_, _| async { Ok(()) },
            |_| async { Ok(()) },
            |_| async { Ok(PersistentMemoryUsage::default()) },
            |_, _| async { Ok(()) },
        )
    };
    let (english, japanese) = tokio::join!(
        load(
            AgentMemory::new(Some(callbacks())),
            AppLanguage::English,
            None,
        ),
        load(
            AgentMemory::new(Some(callbacks())),
            AppLanguage::Japanese,
            None,
        ),
    );

    assert_eq!(
        english.snapshot().status(),
        Some("Conversation persistence is unavailable for this run: offline")
    );
    assert_eq!(
        japanese.snapshot().status(),
        Some("今回の実行では会話を保存できません: offline")
    );
}

#[test]
fn genai_client_is_send_and_sync() {
    fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<Client>();
}

#[test]
fn direct_runtime_setters_advance_the_request_snapshot() {
    let agent = Agent::new(
        Client::default(),
        None,
        None,
        "",
        AgentMemory::unavailable(),
        "default",
        ChatLimits::default(),
        AppLanguage::English,
        None,
    );
    let initial = agent.snapshot().runtime_revision();

    agent.set_client(Client::default());
    agent.set_model(Some(ModelIden::new(
        crate::config::LlmProvider::Ollama,
        "qwen3:8b",
    )));
    agent.set_chat_options(Some(ChatOptions::default().with_temperature(0.4)));
    agent.set_system_prompt("updated");

    assert_eq!(agent.snapshot().runtime_revision(), initial + 4);
}

#[tokio::test]
async fn newer_configuration_wins_over_a_late_persona_restore() {
    use tokio::sync::Notify;

    let agent = Agent::new(
        Client::default(),
        None,
        None,
        "",
        AgentMemory::unavailable(),
        "default",
        ChatLimits::default(),
        AppLanguage::English,
        None,
    );
    let slow_started = Arc::new(Notify::new());
    let release_slow = Arc::new(Notify::new());
    let started = slow_started.clone();
    let release = release_slow.clone();
    let slow_memory = AgentMemory::new(Some(AgentPersistenceCallbacks::new(
        move |_| {
            let started = started.clone();
            let release = release.clone();
            async move {
                started.notify_one();
                release.notified().await;
                Ok(None)
            }
        },
        |_, _| async { Ok(()) },
        |_| async { Ok(()) },
        |_| async { Ok(PersistentMemoryUsage::default()) },
        |_, _| async { Ok(()) },
    )));
    let fast_memory = AgentMemory::new(Some(AgentPersistenceCallbacks::new(
        |_| async { Ok(None) },
        |_, _| async { Ok(()) },
        |_| async { Ok(()) },
        |_| async { Ok(PersistentMemoryUsage::default()) },
        |_, _| async { Ok(()) },
    )));

    let slow_agent = agent.clone();
    let slow = tokio::spawn(async move {
        slow_agent
            .apply_configuration(
                2,
                Client::default(),
                None,
                None,
                "slow",
                slow_memory,
                "slow",
                ChatLimits::default(),
                AppLanguage::English,
            )
            .await
    });
    slow_started.notified().await;
    assert!(
        agent
            .apply_configuration(
                3,
                Client::default(),
                None,
                None,
                "fast",
                fast_memory,
                "fast",
                ChatLimits::default(),
                AppLanguage::English,
            )
            .await
            .expect("较新的配置应当可以安装")
    );
    release_slow.notify_waiters();
    assert!(
        !slow
            .await
            .expect("迟到配置任务不应 panic")
            .expect("迟到配置任务应被正常丢弃")
    );
    assert_eq!(agent.snapshot().active_persona(), "fast");
}

#[test]
fn stale_voice_results_do_not_consume_a_newer_utterance() {
    let agent = Agent::new(
        Client::default(),
        None,
        None,
        "",
        AgentMemory::unavailable(),
        "default",
        ChatLimits::default(),
        AppLanguage::Japanese,
        None,
    );

    assert!(agent.voice_started(7, AppLanguage::English));
    assert!(agent.voice_started(8, AppLanguage::Japanese));
    assert_eq!(agent.take_voice_transcript(7), None);
    assert_eq!(agent.snapshot().pending_voice(), Some(8));
    assert_eq!(agent.take_voice_transcript(8), Some(AppLanguage::Japanese));
    assert_eq!(agent.snapshot().pending_voice(), None);
}
