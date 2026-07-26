//! 验证 Provider 设置草稿的展示名映射与模型 ID 分配。

use std::collections::HashSet;

use crate::{
    agent::settings::{
        next_model_id_for_test, non_empty_for_test, provider_display_name_for_test,
        provider_from_display_name_for_test,
    },
    config::{LLM_PROVIDERS, LlmModelConfig, LlmProvider, LlmSettings},
};

fn model(id: &str) -> LlmModelConfig {
    LlmModelConfig {
        id: id.to_owned(),
        label: "Model".to_owned(),
        provider: LlmProvider::Ollama,
        model: "qwen3:8b".to_owned(),
        endpoint: None,
        api_key: None,
    }
}

#[test]
fn provider_display_names_are_unique_and_round_trip_through_the_selector() {
    let mut names = HashSet::with_capacity(LLM_PROVIDERS.len());

    for provider in LLM_PROVIDERS {
        let name = provider_display_name_for_test(provider);
        assert!(!name.is_empty(), "{provider:?} 应当有展示名");
        assert!(names.insert(name), "展示名 {name} 与其他 Provider 冲突");
        // 选择器只保存展示名，映射必须可逆，否则保存会静默改写 Provider。
        assert_eq!(provider_from_display_name_for_test(name), Some(provider));
    }

    assert_eq!(names.len(), LLM_PROVIDERS.len());
}

#[test]
fn unknown_display_names_do_not_resolve_to_a_provider() {
    for name in ["", "ollama", "OpenAI ", "Unknown Provider"] {
        assert_eq!(
            provider_from_display_name_for_test(name),
            None,
            "{name:?} 不应匹配"
        );
    }
}

#[test]
fn new_model_ids_skip_identifiers_already_in_use() {
    assert_eq!(next_model_id_for_test(&LlmSettings::default()), "model-1");

    let settings = LlmSettings {
        models: vec![model("model-1"), model("model-3"), model("local")],
        ..LlmSettings::default()
    };
    assert_eq!(next_model_id_for_test(&settings), "model-2");

    let contiguous = LlmSettings {
        models: (1..=3)
            .map(|index| model(&format!("model-{index}")))
            .collect(),
        ..LlmSettings::default()
    };
    assert_eq!(next_model_id_for_test(&contiguous), "model-4");
}

#[test]
fn optional_form_fields_treat_whitespace_as_unset() {
    assert_eq!(
        non_empty_for_test("  https://example.com/  "),
        Some("https://example.com/".to_owned())
    );
    assert_eq!(non_empty_for_test(""), None);
    assert_eq!(non_empty_for_test("   \t\n "), None);
}
