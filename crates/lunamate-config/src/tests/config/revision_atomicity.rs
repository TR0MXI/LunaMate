//! 验证 revision 竞争、原子替换与失败时的发布隔离。

use std::{
    fs,
    sync::{Arc, Barrier, mpsc},
    thread,
    time::Duration,
};

use toml_edit::DocumentMut;

use super::{BOUND_AGENT_CONFIG, TestDirectory};
use crate::config::*;

#[test]
fn atomic_replacement_leaves_a_recoverable_complete_document() {
    let directory = TestDirectory::new();
    directory.write(
        r#"[render]
frame_rate = 30

[custom]
value = "preserved"
"#,
    );
    let config = LunaConfig::load_from(directory.config_path());

    config
        .set_frame_rate(FrameRate::Fps120)
        .expect("配置文件应当可以原子替换");

    let saved = fs::read_to_string(directory.config_path()).expect("替换后的配置应当可以读取");
    saved
        .parse::<DocumentMut>()
        .expect("替换后的配置必须是完整 TOML");
    assert!(saved.contains("value = \"preserved\""));
    assert_eq!(
        LunaConfig::load_from(directory.config_path()).frame_rate(),
        FrameRate::Fps120
    );
    assert!(directory.temporary_paths().is_empty());
}

#[test]
fn failed_write_does_not_publish_runtime_value() {
    let directory = TestDirectory::new();
    let config_path = directory.config_path();
    let config = LunaConfig::load_from(config_path.clone());
    fs::create_dir(&config_path).expect("冲突目标目录应当可以创建");

    let revision = config.reserve_frame_rate_revision();
    let result = config.set_frame_rate_at_revision(FrameRate::Fps120, revision);

    assert!(matches!(result, Err(ConfigWriteError::Io { .. })));
    assert_eq!(config.frame_rate(), FrameRate::Fps30);
    assert!(config_path.is_dir());
}

#[test]
fn rename_failure_at_visibility_point_does_not_publish_runtime_value() {
    let directory = TestDirectory::new();
    let config_path = directory.config_path();
    let config = Arc::new(LunaConfig::load_from(config_path.clone()));
    let revision = config.reserve_frame_rate_revision();
    let barrier = Arc::new(Barrier::new(2));
    config.set_prepare_commit_barrier_for_test(Arc::clone(&barrier));
    let writer_config = Arc::clone(&config);
    let writer = thread::spawn(move || {
        writer_config.set_frame_rate_at_revision(FrameRate::Fps120, revision)
    });

    barrier.wait();
    let create_conflict = fs::create_dir(&config_path);
    barrier.wait();

    create_conflict.expect("rename 冲突目标目录应当可以创建");
    let error = writer
        .join()
        .expect("配置写入线程不应 panic")
        .expect_err("目标变为目录后 rename 应当失败");
    assert!(matches!(
        error,
        ConfigWriteError::Io {
            operation: "提交配置文件",
            ..
        }
    ));
    assert_eq!(config.frame_rate(), FrameRate::Fps30);
    assert!(config_path.is_dir());
    assert!(directory.temporary_paths().is_empty());
}

#[test]
fn blocked_parent_sync_does_not_delay_a_new_revision_reservation() {
    let directory = TestDirectory::new();
    let config = Arc::new(LunaConfig::load_from(directory.config_path()));
    let committed_revision = config.reserve_frame_rate_revision();
    let sync_barrier = Arc::new(Barrier::new(2));
    config.set_parent_sync_barrier_for_test(Arc::clone(&sync_barrier));
    let writer_config = Arc::clone(&config);
    let writer = thread::spawn(move || {
        writer_config.set_frame_rate_at_revision(FrameRate::Fps120, committed_revision)
    });

    sync_barrier.wait();
    let published_while_blocked = config.frame_rate();
    let persisted_while_blocked = LunaConfig::load_from(directory.config_path()).frame_rate();

    let (reserved_sender, reserved_receiver) = mpsc::channel();
    let reserving_config = Arc::clone(&config);
    let reserver = thread::spawn(move || {
        let revision = reserving_config.reserve_frame_rate_revision();
        reserved_sender
            .send(revision)
            .expect("revision 接收端在测试期间必须存在");
    });
    let reserved = reserved_receiver.recv_timeout(Duration::from_secs(2));

    sync_barrier.wait();
    assert_eq!(
        writer
            .join()
            .expect("配置写入线程不应 panic")
            .expect("目录同步应当成功"),
        Some(())
    );
    reserver.join().expect("revision 分配线程不应 panic");
    let reserved = reserved.expect("父目录同步阻塞期间 revision 分配不应等待");
    assert_eq!(published_while_blocked, FrameRate::Fps120);
    assert_eq!(persisted_while_blocked, FrameRate::Fps120);
    assert!(reserved > committed_revision);
}

