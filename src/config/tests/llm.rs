use std::collections::HashSet;

use toml_edit::DocumentMut;

use crate::config::{
    LLM_PROVIDERS, LlmModelConfig, LlmProvider, LlmSettings, llm::normalize_endpoint,
    parse_llm_settings, write_llm_settings,
};

#[test]
fn provider_ids_are_unique_and_round_trip() {
    let ids = LLM_PROVIDERS
        .into_iter()
        .map(LlmProvider::id)
        .collect::<HashSet<_>>();
    assert_eq!(ids.len(), LLM_PROVIDERS.len());
    for provider in LLM_PROVIDERS {
        assert_eq!(LlmProvider::from_id(provider.id()), Some(provider));
    }
}

#[test]
fn settings_reject_missing_selection_and_duplicate_ids() {
    let model = LlmModelConfig {
        id: "local".to_owned(),
        label: "Local".to_owned(),
        provider: LlmProvider::Ollama,
        model: "qwen3:8b".to_owned(),
        endpoint: Some("http://localhost:11434".to_owned()),
        api_key: None,
    };
    let duplicate = LlmSettings {
        models: vec![model.clone(), model],
        selected_model: Some("local".to_owned()),
        system_prompt: String::new(),
    };
    assert!(duplicate.normalized().is_err());

    let missing = LlmSettings {
        selected_model: Some("missing".to_owned()),
        ..LlmSettings::default()
    };
    assert!(missing.normalized().is_err());
}

#[test]
fn direct_api_key_is_normalized_and_redacted_in_debug() {
    let settings = LlmSettings {
        models: vec![LlmModelConfig {
            id: "cloud".to_owned(),
            label: "Cloud".to_owned(),
            provider: LlmProvider::OpenAi,
            model: "gpt-5-mini".to_owned(),
            endpoint: None,
            api_key: Some(" 1/key+=value ".to_owned()),
        }],
        selected_model: Some("cloud".to_owned()),
        system_prompt: String::new(),
    };
    let normalized = settings
        .normalized()
        .expect("直接填写的 API key 应当可以规范化");
    let model = normalized.models.first().expect("测试模型应当存在");

    assert_eq!(model.api_key.as_deref(), Some("1/key+=value"));
    let debug = format!("{model:?}");
    assert!(debug.contains("[REDACTED]"));
    assert!(!debug.contains("1/key+=value"));
}

#[test]
fn legacy_environment_key_reference_is_ignored_and_removed_on_write() {
    let mut document = r#"
[llm]
selected = "cloud"

[[llm.models]]
id = "cloud"
label = "Cloud"
provider = "openai"
model = "gpt-5-mini"
api_key_env = "OPENAI_API_KEY"
"#
    .parse::<DocumentMut>()
    .expect("旧版语言模型配置应当可以解析");
    let mut warnings = Vec::new();
    let settings = parse_llm_settings(&document, &mut warnings);

    assert!(warnings.is_empty());
    assert_eq!(
        settings
            .selected()
            .and_then(|model| model.api_key.as_deref()),
        None
    );
    write_llm_settings(&mut document, &settings);
    assert!(!document.to_string().contains("api_key_env"));
}

#[test]
fn one_invalid_model_does_not_discard_the_remaining_models_or_api_keys() {
    let document = r#"
[llm]
selected = "cloud"

[[llm.models]]
id = "bad id"
label = "Broken"
provider = "openai"
model = "gpt-5-mini"
api_key = "should-not-matter"

[[llm.models]]
id = "cloud"
label = "Cloud"
provider = "openai"
model = "gpt-5-mini"
api_key = "keep-me"
"#
    .parse::<DocumentMut>()
    .expect("语言模型配置应当可以解析");
    let mut warnings = Vec::new();
    let settings = parse_llm_settings(&document, &mut warnings);

    assert_eq!(warnings.len(), 1);
    assert_eq!(settings.models.len(), 1);
    assert_eq!(
        settings
            .selected()
            .and_then(|model| model.api_key.as_deref()),
        Some("keep-me")
    );
}

#[test]
fn endpoint_normalization_preserves_base_path() {
    assert_eq!(
        normalize_endpoint(LlmProvider::OpenAi, Some("https://example.com/v1"))
            .expect("HTTPS endpoint 应当有效")
            .as_deref(),
        Some("https://example.com/v1/")
    );
    assert!(normalize_endpoint(LlmProvider::OpenAi, Some("http://example.com/v1")).is_err());
    assert!(normalize_endpoint(LlmProvider::Ollama, Some("http://example.com")).is_err());
}
