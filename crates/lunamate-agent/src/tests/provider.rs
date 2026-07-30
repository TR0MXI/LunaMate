use std::{
    collections::HashSet,
    future,
    pin::Pin,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    time::Duration,
};

use futures::{StreamExt as _, channel::mpsc, stream};
use genai::{
    ModelIden,
    chat::{ChatStreamEvent as GenaiStreamEvent, MessageContent, StreamEnd, ToolCall},
    resolver::AuthData,
};
use rust_i18n::t;
use tokio::time::{Instant, sleep, timeout};

use crate::{
    config::{
        AppLanguage, LLM_PROVIDERS, LlmAdvancedOptions, LlmModelConfig, LlmProvider, ModelKind,
        ModelProvider, REASONING_EFFORT_LEVELS, ReasoningEffort,
    },
    media::prepare_dynamic_image,
    memory::AssistantTrace,
    provider::*,
    session::{ChatContextMessage, ChatRole},
    tools::OutfitOption,
};

const LANGUAGE: AppLanguage = AppLanguage::English;

fn outfit(id: &str, label: &str) -> OutfitOption {
    OutfitOption::new(id, label)
}

struct FakeScreenshotCapability {
    authorized: Arc<AtomicBool>,
    captures: Arc<AtomicUsize>,
    revoke_after_capture: bool,
}

impl ScreenshotCapability for FakeScreenshotCapability {
    fn is_authorized(&self) -> bool {
        self.authorized.load(Ordering::Acquire)
    }

    fn wait_for_revocation(&self) -> Pin<Box<dyn future::Future<Output = ()> + Send + 'static>> {
        Box::pin(future::pending())
    }

    fn capture(
        &self,
    ) -> Pin<
        Box<
            dyn future::Future<
                    Output = Result<crate::media::ImageAttachment, crate::media::ImageInputError>,
                > + Send
                + 'static,
        >,
    > {
        let authorized = Arc::clone(&self.authorized);
        let captures = Arc::clone(&self.captures);
        let revoke_after_capture = self.revoke_after_capture;
        Box::pin(async move {
            captures.fetch_add(1, Ordering::AcqRel);
            let image = prepare_dynamic_image(
                image::DynamicImage::new_rgb8(4, 4),
                "screenshot.jpg".to_owned(),
            );
            if revoke_after_capture {
                authorized.store(false, Ordering::Release);
            }
            image
        })
    }
}

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
fn screenshot_tool_is_registered_only_when_permission_is_enabled() {
    let disabled = build_request(String::new(), Vec::new(), false, &[], AppLanguage::English);
    assert!(disabled.tools.is_none());

    let enabled = build_request(String::new(), Vec::new(), true, &[], AppLanguage::English);
    let tools = enabled.tools.expect("开启权限后应当注册截屏工具");
    assert_eq!(tools.len(), 1);
    assert_eq!(tools[0].name.as_str(), SCREEN_CAPTURE_TOOL);
}

#[test]
fn screenshot_tool_description_uses_the_explicit_application_language() {
    for language in [
        AppLanguage::SimplifiedChinese,
        AppLanguage::TraditionalChinese,
        AppLanguage::English,
        AppLanguage::Japanese,
    ] {
        let request = build_request(String::new(), Vec::new(), true, &[], language);
        let tools = request.tools.expect("开启权限后应当注册截屏工具");
        assert_eq!(
            tools[0].description.as_deref(),
            Some(
                t!(
                    "chat.tool.screen_capture_description",
                    locale = language.id()
                )
                .as_ref()
            )
        );
    }
}

#[tokio::test]
async fn screenshot_capability_is_checked_before_and_after_capture() {
    let call = || ToolCall {
        call_id: "capture".to_owned(),
        fn_name: SCREEN_CAPTURE_TOOL.to_owned(),
        fn_arguments: serde_json::json!({}),
        thought_signatures: None,
    };

    let authorized = Arc::new(AtomicBool::new(false));
    let captures = Arc::new(AtomicUsize::new(0));
    let capability = Arc::new(FakeScreenshotCapability {
        authorized,
        captures: Arc::clone(&captures),
        revoke_after_capture: false,
    });
    let (result, image) = execute_screenshot_tool_for_test(call(), Some(capability)).await;
    assert_eq!(
        result,
        serde_json::json!({"status": "error", "code": "permission_disabled"})
    );
    assert!(!image);
    assert_eq!(captures.load(Ordering::Acquire), 0);

    let authorized = Arc::new(AtomicBool::new(true));
    let captures = Arc::new(AtomicUsize::new(0));
    let capability = Arc::new(FakeScreenshotCapability {
        authorized: Arc::clone(&authorized),
        captures: Arc::clone(&captures),
        revoke_after_capture: true,
    });
    let (result, image) = execute_screenshot_tool_for_test(call(), Some(capability)).await;
    assert_eq!(
        result,
        serde_json::json!({"status": "error", "code": "permission_revoked"})
    );
    assert!(!image);
    assert!(!authorized.load(Ordering::Acquire));
    assert_eq!(captures.load(Ordering::Acquire), 1);
}