#[cfg(unix)]
#[test]
fn parent_sync_failure_after_visible_commit_remains_a_published_success() {
    let directory = TestDirectory::new();
    let config = LunaConfig::load_from(directory.config_path());
    config.fail_next_parent_sync_for_test();
    let revision = config.reserve_frame_rate_revision();

    assert_eq!(
        config
            .set_frame_rate_at_revision(FrameRate::Fps120, revision)
            .expect("已可见提交的父目录同步失败应降级为成功"),
        Some(())
    );
    assert_eq!(config.frame_rate(), FrameRate::Fps120);
    assert_eq!(
        LunaConfig::load_from(directory.config_path()).frame_rate(),
        FrameRate::Fps120
    );
    assert!(directory.temporary_paths().is_empty());
}

#[test]
fn stale_llm_write_cannot_replace_newer_selection() {
    let directory = TestDirectory::new();
    let config = LunaConfig::load_from(directory.config_path());
    let local = LlmSettings {
        models: vec![LlmModelConfig {
            id: "local".to_owned(),
            label: "本地模型".to_owned(),
            kind: ModelKind::ChatCompletions,
            provider: ModelProvider::Genai(LlmProvider::Ollama),
            model: "qwen3:8b".to_owned(),
            endpoint: Some("http://localhost:11434".to_owned()),
            api_key: None,
            voice: None,
            voice_type: None,
            local_path: None,
            use_gpu: false,
            whisper_language: None,
            advanced: LlmAdvancedOptions::default(),
        }],
        selected_model: Some("local".to_owned()),
        selected_transcription_model: None,
    };
    let cloud = LlmSettings {
        models: vec![LlmModelConfig {
            id: "cloud".to_owned(),
            label: "云端模型".to_owned(),
            kind: ModelKind::ChatCompletions,
            provider: ModelProvider::Genai(LlmProvider::OpenAI),
            model: "gpt-5-mini".to_owned(),
            endpoint: None,
            api_key: Some("test-token".to_owned()),
            voice: None,
            voice_type: None,
            local_path: None,
            use_gpu: false,
            whisper_language: None,
            advanced: LlmAdvancedOptions::default(),
        }],
        selected_model: Some("cloud".to_owned()),
        selected_transcription_model: None,
    };
    let old_revision = config.reserve_llm_settings_revision();
    let new_revision = config.reserve_llm_settings_revision();

    assert!(
        config
            .set_llm_settings_at_revision(cloud, new_revision, AppLanguage::SimplifiedChinese,)
            .expect("新配置应当可以保存")
            .is_some()
    );
    assert!(
        config
            .set_llm_settings_at_revision(local, old_revision, AppLanguage::SimplifiedChinese,)
            .expect("迟到配置应当被无害丢弃")
            .is_none()
    );
    assert_eq!(
        config
            .llm_settings()
            .selected()
            .map(|model| model.id.as_str()),
        Some("cloud")
    );
}

#[test]
fn stale_frame_rate_write_cannot_replace_newer_value() {
    let directory = TestDirectory::new();
    let config = LunaConfig::load_from(directory.config_path());
    let old_revision = config.reserve_frame_rate_revision();
    let new_revision = config.reserve_frame_rate_revision();

    assert_eq!(
        config
            .set_frame_rate_at_revision(FrameRate::Fps120, new_revision)
            .expect("新帧率应当可以保存"),
        Some(())
    );
    assert_eq!(
        config
            .set_frame_rate_at_revision(FrameRate::Fps30, old_revision)
            .expect("旧帧率应当被无害丢弃"),
        None
    );
    assert_eq!(config.frame_rate(), FrameRate::Fps120);
    assert_eq!(
        LunaConfig::load_from(directory.config_path()).frame_rate(),
        FrameRate::Fps120
    );
}

#[test]
fn stale_scalar_prepare_cannot_reach_disk_when_the_newer_write_fails() {
    let directory = TestDirectory::new();
    directory.write("[render]\nframe_rate = 60\n");
    let published_file = fs::read(directory.config_path()).expect("已发布配置应当可读");
    let config = Arc::new(LunaConfig::load_from(directory.config_path()));
    let stale_revision = config.reserve_frame_rate_revision();
    let barrier = Arc::new(Barrier::new(2));
    config.set_prepare_commit_barrier_for_test(Arc::clone(&barrier));
    let stale_config = Arc::clone(&config);
    let stale_write = thread::spawn(move || {
        stale_config.set_frame_rate_at_revision(FrameRate::Fps120, stale_revision)
    });

    barrier.wait();
    let failed_revision = config.reserve_frame_rate_revision();
    config.fail_next_prepare_for_test();
    assert_eq!(
        fs::read(directory.config_path()).expect("旧任务暂停时配置应当可读"),
        published_file
    );
    barrier.wait();

    assert_eq!(
        stale_write
            .join()
            .expect("旧标量写入线程不应 panic")
            .expect("过期标量写入应被无害丢弃"),
        None
    );
    assert!(matches!(
        config.set_frame_rate_at_revision(FrameRate::Fps30, failed_revision),
        Err(ConfigWriteError::Io { .. })
    ));
    assert_eq!(config.frame_rate(), FrameRate::Fps60);
    assert_eq!(
        fs::read(directory.config_path()).expect("失败后已发布配置应当可读"),
        published_file
    );
    assert_eq!(
        LunaConfig::load_from(directory.config_path()).frame_rate(),
        FrameRate::Fps60
    );
    assert!(directory.temporary_paths().is_empty());
}

