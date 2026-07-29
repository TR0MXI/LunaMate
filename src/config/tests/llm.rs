use std::collections::HashSet;

use toml_edit::DocumentMut;

use crate::config::{
    LLM_PROVIDERS, LlmAdvancedOptions, LlmModelConfig, LlmProvider, LlmSettings,
    MAX_OUTPUT_TOKENS_MAX, MODEL_CONTEXT_TOKENS_MAX, REASONING_EFFORT_LEVELS, ReasoningEffort,
    TEMPERATURE_MAX, llm::normalize_endpoint, parse_llm_settings, write_llm_settings,
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
        advanced: LlmAdvancedOptions::default(),
    };
    let duplicate = LlmSettings {
        models: vec![model.clone(), model],
        selected_model: Some("local".to_owned()),
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
            advanced: LlmAdvancedOptions::default(),
        }],
        selected_model: Some("cloud".to_owned()),
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

#[test]
fn blank_endpoints_are_treated_as_provider_defaults() {
    for endpoint in [None, Some(""), Some("   ")] {
        assert_eq!(
            normalize_endpoint(LlmProvider::OpenAi, endpoint).expect("空 endpoint 应当合法"),
            None
        );
    }
}

#[test]
fn plain_http_is_only_allowed_for_loopback_hosts() {
    for endpoint in [
        "http://localhost:11434",
        "http://LOCALHOST:11434",
        "http://127.0.0.1:11434",
        "http://[::1]:11434",
    ] {
        assert!(
            normalize_endpoint(LlmProvider::Ollama, Some(endpoint)).is_ok(),
            "本地回环 {endpoint} 应当允许明文 HTTP"
        );
    }

    for endpoint in [
        "http://192.168.1.10:11434",
        "http://example.com",
        "http://[2001:db8::1]:11434",
    ] {
        assert!(
            normalize_endpoint(LlmProvider::Ollama, Some(endpoint)).is_err(),
            "非回环 {endpoint} 不应允许明文 HTTP"
        );
    }
}

#[test]
fn endpoints_carrying_credentials_or_extra_url_parts_are_rejected() {
    for endpoint in [
        "https://user@example.com/v1",
        "https://user:secret@example.com/v1",
        "https://example.com/v1?key=secret",
        "https://example.com/v1#fragment",
        "not-a-url",
        "file:///etc/hosts",
    ] {
        assert!(
            normalize_endpoint(LlmProvider::OpenAi, Some(endpoint)).is_err(),
            "{endpoint} 不应作为 Provider endpoint 接受"
        );
    }
}

#[test]
fn providers_with_fixed_endpoints_reject_overrides() {
    for provider in [LlmProvider::Zai, LlmProvider::Baidu] {
        let error = normalize_endpoint(provider, Some("https://example.com/v1"))
            .expect_err("固定 endpoint 的 Provider 不应接受覆盖");
        assert!(!error.to_string().is_empty());
    }
}

#[test]
fn oversized_endpoints_are_rejected_before_parsing() {
    let endpoint = format!("https://example.com/{}", "a".repeat(2_048));

    assert!(normalize_endpoint(LlmProvider::OpenAi, Some(&endpoint)).is_err());
}

#[test]
fn trailing_slashes_are_collapsed_into_one_base_path() {
    assert_eq!(
        normalize_endpoint(LlmProvider::OpenAi, Some("https://example.com///"))
            .expect("根路径 endpoint 应当有效")
            .as_deref(),
        Some("https://example.com/")
    );
    assert_eq!(
        normalize_endpoint(LlmProvider::OpenAi, Some(" https://example.com/v1//// "))
            .expect("带空白与多余斜杠的 endpoint 应当有效")
            .as_deref(),
        Some("https://example.com/v1/")
    );
}

#[test]
fn selected_returns_only_a_model_that_still_exists() {
    let mut settings = LlmSettings {
        models: vec![LlmModelConfig {
            id: "local".to_owned(),
            label: "Local".to_owned(),
            provider: LlmProvider::Ollama,
            model: "qwen3:8b".to_owned(),
            endpoint: None,
            api_key: None,
            advanced: LlmAdvancedOptions::default(),
        }],
        selected_model: Some("local".to_owned()),
    };

    assert_eq!(
        settings.selected().map(|model| model.id.as_str()),
        Some("local")
    );

    settings.selected_model = Some("removed".to_owned());
    assert!(settings.selected().is_none());

    settings.selected_model = None;
    assert!(settings.selected().is_none());
    assert!(LlmSettings::default().selected().is_none());
}

