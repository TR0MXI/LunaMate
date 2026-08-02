use std::collections::HashSet;

use genai::{ModelIden, resolver::AuthData};

use crate::{
    config::{
        AppLanguage, LLM_PROVIDERS, LlmAdvancedOptions, LlmModelConfig, LlmProvider, ModelKind,
        ModelProvider, REASONING_EFFORT_LEVELS, ReasoningEffort,
    },
    media::prepare_dynamic_image,
    provider::*,
    session::{ChatContextMessage, ChatRole},
};

use super::outfit;

#[test]
fn request_keeps_system_prompt_separate_from_history() {
    let request = ChatServiceRequest {
        model: ModelIden::new(LlmProvider::Ollama, "qwen3:8b"),
        options: None,
        system_prompt: "persona".to_owned(),
        messages: vec![ChatContextMessage {
            source_message_id: None,
            role: ChatRole::User,
            content: "hello".to_owned(),
            image: None,
        }],
        screenshot_capability: None,
        outfits: Vec::new(),
        outfit_revision: 0,
        language: AppLanguage::English,
    };
    let built = build_request(
        request.system_prompt,
        request.messages,
        request.screenshot_capability.is_some(),
        &request.outfits,
        request.language,
    );

    assert_eq!(built.system.as_deref(), Some("persona"));
    assert_eq!(built.messages.len(), 1);
    assert!(built.tools.is_none());
}

#[test]
fn cohere_adapter_is_rejected_for_binary_and_tool_requests() {
    assert!(!provider_supports_binary_and_tools(LlmProvider::Cohere));
    assert!(provider_supports_binary_and_tools(LlmProvider::OpenAI));
    let outfits = vec![
        outfit("default", "default"),
        outfit("alternate", "alternate"),
    ];
    assert!(outfit_tool_options(LlmProvider::Cohere, &outfits).is_empty());
    assert!(outfit_tool_options(LlmProvider::OpenAI, &outfits[..1]).is_empty());
    assert_eq!(outfit_tool_options(LlmProvider::OpenAI, &outfits), outfits);
    // Cohere 是唯一被排除的 Provider；其余目录项都必须支持图片与工具。
    for provider in LLM_PROVIDERS {
        assert_eq!(
            provider_supports_binary_and_tools(provider),
            provider != LlmProvider::Cohere,
            "{provider:?} 的图片与工具支持判定与目录不一致"
        );
    }
}

#[test]
fn stable_provider_catalog_contains_distinct_genai_adapters() {
    let adapters = LLM_PROVIDERS.into_iter().collect::<HashSet<_>>();
    assert_eq!(adapters.len(), LLM_PROVIDERS.len());
}

#[test]
fn history_without_pixels_degrades_to_a_text_placeholder() {
    // 快照恢复的历史消息只保留存在标记；请求构建不能因此丢失该轮次的文本。
    let restored: crate::media::ImageAttachment =
        serde_json::from_str("true").expect("历史图片快照应当可以反序列化");
    let request = build_request(
        String::new(),
        vec![ChatContextMessage {
            source_message_id: None,
            role: ChatRole::User,
            content: "what is this".to_owned(),
            image: Some(restored),
        }],
        false,
        &[],
        AppLanguage::English,
    );

    assert_eq!(request.messages.len(), 1);
    assert!(request.messages[0].content.binaries().is_empty());
    assert!(
        request.messages[0].content.first_text().is_some_and(
            |text| text.contains("what is this") && text.contains("no longer available")
        )
    );
}

#[test]
fn assistant_history_and_blank_system_prompts_are_preserved_verbatim() {
    let request = build_request(
        "   \n  ".to_owned(),
        vec![
            ChatContextMessage {
                source_message_id: None,
                role: ChatRole::User,
                content: "hi".to_owned(),
                image: None,
            },
            ChatContextMessage {
                source_message_id: None,
                role: ChatRole::Assistant,
                content: "hello".to_owned(),
                image: None,
            },
        ],
        false,
        &[],
        AppLanguage::English,
    );

    // 只含空白的系统提示词等同于未设置，不应发送空 system 消息。
    assert_eq!(request.system, None);
    assert_eq!(request.messages.len(), 2);
    assert_eq!(request.messages[1].content.first_text(), Some("hello"));
}