#[test]
fn outfit_tool_is_registered_only_when_the_model_has_an_extra_outfit() {
    let default_only = vec![outfit("default", "Default outfit")];
    let request = build_request(
        String::new(),
        Vec::new(),
        false,
        &default_only,
        AppLanguage::English,
    );
    assert!(request.tools.is_none());

    let outfits = vec![
        outfit("default", "Default outfit"),
        outfit("detective", "Detective"),
    ];
    let request = build_request(
        String::new(),
        Vec::new(),
        false,
        &outfits,
        AppLanguage::English,
    );
    let tools = request.tools.expect("存在额外服装时应当注册换装工具");
    assert_eq!(tools.len(), 1);
    assert_eq!(tools[0].name.as_str(), CHANGE_OUTFIT_TOOL);
    assert_eq!(
        tools[0]
            .schema
            .as_ref()
            .expect("换装工具必须提供参数 schema")["properties"]["outfit"]["enum"],
        serde_json::json!(["Default outfit", "Detective"])
    );

    let outfits = vec![
        outfit("default", "Default outfit"),
        outfit("detective", "Detective"),
    ];
    let request = build_request(
        String::new(),
        Vec::new(),
        true,
        &outfits,
        AppLanguage::English,
    );
    assert_eq!(request.tools.expect("两个可用工具都应注册").len(), 2);
}

#[test]
fn outfit_tool_text_uses_the_explicit_application_language() {
    let outfits = vec![
        outfit("default", "default"),
        outfit("alternate", "alternate"),
    ];
    for language in [
        AppLanguage::SimplifiedChinese,
        AppLanguage::TraditionalChinese,
        AppLanguage::English,
        AppLanguage::Japanese,
    ] {
        let request = build_request(String::new(), Vec::new(), false, &outfits, language);
        let tool = request
            .tools
            .expect("存在额外服装时应当注册换装工具")
            .into_iter()
            .find(|tool| tool.name.as_str() == CHANGE_OUTFIT_TOOL)
            .expect("请求中应当包含换装工具");
        assert_eq!(
            tool.description.as_deref(),
            Some(
                t!(
                    "chat.tool.change_outfit_description",
                    locale = language.id()
                )
                .as_ref()
            )
        );
        assert_eq!(
            tool.schema.as_ref().expect("换装工具必须提供参数 schema")["properties"]["outfit"]["description"],
            t!("chat.tool.change_outfit_argument", locale = language.id()).as_ref()
        );
    }
}