#[test]
fn required_model_fields_are_trimmed_and_bounded() {
    let base = LlmModelConfig {
        id: " local ".to_owned(),
        label: "  Local  ".to_owned(),
        provider: LlmProvider::Ollama,
        model: " qwen3:8b ".to_owned(),
        endpoint: None,
        api_key: Some("   ".to_owned()),
        advanced: LlmAdvancedOptions::default(),
    };
    let normalized = LlmSettings {
        models: vec![base.clone()],
        selected_model: Some(" local ".to_owned()),
    }
    .normalized()
    .expect("去除空白后的配置应当有效");

    assert_eq!(normalized.models[0].id, "local");
    assert_eq!(normalized.models[0].label, "Local");
    assert_eq!(normalized.models[0].model, "qwen3:8b");
    // 只含空白的 API key 等同于未设置，不应写入配置文件。
    assert_eq!(normalized.models[0].api_key, None);
    assert_eq!(normalized.selected_model.as_deref(), Some("local"));

    for invalid in [
        LlmModelConfig {
            id: "  ".to_owned(),
            ..base.clone()
        },
        LlmModelConfig {
            id: "local model".to_owned(),
            ..base.clone()
        },
        LlmModelConfig {
            id: "本地".to_owned(),
            ..base.clone()
        },
        LlmModelConfig {
            label: String::new(),
            ..base.clone()
        },
        LlmModelConfig {
            model: " ".to_owned(),
            ..base.clone()
        },
        LlmModelConfig {
            id: "a".repeat(65),
            ..base.clone()
        },
        LlmModelConfig {
            label: "l".repeat(129),
            ..base.clone()
        },
        LlmModelConfig {
            model: "m".repeat(257),
            ..base.clone()
        },
        LlmModelConfig {
            api_key: Some("k".repeat(4 * 1024 + 1)),
            ..base
        },
    ] {
        assert!(
            LlmSettings {
                models: vec![invalid.clone()],
                selected_model: None,
            }
            .normalized()
            .is_err(),
            "{invalid:?} 应当被拒绝"
        );
    }
}

#[test]
fn model_count_and_system_prompt_have_hard_limits() {
    let model = |index: usize| LlmModelConfig {
        id: format!("model-{index}"),
        label: format!("Model {index}"),
        provider: LlmProvider::Ollama,
        model: "qwen3:8b".to_owned(),
        endpoint: None,
        api_key: None,
        advanced: LlmAdvancedOptions::default(),
    };

    assert!(
        LlmSettings {
            models: (0..64).map(model).collect(),
            selected_model: None,
        }
        .normalized()
        .is_ok()
    );
    assert!(
        LlmSettings {
            models: (0..65).map(model).collect(),
            selected_model: None,
        }
        .normalized()
        .is_err()
    );
}

#[test]
fn advanced_options_outside_their_range_are_rejected() {
    let with_advanced = |advanced: LlmAdvancedOptions| LlmSettings {
        models: vec![LlmModelConfig {
            id: "local".to_owned(),
            label: "Local".to_owned(),
            provider: LlmProvider::Ollama,
            model: "qwen3:8b".to_owned(),
            endpoint: None,
            api_key: None,
            advanced,
        }],
        selected_model: None,
    };

    for level in REASONING_EFFORT_LEVELS {
        assert!(
            with_advanced(LlmAdvancedOptions {
                reasoning_effort: Some(level),
                ..LlmAdvancedOptions::default()
            })
            .normalized()
            .is_ok(),
            "{level:?} 应当是合法档位"
        );
    }
    assert!(
        with_advanced(LlmAdvancedOptions {
            context_window_tokens: Some(MODEL_CONTEXT_TOKENS_MAX + 1),
            ..LlmAdvancedOptions::default()
        })
        .normalized()
        .is_err()
    );
    assert!(
        with_advanced(LlmAdvancedOptions {
            max_output_tokens: Some(0),
            ..LlmAdvancedOptions::default()
        })
        .normalized()
        .is_err()
    );
    assert!(
        with_advanced(LlmAdvancedOptions {
            max_output_tokens: Some(MAX_OUTPUT_TOKENS_MAX + 1),
            ..LlmAdvancedOptions::default()
        })
        .normalized()
        .is_err()
    );
    assert!(
        with_advanced(LlmAdvancedOptions {
            context_window_tokens: Some(4_600),
            max_output_tokens: Some(4_096),
            ..LlmAdvancedOptions::default()
        })
        .normalized()
        .is_err()
    );
    assert!(
        with_advanced(LlmAdvancedOptions {
            temperature: Some(TEMPERATURE_MAX + 0.1),
            ..LlmAdvancedOptions::default()
        })
        .normalized()
        .is_err()
    );
    // 非有限值来自手写配置，必须在发布前挡住而不是发给 Provider。
    assert!(
        with_advanced(LlmAdvancedOptions {
            top_p: Some(f64::NAN),
            ..LlmAdvancedOptions::default()
        })
        .normalized()
        .is_err()
    );
}