#[test]
fn user_image_is_encoded_as_multipart_content() {
    let source_name = "private-user-filename.jpg";
    let image = prepare_dynamic_image(image::DynamicImage::new_rgb8(10, 6), source_name.to_owned())
        .expect("测试图片应当可以规范化");
    let request = build_request(
        String::new(),
        vec![ChatContextMessage {
            source_message_id: None,
            role: ChatRole::User,
            content: "inspect".to_owned(),
            image: Some(image),
        }],
        false,
        &[],
        AppLanguage::English,
    );

    assert_eq!(request.messages[0].content.first_text(), Some("inspect"));
    let binaries = request.messages[0].content.binaries();
    assert_eq!(binaries.len(), 1);
    assert_eq!(binaries[0].name.as_deref(), Some("image.jpg"));
    assert_ne!(binaries[0].name.as_deref(), Some(source_name));
}

#[test]
fn auth_data_uses_direct_key_and_disables_environment_fallback_when_empty() {
    let mut model = LlmModelConfig {
        id: "cloud".to_owned(),
        label: "Cloud".to_owned(),
        kind: ModelKind::ChatCompletions,
        provider: ModelProvider::Genai(LlmProvider::OpenAI),
        model: "gpt-5-mini".to_owned(),
        endpoint: None,
        api_key: Some("not-an-environment-name/key".to_owned()),
        voice: None,
        voice_type: None,
        local_path: None,
        use_gpu: false,
        whisper_language: None,
        advanced: LlmAdvancedOptions::default(),
    };

    assert_eq!(
        auth_data(&model)
            .single_key_value()
            .expect("直接 API key 应当可以读取"),
        "not-an-environment-name/key"
    );
    model.api_key = None;
    assert!(matches!(auth_data(&model), AuthData::None));
}

#[test]
fn unset_advanced_options_send_no_request_overrides() {
    // 默认配置必须与接入高级参数之前的请求完全一致，避免静默改变已有行为。
    assert!(base_chat_options(&LlmAdvancedOptions::default()).is_none());
}

#[test]
fn advanced_options_map_onto_provider_request_options() {
    let options = base_chat_options(&LlmAdvancedOptions {
        context_window_tokens: Some(128_000),
        reasoning_effort: Some(ReasoningEffort::Budget(4_096)),
        max_output_tokens: Some(1_024),
        temperature: Some(0.25),
        top_p: Some(0.9),
    })
    .expect("设置了高级参数时必须构造请求选项");

    assert_eq!(options.max_tokens, Some(1_024));
    assert_eq!(options.temperature, Some(0.25));
    assert_eq!(options.top_p, Some(0.9));
    // 模型上下文窗口只用于本地历史裁剪，不能泄漏成 Provider 请求参数。
    assert!(matches!(
        options.reasoning_effort,
        Some(genai::chat::ReasoningEffort::Budget(4_096))
    ));
}

#[test]
fn every_reasoning_level_maps_to_a_distinct_provider_keyword() {
    let mut keywords = HashSet::with_capacity(REASONING_EFFORT_LEVELS.len());
    for level in REASONING_EFFORT_LEVELS {
        let options = base_chat_options(&LlmAdvancedOptions {
            reasoning_effort: Some(level.clone()),
            ..LlmAdvancedOptions::default()
        })
        .expect("选择了思考强度时必须构造请求选项");
        let effort = options.reasoning_effort.expect("思考强度必须写入请求选项");
        let keyword = effort.as_keyword().expect("非预算档位必须有对应关键字");
        assert!(
            keywords.insert(keyword),
            "{level:?} 与其他档位映射到同一关键字 {keyword}"
        );
    }
}
