//! 验证供应商设置草稿的展示名映射、图标资源、ID 分配与高级参数映射。

use std::collections::HashSet;

use crate::{
    agent::settings::{
        next_model_id_for_test, non_empty_for_test, provider_display_name_for_test,
        provider_from_display_name_for_test, provider_icon_for_test, reasoning_index_for_test,
        reasoning_option_count_for_test,
    },
    config::{
        LLM_PROVIDERS, LlmAdvancedOptions, LlmModelConfig, LlmProvider, LlmSettings,
        REASONING_EFFORT_LEVELS, ReasoningEffort,
    },
};

fn model(id: &str) -> LlmModelConfig {
    LlmModelConfig {
        id: id.to_owned(),
        label: "Model".to_owned(),
        provider: LlmProvider::Ollama,
        model: "qwen3:8b".to_owned(),
        endpoint: None,
        api_key: None,
        advanced: LlmAdvancedOptions::default(),
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

#[test]
fn every_provider_resolves_to_a_bundled_icon_asset() {
    for provider in LLM_PROVIDERS {
        let path = provider_icon_for_test(provider);
        assert_eq!(
            path,
            format!("icons/providers/{}.svg", provider.id()),
            "{provider:?} 的图标路径必须使用稳定 Provider ID"
        );
        // 资源随二进制一起分发，缺文件会在运行时静默变成空白图标。
        let file = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("assets")
            .join(&path);
        assert!(file.is_file(), "缺少图标资源 {}", file.display());
    }
}

#[test]
fn reasoning_effort_round_trips_through_the_selector_index() {
    assert_eq!(reasoning_index_for_test(None), 0);
    for (offset, level) in REASONING_EFFORT_LEVELS.into_iter().enumerate() {
        assert_eq!(reasoning_index_for_test(Some(level)), offset + 1);
    }
    // 自定义预算共用最后一项，token 数由独立输入框保存。
    assert_eq!(
        reasoning_index_for_test(Some(ReasoningEffort::Budget(1_024))),
        REASONING_EFFORT_LEVELS.len() + 1
    );
    assert_eq!(
        reasoning_option_count_for_test(),
        REASONING_EFFORT_LEVELS.len() + 2
    );
}
