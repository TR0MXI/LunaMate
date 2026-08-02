//! 验证 Agent 直接组合运行时组件后的恢复、热更新与最终保存。

use std::sync::Arc;

use crate::{
    Agent, AgentError, AgentInput, AgentMemory, ChatLimits, Client, ModelIden,
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
async fn persistence_from_a_non_tokio_thread_uses_the_agent_runtime() {
    let database = TestDatabase::open_memory()
        .await
        .expect("内存数据库应可打开");
    let agent = load(
        AgentMemory::new(Some(database.callbacks())),
        AppLanguage::English,
        None,
    )
    .await;
    let background_agent = agent.clone();

    std::thread::spawn(move || background_agent.persist(true))
        .join()
        .expect("非 Tokio 线程触发持久化不应 panic");
    tokio::task::yield_now().await;

    let saved = database
        .read_document("agent", "chat-session/default")
        .await
        .expect("后台持久化不应返回数据库错误")
        .expect("后台持久化应写入当前会话");
    assert!(serde_json::from_slice::<serde_json::Value>(saved.contents()).is_ok());
}

#[tokio::test]
async fn voice_barge_in_from_a_non_tokio_thread_persists_the_interrupted_chat() {
    let database = TestDatabase::open_memory()
        .await
        .expect("内存数据库应可打开");
    let agent = load(
        AgentMemory::new(Some(database.callbacks())),
        AppLanguage::English,
        None,
    )
    .await;
    {
        let mut state = agent.state.lock();
        state
            .session
            .start_turn_with_image("in-flight", None, AppLanguage::English)
            .expect("测试应能创建活动回复");
        state.reply_message_id = state.session.messages().back().map(|message| message.id());
    }
    let background_agent = agent.clone();

    std::thread::spawn(move || {
        assert!(background_agent.voice_started(1, AppLanguage::English));
    })
    .join()
    .expect("语音打断不应让非 Tokio 线程 panic");
    tokio::task::yield_now().await;

    let saved = database
        .read_document("agent", "chat-session/default")
        .await
        .expect("语音打断后的后台持久化不应返回数据库错误")
        .expect("语音打断后的会话应写入数据库");
    assert!(serde_json::from_slice::<serde_json::Value>(saved.contents()).is_ok());
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

#[tokio::test]
async fn voice_barge_in_invalidates_a_queued_text_request() {
    let agent = Arc::new(Agent::new(
        Client::default(),
        None,
        None,
        "",
        AgentMemory::unavailable(),
        "default",
        ChatLimits::default(),
        AppLanguage::English,
        None,
    ));
    let queued_revision = agent.request_revision();

    assert!(agent.voice_started(9, AppLanguage::English));
    let result = Agent::send(
        Arc::clone(&agent),
        AgentInput {
            text: "stale text".to_owned(),
            image: None,
            screenshot_capability: None,
            outfits: Vec::new(),
            outfit_revision: 0,
            request_revision: queued_revision,
            language: AppLanguage::English,
        },
    )
    .await;

    assert!(matches!(result, Err(AgentError::StaleInput)));
    assert_eq!(agent.snapshot().pending_voice(), Some(9));
}

#[tokio::test]
async fn suspended_agent_rejects_a_queued_send_without_creating_context() {
    let agent = Agent::new(
        Client::default(),
        Some(ModelIden::new(
            crate::config::LlmProvider::Ollama,
            "qwen3:8b",
        )),
        None,
        "",
        AgentMemory::unavailable(),
        "default",
        ChatLimits::default(),
        AppLanguage::English,
        None,
    );
    let queued_revision = agent.request_revision();
    assert!(!agent.suspend_and_discard_active_turn());

    let suspended = agent
        .clone()
        .send(AgentInput {
            text: "must not enter context".to_owned(),
            image: None,
            screenshot_capability: None,
            outfits: Vec::new(),
            outfit_revision: 0,
            request_revision: agent.request_revision(),
            language: AppLanguage::English,
        })
        .await;
    assert!(matches!(suspended, Err(AgentError::Suspended)));
    agent.resume_after_hidden();

    let stale = agent
        .clone()
        .send(AgentInput {
            text: "queued before hiding".to_owned(),
            image: None,
            screenshot_capability: None,
            outfits: Vec::new(),
            outfit_revision: 0,
            request_revision: queued_revision,
            language: AppLanguage::English,
        })
        .await;
    assert!(matches!(stale, Err(AgentError::StaleInput)));
    assert!(agent.snapshot().messages().is_empty());
}
