//! 验证 Agent 配置跨域事务、快照发布与文档往返。

use std::fs;

use super::{BOUND_AGENT_CONFIG, TestDirectory};
use crate::config::*;

#[test]
fn agent_config_generation_and_domains_publish_as_one_snapshot() {
    let directory = TestDirectory::new();
    let config = LunaConfig::load_from(directory.config_path());
    let initial = config.agent_config_snapshot();
    let model = LlmModelConfig {
        id: "local".to_owned(),
        label: "Local".to_owned(),
        kind: ModelKind::ChatCompletions,
        provider: ModelProvider::Genai(LlmProvider::Ollama),
        model: "qwen3:8b".to_owned(),
        endpoint: Some("http://localhost:11434/".to_owned()),
        api_key: None,
        voice: None,
        voice_type: None,
        local_path: None,
        use_gpu: false,
        whisper_language: None,
        advanced: LlmAdvancedOptions::default(),
    };
    let llm_revision = config.reserve_llm_settings_revision();
    config
        .set_llm_settings_at_revision(
            LlmSettings {
                models: vec![model],
                selected_model: Some("local".to_owned()),
                selected_transcription_model: None,
            },
            llm_revision,
            AppLanguage::SimplifiedChinese,
        )
        .expect("测试 Provider 配置应当可以保存")
        .expect("最新 Provider 配置应当发布");
    let with_model = config.agent_config_snapshot();
    assert!(with_model.generation() > initial.generation());
    assert_eq!(
        with_model
            .settings()
            .selected()
            .map(|model| model.id.as_str()),
        Some("local")
    );
    assert_eq!(
        with_model
            .personas()
            .active()
            .map(|persona| persona.id.as_str()),
        Some(DEFAULT_PERSONA_ID)
    );

    let persona_revision = config.reserve_persona_settings_revision();
    config
        .set_persona_settings_at_revision(
            PersonaSettings {
                personas: vec![PersonaConfig::new("other", "Other")],
                selected: Some("other".to_owned()),
                pending_deletions: Vec::new(),
            },
            persona_revision,
            AppLanguage::SimplifiedChinese,
        )
        .expect("测试人格配置应当可以保存")
        .expect("最新人格配置应当发布");
    let with_persona = config.agent_config_snapshot();
    assert!(with_persona.generation() > with_model.generation());
    assert_eq!(
        with_persona
            .settings()
            .selected()
            .map(|model| model.id.as_str()),
        Some("local")
    );
    assert_eq!(
        with_persona
            .personas()
            .active()
            .map(|persona| persona.id.as_str()),
        Some("other")
    );

    let mut appearance = config.appearance().as_ref().clone();
    appearance.language = AppLanguage::Japanese;
    let appearance_revision = config.reserve_appearance_revision();
    config
        .set_appearance_at_revision(appearance, appearance_revision)
        .expect("测试语言配置应当可以保存")
        .expect("最新语言配置应当发布");
    let japanese = config.agent_config_snapshot();
    assert!(japanese.generation() > with_persona.generation());
    assert_eq!(japanese.language(), AppLanguage::Japanese);
    assert_eq!(
        japanese
            .settings()
            .selected()
            .map(|model| model.id.as_str()),
        Some("local")
    );
    assert_eq!(
        japanese
            .personas()
            .active()
            .map(|persona| persona.id.as_str()),
        Some("other")
    );
}

#[test]
fn deleting_a_persona_bound_model_is_rejected_without_any_publication_or_write() {
    let directory = TestDirectory::new();
    directory.write(BOUND_AGENT_CONFIG);
    let config = LunaConfig::load_from(directory.config_path());
    let original_file = fs::read(directory.config_path()).expect("测试配置原文应当可读");
    let original_llm = config.llm_settings();
    let original_personas = config.persona_settings();
    let original_snapshot = config.agent_config_snapshot();

    for removed in ["chat", "voice"] {
        let mut draft = original_llm.as_ref().clone();
        draft.models.retain(|model| model.id != removed);
        if draft.selected_model.as_deref() == Some(removed) {
            draft.selected_model = None;
        }
        let revision = config.reserve_llm_settings_revision();

        let error = config
            .set_llm_settings_at_revision(draft, revision, AppLanguage::SimplifiedChinese)
            .expect_err("删除人格绑定的模型必须被跨域校验拒绝");

        assert!(matches!(error, ConfigWriteError::InvalidValue(_)));
        assert_eq!(
            fs::read(directory.config_path()).expect("失败后配置原文应当可读"),
            original_file
        );
        assert_eq!(config.llm_settings().as_ref(), original_llm.as_ref());
        assert_eq!(
            config.persona_settings().as_ref(),
            original_personas.as_ref()
        );
        let snapshot = config.agent_config_snapshot();
        assert_eq!(snapshot.generation(), original_snapshot.generation());
        assert_eq!(snapshot.settings().as_ref(), original_llm.as_ref());
        assert_eq!(snapshot.personas().as_ref(), original_personas.as_ref());
    }
}