#[test]
fn stale_llm_prepare_cannot_reach_disk_when_the_newer_write_fails() {
    let directory = TestDirectory::new();
    directory.write(BOUND_AGENT_CONFIG);
    let published_file = fs::read(directory.config_path()).expect("已发布配置应当可读");
    let config = Arc::new(LunaConfig::load_from(directory.config_path()));
    let published_settings = config.llm_settings();
    let published_generation = config.agent_config_snapshot().generation();
    let mut stale_draft = published_settings.as_ref().clone();
    stale_draft
        .models
        .iter_mut()
        .find(|model| model.id == "chat")
        .expect("测试聊天模型必须存在")
        .label = "过期草稿".to_owned();
    let stale_revision = config.reserve_llm_settings_revision();
    let barrier = Arc::new(Barrier::new(2));
    config.set_prepare_commit_barrier_for_test(Arc::clone(&barrier));
    let stale_config = Arc::clone(&config);
    let stale_write = thread::spawn(move || {
        stale_config.set_llm_settings_at_revision(
            stale_draft,
            stale_revision,
            AppLanguage::SimplifiedChinese,
        )
    });

    barrier.wait();
    let failed_revision = config.reserve_llm_settings_revision();
    config.fail_next_prepare_for_test();
    assert_eq!(
        fs::read(directory.config_path()).expect("旧任务暂停时配置应当可读"),
        published_file
    );
    barrier.wait();

    assert!(
        stale_write
            .join()
            .expect("旧 LLM 写入线程不应 panic")
            .expect("过期 LLM 写入应被无害丢弃")
            .is_none()
    );
    let mut failed_draft = published_settings.as_ref().clone();
    failed_draft
        .models
        .iter_mut()
        .find(|model| model.id == "chat")
        .expect("测试聊天模型必须存在")
        .label = "更新草稿".to_owned();
    assert!(matches!(
        config.set_llm_settings_at_revision(
            failed_draft,
            failed_revision,
            AppLanguage::SimplifiedChinese,
        ),
        Err(ConfigWriteError::Io { .. })
    ));
    assert_eq!(config.llm_settings().as_ref(), published_settings.as_ref());
    assert_eq!(
        config.agent_config_snapshot().generation(),
        published_generation
    );
    assert_eq!(
        fs::read(directory.config_path()).expect("失败后已发布配置应当可读"),
        published_file
    );
    assert_eq!(
        LunaConfig::load_from(directory.config_path())
            .llm_settings()
            .as_ref(),
        published_settings.as_ref()
    );
    assert!(directory.temporary_paths().is_empty());
}

#[test]
fn stale_screenshot_enable_cannot_reach_disk_when_newer_disable_fails() {
    let directory = TestDirectory::new();
    directory.write("[tools]\nallow_agent_screenshot = false\n");
    let published_file = fs::read(directory.config_path()).expect("已发布配置应当可读");
    let config = Arc::new(LunaConfig::load_from(directory.config_path()));
    let stale_revision = config.reserve_allow_agent_screenshot_revision(true);
    let barrier = Arc::new(Barrier::new(2));
    config.set_prepare_commit_barrier_for_test(Arc::clone(&barrier));
    let stale_config = Arc::clone(&config);
    let stale_write = thread::spawn(move || {
        stale_config.set_allow_agent_screenshot_at_revision(true, stale_revision)
    });

    barrier.wait();
    let failed_revision = config.reserve_allow_agent_screenshot_revision(false);
    config.fail_next_prepare_for_test();
    assert_eq!(
        fs::read(directory.config_path()).expect("旧授权任务暂停时配置应当可读"),
        published_file
    );
    barrier.wait();

    assert_eq!(
        stale_write
            .join()
            .expect("旧截屏授权线程不应 panic")
            .expect("过期截屏授权应被无害丢弃"),
        None
    );
    assert!(matches!(
        config.set_allow_agent_screenshot_at_revision(false, failed_revision),
        Err(ConfigWriteError::Io { .. })
    ));
    assert!(!config.allow_agent_screenshot());
    assert!(!config.requested_allow_agent_screenshot());
    assert!(config.agent_screenshot_permission_retry_required());
    assert_eq!(
        fs::read(directory.config_path()).expect("失败后已发布授权应当可读"),
        published_file
    );
    assert!(!LunaConfig::load_from(directory.config_path()).allow_agent_screenshot());
    assert!(directory.temporary_paths().is_empty());
}