#[test]
fn outfit_tool_rejects_unknown_or_malformed_choices() {
    let outfits = vec![
        outfit("default", "Default outfit"),
        outfit("detective-id", "Detective"),
    ];
    assert_eq!(
        outfit_argument(&serde_json::json!({"outfit": "Detective"}), &outfits)
            .map(OutfitOption::id),
        Ok("detective-id")
    );
    assert_eq!(
        outfit_argument(&serde_json::json!({"outfit": "Missing"}), &outfits),
        Err("outfit_unavailable")
    );
    assert_eq!(
        outfit_argument(
            &serde_json::json!({"outfit": "Detective", "extra": true}),
            &outfits
        ),
        Err("invalid_arguments")
    );
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
fn failed_capture_handoff_tells_the_model_to_continue_without_the_image() {
    let mut request = build_request(
        String::new(),
        vec![ChatContextMessage {
            source_message_id: None,
            role: ChatRole::User,
            content: "inspect my screen".to_owned(),
            image: None,
        }],
        true,
        &[],
        AppLanguage::English,
    );

    append_stateless_capture_result(&mut request, None, AppLanguage::English);

    assert_eq!(request.messages.len(), 1);
    assert!(request.messages[0].content.binaries().is_empty());
    assert!(request.messages[0].content.texts().iter().any(|text| {
        text.contains(
            t!(
                "chat.tool.screen_capture_unavailable",
                locale = AppLanguage::English.id()
            )
            .as_ref(),
        )
    }));
}

#[test]
fn capture_handoff_prompts_use_the_explicit_application_language() {
    let image = prepare_dynamic_image(
        image::DynamicImage::new_rgb8(10, 6),
        "screenshot.jpg".to_owned(),
    )
    .expect("测试截图应当可以规范化");
    for language in [
        AppLanguage::SimplifiedChinese,
        AppLanguage::TraditionalChinese,
        AppLanguage::English,
        AppLanguage::Japanese,
    ] {
        let mut request = build_request(String::new(), Vec::new(), true, &[], language);
        append_stateless_capture_result(&mut request, None, language);
        let expected = t!(
            "chat.tool.screen_capture_unavailable",
            locale = language.id()
        );

        assert!(
            request.messages[0]
                .content
                .texts()
                .iter()
                .any(|text| text.contains(expected.as_ref()))
        );

        let mut request = build_request(String::new(), Vec::new(), true, &[], language);
        append_stateless_capture_result(&mut request, Some(&image), language);
        let expected = t!("chat.tool.screen_capture_handoff", locale = language.id());
        assert!(
            request.messages[0]
                .content
                .texts()
                .iter()
                .any(|text| text.contains(expected.as_ref()))
        );
    }
}

#[test]
fn capture_handoff_creates_a_user_turn_when_the_request_has_no_messages() {
    let mut request = build_request(String::new(), Vec::new(), true, &[], AppLanguage::English);

    append_stateless_capture_result(&mut request, None, AppLanguage::English);

    assert_eq!(request.messages.len(), 1);
    assert!(!request.messages[0].content.texts().is_empty());
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
fn signed_tool_handoff_retries_from_original_user_message() {
    let image = prepare_dynamic_image(
        image::DynamicImage::new_rgb8(10, 6),
        "screenshot.jpg".to_owned(),
    )
    .expect("测试截图应当可以规范化");
    let mut request = build_request(
        String::new(),
        vec![ChatContextMessage {
            source_message_id: None,
            role: ChatRole::User,
            content: "inspect my screen".to_owned(),
            image: None,
        }],
        true,
        &[],
        AppLanguage::English,
    );

    append_stateless_capture_result(&mut request, Some(&image), AppLanguage::English);

    assert_eq!(request.messages.len(), 1);
    assert_eq!(request.messages[0].content.binaries().len(), 1);
    assert!(
        request.messages[0]
            .content
            .texts()
            .iter()
            .any(|text| text.contains("capture_screen"))
    );
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
        app_id: None,
        voice: None,
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

#[tokio::test]
async fn eof_after_text_flushes_partial_reply_but_is_not_completed() {
    let (mut sender, mut receiver) = mpsc::channel(4);
    let stream = stream::iter(vec![Ok(GenaiStreamEvent::Chunk(
        genai::chat::StreamChunk {
            content: "partial".to_owned(),
        },
    ))]);

    let now = Instant::now();
    let result = consume_stream(
        stream,
        now + Duration::from_secs(1),
        Duration::from_secs(1),
        now + Duration::from_secs(1),
        &mut sender,
        LANGUAGE,
    )
    .await;

    let failure = result.expect_err("缺少 End 的流不能视为完成");
    assert_eq!(
        failure.message,
        t!("chat.error.stream_ended", locale = LANGUAGE.id()).to_string()
    );
    assert!(failure.response_started);
    assert!(
        matches!(receiver.next().await, Some(ChatStreamEvent::Delta(text)) if text == "partial")
    );
}

#[tokio::test]
async fn concurrent_stream_failures_keep_their_request_languages() {
    let english = async {
        let (mut sender, _receiver) = mpsc::channel(1);
        let now = Instant::now();
        consume_stream(
            stream::empty(),
            now + Duration::from_secs(1),
            Duration::from_secs(1),
            now + Duration::from_secs(1),
            &mut sender,
            AppLanguage::English,
        )
        .await
        .expect_err("提前结束的英文流必须失败")
        .message
    };
    let japanese = async {
        let (mut sender, _receiver) = mpsc::channel(1);
        let now = Instant::now();
        consume_stream(
            stream::empty(),
            now + Duration::from_secs(1),
            Duration::from_secs(1),
            now + Duration::from_secs(1),
            &mut sender,
            AppLanguage::Japanese,
        )
        .await
        .expect_err("提前结束的日文流必须失败")
        .message
    };

    let (english, japanese) = tokio::join!(english, japanese);
    assert_eq!(english, "The model stream ended unexpectedly");
    assert_eq!(japanese, "モデルストリームが予期せず終了しました");
}

#[tokio::test]
async fn stream_error_flushes_buffered_reply_before_failure() {
    let (mut sender, mut receiver) = mpsc::channel(4);
    let stream = stream::iter(vec![
        Ok(GenaiStreamEvent::Chunk(genai::chat::StreamChunk {
            content: "partial".to_owned(),
        })),
        Err(genai::Error::Internal("test failure".to_owned())),
    ]);

    let now = Instant::now();
    let result = consume_stream(
        stream,
        now + Duration::from_secs(1),
        Duration::from_secs(1),
        now + Duration::from_secs(1),
        &mut sender,
        LANGUAGE,
    )
    .await;

    assert!(result.is_err());
    assert!(
        matches!(receiver.next().await, Some(ChatStreamEvent::Delta(text)) if text == "partial")
    );
}

#[tokio::test]
async fn empty_terminal_event_is_not_recorded_as_complete_reply() {
    let (mut sender, _) = mpsc::channel(4);
    let stream = stream::iter(vec![Ok(GenaiStreamEvent::End(
        genai::chat::StreamEnd::default(),
    ))]);

    let now = Instant::now();
    let result = consume_stream(
        stream,
        now + Duration::from_secs(1),
        Duration::from_secs(1),
        now + Duration::from_secs(1),
        &mut sender,
        LANGUAGE,
    )
    .await;

    let failure = result.expect_err("空终止事件必须失败");
    assert!(!failure.retryable);
    assert_eq!(
        failure.message,
        t!("chat.error.empty_response", locale = LANGUAGE.id()).to_string()
    );
}

#[tokio::test]
async fn ordinary_completion_captures_streamed_readable_reasoning() {
    let stream = stream::iter(vec![
        Ok(GenaiStreamEvent::ReasoningChunk(genai::chat::StreamChunk {
            content: "step one".to_owned(),
        })),
        Ok(GenaiStreamEvent::ReasoningChunk(genai::chat::StreamChunk {
            content: " and two".to_owned(),
        })),
        Ok(GenaiStreamEvent::Chunk(genai::chat::StreamChunk {
            content: "answer".to_owned(),
        })),
        Ok(GenaiStreamEvent::End(StreamEnd::default())),
    ]);
    let (mut sender, _receiver) = mpsc::channel(4);
    let now = Instant::now();

    let outcome = consume_stream(
        stream,
        now + Duration::from_secs(1),
        Duration::from_secs(1),
        now + Duration::from_secs(1),
        &mut sender,
        LANGUAGE,
    )
    .await
    .expect("普通回复应当完成");

    let StreamOutcome::Complete { reasoning } = outcome else {
        panic!("普通回复应返回完成结果");
    };
    assert_eq!(reasoning.as_deref(), Some("step one and two"));
}

#[tokio::test]
async fn ordinary_completion_prefers_terminal_captured_reasoning() {
    let stream = stream::iter(vec![
        Ok(GenaiStreamEvent::ReasoningChunk(genai::chat::StreamChunk {
            content: "streamed summary".to_owned(),
        })),
        Ok(GenaiStreamEvent::Chunk(genai::chat::StreamChunk {
            content: "answer".to_owned(),
        })),
        Ok(GenaiStreamEvent::End(StreamEnd {
            captured_reasoning_content: Some("terminal summary".to_owned()),
            ..StreamEnd::default()
        })),
    ]);
    let (mut sender, _receiver) = mpsc::channel(4);
    let now = Instant::now();

    let outcome = consume_stream(
        stream,
        now + Duration::from_secs(1),
        Duration::from_secs(1),
        now + Duration::from_secs(1),
        &mut sender,
        LANGUAGE,
    )
    .await
    .expect("普通回复应当完成");

    let StreamOutcome::Complete { reasoning } = outcome else {
        panic!("普通回复应返回完成结果");
    };
    assert_eq!(reasoning.as_deref(), Some("terminal summary"));
}

#[tokio::test]
async fn tool_only_terminal_event_returns_complete_tool_call() {
    let call = ToolCall {
        call_id: "call-1".to_owned(),
        fn_name: SCREEN_CAPTURE_TOOL.to_owned(),
        fn_arguments: serde_json::json!({}),
        thought_signatures: None,
    };
    let stream = stream::iter(vec![Ok(GenaiStreamEvent::End(StreamEnd {
        captured_content: Some(MessageContent::from_tool_calls(vec![call])),
        captured_reasoning_content: Some("reasoning handoff".to_owned()),
        ..StreamEnd::default()
    }))]);
    let (mut sender, _) = mpsc::channel(4);
    let now = Instant::now();

    let outcome = consume_stream(
        stream,
        now + Duration::from_secs(1),
        Duration::from_secs(1),
        now + Duration::from_secs(1),
        &mut sender,
        LANGUAGE,
    )
    .await
    .expect("纯工具响应不应被视为空正文");

    let StreamOutcome::ToolUse {
        assistant_message,
        calls,
        reasoning,
    } = outcome
    else {
        panic!("应当返回完整工具调用");
    };
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].fn_name, SCREEN_CAPTURE_TOOL);
    assert_eq!(reasoning.as_deref(), Some("reasoning handoff"));
    assert_eq!(
        assistant_message.content.first_reasoning_content(),
        Some("reasoning handoff")
    );
}

#[tokio::test]
async fn outfit_tool_waits_for_the_desktop_pet_to_apply_the_change() {
    let (sender, mut receiver) = mpsc::channel(1);
    let task = tokio::spawn(async move {
        let mut sender = sender;
        request_outfit_change(
            "outfit-detective".to_owned(),
            42,
            Instant::now() + Duration::from_secs(1),
            &mut sender,
        )
        .await
    });

    let Some(ChatStreamEvent::ChangeOutfit(request)) = receiver.next().await else {
        panic!("换装工具应当向桌宠视图发送语义请求");
    };
    assert_eq!(request.outfit_id(), "outfit-detective");
    assert_eq!(request.revision(), 42);
    request.complete(true);

    assert_eq!(task.await.expect("换装工具任务不应 panic"), Ok(()));
}

#[tokio::test]
async fn local_tool_trace_keeps_arguments_and_sanitized_result_only() {
    let call = ToolCall {
        call_id: "private-call-id".to_owned(),
        fn_name: SCREEN_CAPTURE_TOOL.to_owned(),
        fn_arguments: serde_json::json!({}),
        thought_signatures: Some(vec!["private-thought-signature".to_owned()]),
    };

    let executions = execute_tool_traces_for_test(&[call]).await;

    assert_eq!(executions.len(), 1);
    assert_eq!(executions[0].name(), SCREEN_CAPTURE_TOOL);
    assert_eq!(executions[0].arguments(), &serde_json::json!({}));
    assert_eq!(
        executions[0].result(),
        &serde_json::json!({"status": "error", "code": "permission_disabled"})
    );
    let encoded =
        serde_json::to_string(&AssistantTrace::new(None, executions)).expect("工具详情应可序列化");
    assert!(!encoded.contains("private-call-id"));
    assert!(!encoded.contains("private-thought-signature"));
}

#[tokio::test]
async fn non_empty_trace_is_sent_immediately_before_finished() {
    let trace = AssistantTrace::new(Some("reasoning".to_owned()), Vec::new());
    let (mut sender, mut receiver) = mpsc::channel(4);

    send_completion_events(&mut sender, trace, Instant::now() + Duration::from_secs(1)).await;

    let Some(ChatStreamEvent::Trace(trace)) = receiver.next().await else {
        panic!("完成前应先发送非空详情");
    };
    assert_eq!(trace.reasoning(), Some("reasoning"));
    assert!(matches!(
        receiver.next().await,
        Some(ChatStreamEvent::Finished)
    ));

    let (mut sender, mut receiver) = mpsc::channel(2);
    send_completion_events(
        &mut sender,
        AssistantTrace::default(),
        Instant::now() + Duration::from_secs(1),
    )
    .await;
    assert!(matches!(
        receiver.next().await,
        Some(ChatStreamEvent::Finished)
    ));
}

#[tokio::test]
async fn streamed_thought_signature_is_preserved_for_tool_handoff() {
    let call = ToolCall {
        call_id: "call-1".to_owned(),
        fn_name: SCREEN_CAPTURE_TOOL.to_owned(),
        fn_arguments: serde_json::json!({}),
        thought_signatures: None,
    };
    let stream = stream::iter(vec![
        Ok(GenaiStreamEvent::ThoughtSignatureChunk(
            genai::chat::StreamChunk {
                content: "signed-reasoning".to_owned(),
            },
        )),
        Ok(GenaiStreamEvent::End(StreamEnd {
            captured_content: Some(MessageContent::from_tool_calls(vec![call])),
            ..StreamEnd::default()
        })),
    ]);
    let (mut sender, _) = mpsc::channel(4);
    let now = Instant::now();

    let outcome = consume_stream(
        stream,
        now + Duration::from_secs(1),
        Duration::from_secs(1),
        now + Duration::from_secs(1),
        &mut sender,
        LANGUAGE,
    )
    .await
    .expect("签名工具响应应当形成续轮消息");

    let StreamOutcome::ToolUse {
        assistant_message,
        calls,
        reasoning,
    } = outcome
    else {
        panic!("应当返回完整工具调用");
    };
    assert_eq!(
        assistant_message.content.thought_signatures(),
        vec!["signed-reasoning"]
    );
    assert_eq!(
        calls[0].thought_signatures.as_deref(),
        Some(["signed-reasoning".to_owned()].as_slice())
    );
    assert!(reasoning.is_none(), "思考签名不得成为可展示推理");
}

#[tokio::test]
async fn start_event_alone_does_not_start_the_response() {
    let stream = stream::iter(vec![
        Ok(GenaiStreamEvent::Start),
        Ok(GenaiStreamEvent::Chunk(genai::chat::StreamChunk {
            content: "hello".to_owned(),
        })),
        Ok(GenaiStreamEvent::End(genai::chat::StreamEnd::default())),
    ]);
    let (mut sender, mut receiver) = mpsc::channel(4);
    let now = Instant::now();

    let outcome = consume_stream(
        stream,
        now + Duration::from_secs(1),
        Duration::from_secs(1),
        now + Duration::from_secs(1),
        &mut sender,
        LANGUAGE,
    )
    .await
    .expect("含 Start 事件的正常流应当完成");

    assert!(matches!(
        outcome,
        StreamOutcome::Complete { reasoning: None }
    ));
    assert!(matches!(receiver.next().await, Some(ChatStreamEvent::Delta(text)) if text == "hello"));
}

#[tokio::test]
async fn oversized_visible_content_is_rejected_after_flushing_what_was_sent() {
    let stream = stream::iter(vec![
        Ok(GenaiStreamEvent::Chunk(genai::chat::StreamChunk {
            content: "prefix".to_owned(),
        })),
        Ok(GenaiStreamEvent::Chunk(genai::chat::StreamChunk {
            content: "x".repeat(64 * 1024 + 1),
        })),
    ]);
    let (mut sender, mut receiver) = mpsc::channel(4);
    let now = Instant::now();

    let failure = consume_stream(
        stream,
        now + Duration::from_secs(1),
        Duration::from_secs(1),
        now + Duration::from_secs(1),
        &mut sender,
        LANGUAGE,
    )
    .await
    .expect_err("可见正文必须受字节上限约束");

    assert_eq!(
        failure.message,
        t!("chat.error.invalid_response", locale = LANGUAGE.id()).to_string()
    );
    assert!(failure.response_started);
    assert!(
        matches!(receiver.next().await, Some(ChatStreamEvent::Delta(text)) if text == "prefix")
    );
}

#[tokio::test]
async fn too_many_streamed_tool_calls_are_rejected() {
    let events = (0..5)
        .map(|index| {
            Ok(GenaiStreamEvent::ToolCallChunk(genai::chat::ToolChunk {
                tool_call: ToolCall {
                    call_id: format!("call-{index}"),
                    fn_name: SCREEN_CAPTURE_TOOL.to_owned(),
                    fn_arguments: serde_json::json!({}),
                    thought_signatures: None,
                },
            }))
        })
        .collect::<Vec<_>>();
    let (mut sender, _) = mpsc::channel(4);
    let now = Instant::now();

    let failure = consume_stream(
        stream::iter(events),
        now + Duration::from_secs(1),
        Duration::from_secs(1),
        now + Duration::from_secs(1),
        &mut sender,
        LANGUAGE,
    )
    .await
    .expect_err("超过工具调用上限必须失败");

    assert_eq!(
        failure.message,
        t!("chat.error.invalid_response", locale = LANGUAGE.id()).to_string()
    );
    assert!(!failure.retryable);
}

#[tokio::test]
async fn oversized_captured_reasoning_in_the_terminal_event_is_rejected() {
    let stream = stream::iter(vec![Ok(GenaiStreamEvent::End(StreamEnd {
        captured_reasoning_content: Some("r".repeat(MAX_HANDOFF_CONTENT_BYTES + 1)),
        ..StreamEnd::default()
    }))]);
    let (mut sender, _) = mpsc::channel(4);
    let now = Instant::now();

    let failure = consume_stream(
        stream,
        now + Duration::from_secs(1),
        Duration::from_secs(1),
        now + Duration::from_secs(1),
        &mut sender,
        LANGUAGE,
    )
    .await
    .expect_err("终止事件中的隐藏内容必须受上限约束");

    assert_eq!(
        failure.message,
        t!("chat.error.invalid_response", locale = LANGUAGE.id()).to_string()
    );
}

#[tokio::test]
async fn terminal_event_with_too_many_tool_calls_is_rejected() {
    let calls = (0..5)
        .map(|index| ToolCall {
            call_id: format!("call-{index}"),
            fn_name: SCREEN_CAPTURE_TOOL.to_owned(),
            fn_arguments: serde_json::json!({}),
            thought_signatures: None,
        })
        .collect::<Vec<_>>();
    let stream = stream::iter(vec![Ok(GenaiStreamEvent::End(StreamEnd {
        captured_content: Some(MessageContent::from_tool_calls(calls)),
        ..StreamEnd::default()
    }))]);
    let (mut sender, _) = mpsc::channel(4);
    let now = Instant::now();

    let failure = consume_stream(
        stream,
        now + Duration::from_secs(1),
        Duration::from_secs(1),
        now + Duration::from_secs(1),
        &mut sender,
        LANGUAGE,
    )
    .await
    .expect_err("终止事件中的工具调用数量必须受限");

    assert_eq!(
        failure.message,
        t!("chat.error.invalid_response", locale = LANGUAGE.id()).to_string()
    );
}

#[tokio::test]
async fn closed_receiver_stops_the_stream_without_reporting_a_failure() {
    let stream = stream::iter(vec![
        Ok(GenaiStreamEvent::Chunk(genai::chat::StreamChunk {
            content: "x".repeat(FLUSH_BYTES),
        })),
        Ok(GenaiStreamEvent::Chunk(genai::chat::StreamChunk {
            content: "more".to_owned(),
        })),
    ]);
    let (mut sender, receiver) = mpsc::channel(1);
    // 聊天视图关闭后接收端消失，网络任务应当安静收尾而不是构造错误提示。
    drop(receiver);
    let now = Instant::now();

    let outcome = consume_stream(
        stream,
        now + Duration::from_secs(1),
        Duration::from_secs(1),
        now + Duration::from_secs(1),
        &mut sender,
        LANGUAGE,
    )
    .await
    .expect("接收端关闭不应视为 Provider 失败");

    assert!(matches!(outcome, StreamOutcome::Complete { .. }));
}

#[tokio::test]
async fn oversized_hidden_handoff_content_is_rejected() {
    let stream = stream::iter(vec![Ok(GenaiStreamEvent::ReasoningChunk(
        genai::chat::StreamChunk {
            content: "x".repeat(MAX_HANDOFF_CONTENT_BYTES + 1),
        },
    ))]);
    let (mut sender, _) = mpsc::channel(4);
    let now = Instant::now();

    let failure = consume_stream(
        stream,
        now + Duration::from_secs(1),
        Duration::from_secs(1),
        now + Duration::from_secs(1),
        &mut sender,
        LANGUAGE,
    )
    .await
    .expect_err("隐藏续轮内容必须受字节上限约束");

    assert_eq!(
        failure.message,
        t!("chat.error.invalid_response", locale = LANGUAGE.id()).to_string()
    );
    assert!(!failure.retryable);
}

#[tokio::test]
async fn continuous_small_chunks_flush_without_waiting_for_512_bytes() {
    let stream = stream::unfold(0u8, |index| async move {
        match index {
            0..=7 => {
                sleep(Duration::from_millis(5)).await;
                Some((
                    Ok(GenaiStreamEvent::Chunk(genai::chat::StreamChunk {
                        content: "x".to_owned(),
                    })),
                    index + 1,
                ))
            }
            8 => {
                sleep(Duration::from_millis(80)).await;
                Some((
                    Ok(GenaiStreamEvent::End(genai::chat::StreamEnd::default())),
                    9,
                ))
            }
            _ => None,
        }
    });
    let (sender, mut receiver) = mpsc::channel(4);
    let task = tokio::spawn(async move {
        let mut sender = sender;
        let now = Instant::now();
        consume_stream(
            Box::pin(stream),
            now + Duration::from_secs(1),
            Duration::from_secs(1),
            now + Duration::from_secs(1),
            &mut sender,
            LANGUAGE,
        )
        .await
    });

    let first = timeout(Duration::from_millis(70), receiver.next())
        .await
        .expect("持续小增量也应按时间批量刷新");
    assert!(
        matches!(first, Some(ChatStreamEvent::Delta(text)) if !text.is_empty() && text.len() < FLUSH_BYTES)
    );
    assert!(task.await.expect("流任务不应 panic").is_ok());
}

#[tokio::test]
async fn reasoning_does_not_satisfy_the_first_content_deadline() {
    let stream = stream::iter(vec![Ok(GenaiStreamEvent::ReasoningChunk(
        genai::chat::StreamChunk {
            content: "thinking".to_owned(),
        },
    ))])
    .chain(stream::pending::<Result<GenaiStreamEvent, genai::Error>>());
    let (mut sender, _) = mpsc::channel(4);
    let now = Instant::now();

    let failure = consume_stream(
        Box::pin(stream),
        now + Duration::from_millis(20),
        Duration::from_secs(1),
        now + Duration::from_secs(1),
        &mut sender,
        LANGUAGE,
    )
    .await
    .expect_err("推理片段不能替代首段可见正文");

    assert_eq!(
        failure.message,
        t!("chat.error.first_content_timeout", locale = LANGUAGE.id()).to_string()
    );
    assert!(!failure.response_started);
}

#[tokio::test]
async fn semantic_idle_timeout_applies_after_visible_content() {
    let stream = stream::iter(vec![Ok(GenaiStreamEvent::Chunk(
        genai::chat::StreamChunk {
            content: "partial".to_owned(),
        },
    ))])
    .chain(stream::pending::<Result<GenaiStreamEvent, genai::Error>>());
    let (mut sender, mut receiver) = mpsc::channel(4);
    let now = Instant::now();

    let failure = consume_stream(
        Box::pin(stream),
        now + Duration::from_secs(1),
        Duration::from_millis(20),
        now + Duration::from_secs(1),
        &mut sender,
        LANGUAGE,
    )
    .await
    .expect_err("正文后的语义流空闲必须超时");

    assert_eq!(
        failure.message,
        t!("chat.error.idle_timeout", locale = LANGUAGE.id()).to_string()
    );
    assert!(failure.response_started);
    assert!(
        matches!(receiver.next().await, Some(ChatStreamEvent::Delta(text)) if text == "partial")
    );
}

#[tokio::test]
async fn total_timeout_keeps_partial_text_and_the_specific_terminal_error() {
    let stream = stream::iter(vec![Ok(GenaiStreamEvent::Chunk(
        genai::chat::StreamChunk {
            content: "partial".to_owned(),
        },
    ))])
    .chain(stream::pending::<Result<GenaiStreamEvent, genai::Error>>());
    let (mut sender, mut receiver) = mpsc::channel(4);
    let now = Instant::now();
    let total_deadline = now + Duration::from_millis(20);

    let failure = consume_stream(
        Box::pin(stream),
        now + Duration::from_secs(1),
        Duration::from_secs(1),
        total_deadline,
        &mut sender,
        LANGUAGE,
    )
    .await
    .expect_err("总时限到达后必须返回明确错误");
    assert_eq!(
        failure.message,
        t!("chat.error.total_timeout", locale = LANGUAGE.id()).to_string()
    );
    assert!(
        matches!(receiver.next().await, Some(ChatStreamEvent::Delta(text)) if text == "partial")
    );

    assert!(
        send_terminal_event(
            &mut sender,
            ChatStreamEvent::Failed(failure.message),
            total_deadline,
        )
        .await
    );
    assert!(
        matches!(receiver.next().await, Some(ChatStreamEvent::Failed(message)) if message == t!("chat.error.total_timeout", locale = LANGUAGE.id()))
    );
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
