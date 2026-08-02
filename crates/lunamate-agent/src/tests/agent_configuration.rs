//! 验证 Agent 配置事务、人格切换与运行时 revision 隔离。

use std::sync::Arc;

use crate::{
    Agent, AgentError, AgentMemory, ChatLimits, Client, ModelIden,
    config::AppLanguage,
    persistence::{AgentPersistenceCallbacks, PersistenceError, PersistentMemoryUsage},
};

use super::persistence::TestDatabase;

#[tokio::test]
async fn shutting_down_rejection_does_not_modify_runtime_configuration() {
    let database = TestDatabase::open_memory()
        .await
        .expect("内存数据库应可打开");
    let agent = Agent::new(
        Client::default(),
        None,
        None,
        "original",
        AgentMemory::new(Some(database.callbacks())),
        "default",
        ChatLimits::default(),
        AppLanguage::English,
        None,
    );
    agent.shutdown().await.expect("无可用会话存储时关闭应成功");

    let result = agent
        .apply_configuration(
            7,
            Client::default(),
            Some(ModelIden::new(
                crate::config::LlmProvider::Ollama,
                "qwen3:8b",
            )),
            Some(crate::ChatOptions::default().with_temperature(0.4)),
            "changed",
            AgentMemory::unavailable(),
            "other",
            ChatLimits {
                max_messages: 2,
                max_tokens: 512,
                max_request_tokens: 256,
            },
            AppLanguage::Japanese,
        )
        .await;

    assert!(matches!(result, Err(AgentError::ShuttingDown)));
    let runtime = agent.runtime.read();
    assert_eq!(runtime.revision, 1);
    assert_eq!(runtime.configuration_revision, 0);
    assert!(runtime.model.is_none());
    assert!(runtime.options.is_none());
    assert_eq!(runtime.system_prompt.as_ref(), "original");
    assert!(runtime.memory.is_available());
    assert_eq!(runtime.active_persona, "default");
    assert_eq!(runtime.limits, ChatLimits::default());
    assert_eq!(runtime.language, AppLanguage::English);
    drop(runtime);
    let state = agent.state.lock();
    assert_eq!(state.pending_configuration_revision, 0);
    assert!(!state.switching_memory);
}

#[tokio::test]
async fn invalid_limits_do_not_start_a_persona_switch_or_modify_runtime() {
    let agent = Agent::new(
        Client::default(),
        None,
        None,
        "original",
        AgentMemory::unavailable(),
        "default",
        ChatLimits::default(),
        AppLanguage::English,
        None,
    );

    let result = agent
        .apply_configuration(
            2,
            Client::default(),
            Some(ModelIden::new(
                crate::config::LlmProvider::Ollama,
                "qwen3:8b",
            )),
            None,
            "changed",
            AgentMemory::unavailable(),
            "other",
            ChatLimits {
                max_messages: 1,
                ..ChatLimits::default()
            },
            AppLanguage::Japanese,
        )
        .await;

    assert!(matches!(result, Err(AgentError::Session(_))));
    let runtime = agent.runtime.read();
    assert_eq!(runtime.revision, 1);
    assert_eq!(runtime.configuration_revision, 0);
    assert!(runtime.model.is_none());
    assert_eq!(runtime.system_prompt.as_ref(), "original");
    assert_eq!(runtime.active_persona, "default");
    assert_eq!(runtime.limits, ChatLimits::default());
    assert_eq!(runtime.language, AppLanguage::English);
    drop(runtime);
    let state = agent.state.lock();
    assert_eq!(state.pending_configuration_revision, 0);
    assert!(!state.switching_memory);
}

#[tokio::test]
async fn failed_same_persona_validation_keeps_the_active_session_untouched() {
    let agent = Agent::new(
        Client::default(),
        None,
        None,
        "original",
        AgentMemory::unavailable(),
        "default",
        ChatLimits::default(),
        AppLanguage::English,
        None,
    );
    {
        let mut state = agent.state.lock();
        state
            .session
            .start_turn_with_image("still streaming", None, AppLanguage::English)
            .expect("测试会话应能开始活动响应");
    }
    let messages = agent.snapshot().messages().to_vec();

    let result = agent
        .apply_configuration(
            2,
            Client::default(),
            None,
            None,
            "changed",
            AgentMemory::unavailable(),
            "default",
            ChatLimits {
                max_messages: 1,
                ..ChatLimits::default()
            },
            AppLanguage::Japanese,
        )
        .await;

    assert!(matches!(result, Err(AgentError::Session(_))));
    let snapshot = agent.snapshot();
    assert!(snapshot.is_streaming());
    assert_eq!(snapshot.messages(), messages);
    let runtime = agent.runtime.read();
    assert_eq!(runtime.revision, 1);
    assert_eq!(runtime.configuration_revision, 0);
    assert_eq!(runtime.system_prompt.as_ref(), "original");
    assert_eq!(runtime.language, AppLanguage::English);
    drop(runtime);
    let state = agent.state.lock();
    assert_eq!(state.pending_configuration_revision, 0);
    assert!(!state.switching_memory);
}

