//! 验证运行时配置快照只接受并保存规范化后的两个配置域。

use std::sync::Arc;

use crate::config::{
    AgentConfigSnapshot, AppLanguage, LlmAdvancedOptions, LlmModelConfig, LlmProvider, LlmSettings,
    PersonaConfig, PersonaSettings,
};

#[test]
fn snapshot_constructor_normalizes_both_settings_domains() {
    let settings = Arc::new(LlmSettings {
        models: vec![LlmModelConfig {
            id: " local-model ".to_owned(),
            label: " Local Model ".to_owned(),
            provider: LlmProvider::Ollama,
            model: " qwen3:8b ".to_owned(),
            endpoint: Some(" http://localhost:11434/v1 ".to_owned()),
            api_key: Some(" local-key ".to_owned()),
            advanced: LlmAdvancedOptions::default(),
        }],
        selected_model: Some(" local-model ".to_owned()),
    });
    let mut persona = PersonaConfig::new(" assistant ", " Assistant ");
    persona.model = Some(" local-model ".to_owned());
    let personas = Arc::new(PersonaSettings {
        personas: vec![persona],
        selected: Some(" assistant ".to_owned()),
        pending_deletions: vec![" removed ".to_owned(), "removed".to_owned()],
    });

    let snapshot =
        AgentConfigSnapshot::try_new(41, settings, personas, AppLanguage::TraditionalChinese)
            .expect("可规范化的两个配置域应当被接受");

    assert_eq!(snapshot.generation(), 41);
    assert_eq!(snapshot.language(), AppLanguage::TraditionalChinese);
    let model = &snapshot.settings().models[0];
    assert_eq!(model.id, "local-model");
    assert_eq!(model.label, "Local Model");
    assert_eq!(model.model, "qwen3:8b");
    assert_eq!(
        model.endpoint.as_deref(),
        Some("http://localhost:11434/v1/")
    );
    assert_eq!(model.api_key.as_deref(), Some("local-key"));
    assert_eq!(
        snapshot.settings().selected_model.as_deref(),
        Some("local-model")
    );
    let persona = &snapshot.personas().personas[0];
    assert_eq!(persona.id, "assistant");
    assert_eq!(persona.name, "Assistant");
    assert_eq!(persona.model.as_deref(), Some("local-model"));
    assert_eq!(snapshot.personas().selected.as_deref(), Some("assistant"));
    assert_eq!(snapshot.personas().pending_deletions, ["removed"]);
}

#[test]
fn snapshot_constructor_rejects_invalid_provider_settings() {
    let settings = Arc::new(LlmSettings {
        models: vec![LlmModelConfig {
            id: "invalid/id".to_owned(),
            label: "Invalid".to_owned(),
            provider: LlmProvider::Ollama,
            model: "model".to_owned(),
            endpoint: None,
            api_key: None,
            advanced: LlmAdvancedOptions::default(),
        }],
        selected_model: None,
    });

    assert!(
        AgentConfigSnapshot::try_new(
            1,
            settings,
            Arc::new(PersonaSettings::default()),
            AppLanguage::English,
        )
        .is_err()
    );
}

#[test]
fn snapshot_constructor_rejects_invalid_persona_settings() {
    let personas = Arc::new(PersonaSettings {
        personas: Vec::new(),
        selected: None,
        pending_deletions: Vec::new(),
    });

    assert!(
        AgentConfigSnapshot::try_new(
            1,
            Arc::new(LlmSettings::default()),
            personas,
            AppLanguage::English,
        )
        .is_err()
    );
}

#[test]
fn snapshot_validation_uses_its_explicit_language() {
    let cases = [
        (
            AppLanguage::SimplifiedChinese,
            "模型 ID 只能包含 ASCII 字母、数字、- 和 _",
        ),
        (
            AppLanguage::TraditionalChinese,
            "模型 ID 只能包含 ASCII 字母、數字、- 和 _",
        ),
        (
            AppLanguage::English,
            "Model ID may contain only ASCII letters, digits, - and _",
        ),
        (
            AppLanguage::Japanese,
            "モデル IDには ASCII の英字、数字、-、_ のみ使用できます",
        ),
    ];

    for (language, expected) in cases {
        let settings = LlmSettings {
            models: vec![LlmModelConfig {
                id: "invalid/id".to_owned(),
                label: "Invalid".to_owned(),
                provider: LlmProvider::Ollama,
                model: "model".to_owned(),
                endpoint: None,
                api_key: None,
                advanced: LlmAdvancedOptions::default(),
            }],
            selected_model: None,
        };
        let error = AgentConfigSnapshot::try_new(
            1,
            Arc::new(settings),
            Arc::new(PersonaSettings::default_for(language)),
            language,
        )
        .err()
        .expect("非法模型 ID 必须被拒绝");

        assert_eq!(error.to_string(), expected);
    }
}

#[test]
fn default_persona_name_uses_the_requested_language() {
    let cases = [
        (AppLanguage::SimplifiedChinese, "默认人格"),
        (AppLanguage::TraditionalChinese, "預設人格"),
        (AppLanguage::English, "Default persona"),
        (AppLanguage::Japanese, "既定のペルソナ"),
    ];

    for (language, expected) in cases {
        assert_eq!(
            PersonaSettings::default_for(language)
                .active()
                .map(|persona| persona.name.as_str()),
            Some(expected)
        );
    }
}