#[test]
fn malformed_llm_tables_produce_warnings_instead_of_dropping_the_config() {
    let document = r#"
[llm]
selected = 42
models = "not-an-array"
"#
    .parse::<DocumentMut>()
    .expect("测试配置应当可以解析");
    let mut warnings = Vec::new();

    let settings = parse_llm_settings(&document, &mut warnings);

    assert_eq!(settings, LlmSettings::default());
    assert_eq!(warnings.len(), 2);
    assert!(
        warnings
            .iter()
            .any(|warning| warning.contains("llm.selected"))
    );
    assert!(
        warnings
            .iter()
            .any(|warning| warning.contains("llm.models"))
    );
}

#[test]
fn selection_pointing_at_a_removed_model_is_dropped_with_a_warning() {
    let document = r#"
[llm]
selected = "  removed  "

[[llm.models]]
id = "local"
label = "Local"
provider = "ollama"
model = "qwen3:8b"
"#
    .parse::<DocumentMut>()
    .expect("测试配置应当可以解析");
    let mut warnings = Vec::new();

    let settings = parse_llm_settings(&document, &mut warnings);

    assert_eq!(settings.models.len(), 1);
    assert_eq!(settings.selected_model, None);
    assert!(warnings.iter().any(|warning| warning.contains("removed")));
}

#[test]
fn stored_advanced_options_round_trip_and_only_the_broken_entry_is_dropped() {
    let mut document = r#"
[[llm.models]]
id = "broken"
label = "Broken"
provider = "ollama"
model = "qwen3:8b"
reasoning_effort = "sideways"

[[llm.models]]
id = "good"
label = "Good"
provider = "openai"
model = "gpt-5-mini"
context_window_tokens = 128000
reasoning_effort = "budget"
reasoning_budget = 1024
max_output_tokens = 256
temperature = 1
top_p = 0.5
"#
    .parse::<DocumentMut>()
    .expect("测试配置应当可以解析");
    let mut warnings = Vec::new();

    let settings = parse_llm_settings(&document, &mut warnings);

    assert_eq!(settings.models.len(), 1);
    assert_eq!(warnings.len(), 1);
    let advanced = settings
        .model("good")
        .map(|model| model.advanced)
        .expect("合法条目必须保留");
    assert_eq!(
        advanced,
        LlmAdvancedOptions {
            context_window_tokens: Some(128_000),
            reasoning_effort: Some(ReasoningEffort::Budget(1_024)),
            max_output_tokens: Some(256),
            // 手写配置常把 1.0 写成整数 1，解析必须接受两种字面量。
            temperature: Some(1.0),
            top_p: Some(0.5),
        }
    );

    write_llm_settings(&mut document, &settings);
    let mut rewritten_warnings = Vec::new();
    assert_eq!(
        parse_llm_settings(&document, &mut rewritten_warnings),
        settings
    );
    assert!(rewritten_warnings.is_empty());
}

#[test]
fn unknown_provider_ids_only_discard_the_offending_model() {
    let document = r#"
[[llm.models]]
id = "legacy"
label = "Legacy"
provider = "not-a-provider"
model = "legacy-model"

[[llm.models]]
id = "local"
label = "Local"
provider = "ollama"
model = "qwen3:8b"
"#
    .parse::<DocumentMut>()
    .expect("测试配置应当可以解析");
    let mut warnings = Vec::new();

    let settings = parse_llm_settings(&document, &mut warnings);

    assert_eq!(settings.models.len(), 1);
    assert_eq!(settings.models[0].id, "local");
    assert!(
        warnings
            .iter()
            .any(|warning| warning.contains("not-a-provider"))
    );
}

#[test]
fn documents_without_an_llm_table_parse_as_default_settings() {
    let document = "[system]\nframe_rate = \"60\"\n"
        .parse::<DocumentMut>()
        .expect("测试配置应当可以解析");
    let mut warnings = Vec::new();

    assert_eq!(
        parse_llm_settings(&document, &mut warnings),
        LlmSettings::default()
    );
    assert!(warnings.is_empty());
}