#[tokio::test]
async fn failed_old_session_save_clears_switching_and_allows_the_same_revision_retry() {
    use std::sync::atomic::{AtomicUsize, Ordering};

    let save_attempts = Arc::new(AtomicUsize::new(0));
    let attempts = save_attempts.clone();
    let memory = AgentMemory::new(Some(AgentPersistenceCallbacks::new(
        |_| async { Ok(None) },
        move |_, _| {
            let attempt = attempts.fetch_add(1, Ordering::AcqRel);
            async move {
                if attempt == 0 {
                    Err(PersistenceError::new("save_failed", "save failed"))
                } else {
                    Ok(())
                }
            }
        },
        |_| async { Ok(()) },
        |_| async { Ok(PersistentMemoryUsage::default()) },
        |_, _| async { Ok(()) },
    )));
    let agent = Agent::load(
        Client::default(),
        None,
        None,
        "original",
        memory.clone(),
        "default",
        ChatLimits::default(),
        AppLanguage::English,
        None,
    )
    .await;

    let first = agent
        .apply_configuration(
            2,
            Client::default(),
            None,
            None,
            "changed",
            memory.clone(),
            "other",
            ChatLimits::default(),
            AppLanguage::Japanese,
        )
        .await;
    assert!(matches!(first, Err(AgentError::Persistence(_))));
    assert!(!agent.snapshot().is_switching_memory());
    assert_eq!(agent.snapshot().active_persona(), "default");
    {
        let runtime = agent.runtime.read();
        assert_eq!(runtime.configuration_revision, 0);
        assert_eq!(runtime.system_prompt.as_ref(), "original");
    }
    assert_eq!(agent.state.lock().pending_configuration_revision, 2);

    assert!(
        agent
            .apply_configuration(
                2,
                Client::default(),
                None,
                None,
                "changed",
                memory,
                "other",
                ChatLimits::default(),
                AppLanguage::Japanese,
            )
            .await
            .expect("同 revision 重试应成功安装")
    );
    assert_eq!(save_attempts.load(Ordering::Acquire), 2);
    assert_eq!(agent.snapshot().active_persona(), "other");
    assert!(!agent.snapshot().is_switching_memory());
    let runtime = agent.runtime.read();
    assert_eq!(runtime.configuration_revision, 2);
    assert_eq!(runtime.system_prompt.as_ref(), "changed");
    assert_eq!(runtime.language, AppLanguage::Japanese);
}

#[tokio::test]
async fn newer_configuration_stays_switching_when_an_older_save_fails() {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use tokio::sync::Notify;

    let save_attempts = Arc::new(AtomicUsize::new(0));
    let old_save_started = Arc::new(Notify::new());
    let release_old_save = Arc::new(Notify::new());
    let newer_save_started = Arc::new(Notify::new());
    let release_newer_save = Arc::new(Notify::new());
    let attempts = save_attempts.clone();
    let old_started = old_save_started.clone();
    let old_release = release_old_save.clone();
    let newer_started = newer_save_started.clone();
    let newer_release = release_newer_save.clone();
    let memory = AgentMemory::new(Some(AgentPersistenceCallbacks::new(
        |_| async { Ok(None) },
        move |_, _| {
            let attempt = attempts.fetch_add(1, Ordering::AcqRel);
            let old_started = old_started.clone();
            let old_release = old_release.clone();
            let newer_started = newer_started.clone();
            let newer_release = newer_release.clone();
            async move {
                if attempt == 0 {
                    old_started.notify_one();
                    old_release.notified().await;
                    Err(PersistenceError::new("old_save_failed", "old save failed"))
                } else {
                    newer_started.notify_one();
                    newer_release.notified().await;
                    Ok(())
                }
            }
        },
        |_| async { Ok(()) },
        |_| async { Ok(PersistentMemoryUsage::default()) },
        |_, _| async { Ok(()) },
    )));
    let agent = Agent::load(
        Client::default(),
        None,
        None,
        "original",
        memory.clone(),
        "default",
        ChatLimits::default(),
        AppLanguage::English,
        None,
    )
    .await;

    let older_agent = agent.clone();
    let older_memory = memory.clone();
    let older = tokio::spawn(async move {
        older_agent
            .apply_configuration(
                2,
                Client::default(),
                None,
                None,
                "older",
                older_memory,
                "older",
                ChatLimits::default(),
                AppLanguage::English,
            )
            .await
    });
    old_save_started.notified().await;

    let newer_agent = agent.clone();
    let newer = tokio::spawn(async move {
        newer_agent
            .apply_configuration(
                3,
                Client::default(),
                None,
                None,
                "newer",
                memory,
                "newer",
                ChatLimits::default(),
                AppLanguage::Japanese,
            )
            .await
    });
    tokio::task::yield_now().await;
    {
        let state = agent.state.lock();
        assert_eq!(state.pending_configuration_revision, 3);
        assert!(state.switching_memory);
    }

    release_old_save.notify_one();
    newer_save_started.notified().await;
    assert!(matches!(
        older.await.expect("旧配置任务不应 panic"),
        Err(AgentError::Persistence(_))
    ));
    {
        let runtime = agent.runtime.read();
        assert_eq!(runtime.configuration_revision, 0);
        assert_eq!(runtime.system_prompt.as_ref(), "original");
        assert_eq!(runtime.active_persona, "default");
        let state = agent.state.lock();
        assert_eq!(state.pending_configuration_revision, 3);
        assert!(state.switching_memory);
    }

    release_newer_save.notify_one();
    assert!(
        newer
            .await
            .expect("更新配置任务不应 panic")
            .expect("更新配置应成功安装")
    );
    assert_eq!(save_attempts.load(Ordering::Acquire), 2);
    assert_eq!(agent.snapshot().active_persona(), "newer");
    assert!(!agent.snapshot().is_switching_memory());
    let runtime = agent.runtime.read();
    assert_eq!(runtime.configuration_revision, 3);
    assert_eq!(runtime.system_prompt.as_ref(), "newer");
    assert_eq!(runtime.language, AppLanguage::Japanese);
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