#[test]
fn saving_a_dangling_persona_binding_is_rejected_transactionally() {
    let directory = TestDirectory::new();
    directory.write(BOUND_AGENT_CONFIG);
    let config = LunaConfig::load_from(directory.config_path());
    let original_file = fs::read(directory.config_path()).expect("测试配置原文应当可读");
    let original_llm = config.llm_settings();
    let original_personas = config.persona_settings();
    let original_snapshot = config.agent_config_snapshot();

    for field in ["model", "tts_model"] {
        let mut draft = original_personas.as_ref().clone();
        let persona = draft.personas.first_mut().expect("测试人格必须存在");
        match field {
            "model" => persona.model = Some("missing-chat".to_owned()),
            "tts_model" => persona.tts_model = Some("missing-tts".to_owned()),
            _ => unreachable!("测试字段集合固定"),
        }
        let revision = config.reserve_persona_settings_revision();

        let error = config
            .set_persona_settings_at_revision(draft, revision, AppLanguage::SimplifiedChinese)
            .expect_err("悬空人格模型绑定必须被跨域校验拒绝");

        assert!(matches!(error, ConfigWriteError::InvalidValue(_)));
        assert_eq!(
            fs::read(directory.config_path()).expect("失败后配置原文应当可读"),
            original_file
        );
        assert_eq!(config.llm_settings().as_ref(), original_llm.as_ref());
        assert_eq!(
            config.persona_settings().as_ref(),
            original_personas.as_ref()
        );
        let snapshot = config.agent_config_snapshot();
        assert_eq!(snapshot.generation(), original_snapshot.generation());
        assert_eq!(snapshot.settings().as_ref(), original_llm.as_ref());
        assert_eq!(snapshot.personas().as_ref(), original_personas.as_ref());
    }
}

#[test]
fn llm_models_round_trip_with_direct_api_key_and_advanced_options() {
    let directory = TestDirectory::new();
    directory.write(
        r#"# 保留配置注释
[custom]
enabled = true

[llm]
selected = "local"

[[llm.models]]
id = "local"
label = "本地 Qwen"
kind = "chat-completions"
provider = "ollama"
model = "qwen3:8b"
endpoint = "http://localhost:11434/"

[[llm.models]]
id = "cloud"
label = "云端模型"
kind = "chat-completions"
provider = "openai"
model = "gpt-5-mini"
api_key = "test-token+/="
future_option = "keep"
"#,
    );
    let config = LunaConfig::load_from(directory.config_path());
    let loaded = config.llm_settings();
    assert_eq!(loaded.models.len(), 2);
    assert_eq!(
        loaded.selected().map(|model| model.id.as_str()),
        Some("local")
    );
    let mut edited = loaded.as_ref().clone();
    edited.selected_model = Some("cloud".to_owned());
    if let Some(model) = edited.models.first_mut() {
        model.advanced = LlmAdvancedOptions {
            context_window_tokens: Some(32_768),
            reasoning_effort: Some(ReasoningEffort::Budget(2_048)),
            max_output_tokens: Some(512),
            temperature: Some(0.5),
            top_p: None,
        };
    }
    let revision = config.reserve_llm_settings_revision();
    config
        .set_llm_settings_at_revision(edited, revision, AppLanguage::SimplifiedChinese)
        .expect("有效语言模型配置应当可以保存")
        .expect("最新语言模型配置不应被丢弃");

    let saved = fs::read_to_string(directory.config_path()).expect("保存配置应当可以读取");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;

        let mode = fs::metadata(directory.config_path())
            .expect("保存后的配置文件应当存在")
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o600);
    }
    assert!(saved.contains("# 保留配置注释"));
    assert!(saved.contains("enabled = true"));
    assert!(saved.contains("api_key = \"test-token+/=\""));
    assert!(saved.contains("future_option = \"keep\""));
    assert!(saved.contains("reasoning_effort = \"budget\""));
    assert!(saved.contains("reasoning_budget = 2048"));
    let reloaded = LunaConfig::load_from(directory.config_path()).llm_settings();
    assert_eq!(
        reloaded.selected().map(|model| model.id.as_str()),
        Some("cloud")
    );
    assert_eq!(
        reloaded.model("local").map(|model| model.advanced.clone()),
        Some(LlmAdvancedOptions {
            context_window_tokens: Some(32_768),
            reasoning_effort: Some(ReasoningEffort::Budget(2_048)),
            max_output_tokens: Some(512),
            temperature: Some(0.5),
            top_p: None,
        })
    );
    assert_eq!(
        reloaded
            .selected()
            .and_then(|model| model.api_key.as_deref()),
        Some("test-token+/=")
    );
}

#[test]
fn inline_llm_table_becomes_a_table_before_models_are_added() {
    let directory = TestDirectory::new();
    directory.write("llm = { future_option = \"keep\" }\n");
    let config = LunaConfig::load_from(directory.config_path());
    let settings = LlmSettings {
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
    let revision = config.reserve_llm_settings_revision();
    config
        .set_llm_settings_at_revision(settings, revision, AppLanguage::SimplifiedChinese)
        .expect("内联表配置应当可以保存")
        .expect("最新配置不应被丢弃");

    let saved = fs::read_to_string(directory.config_path()).expect("保存配置应当可以读取");
    assert!(saved.contains("[llm]"), "保存内容：{saved}");
    assert!(saved.contains("[[llm.models]]"));
    let reloaded = LunaConfig::load_from(directory.config_path()).llm_settings();
    assert_eq!(reloaded.models.len(), 1);
    assert_eq!(
        reloaded.selected().map(|model| model.id.as_str()),
        Some("local")
    );
}
