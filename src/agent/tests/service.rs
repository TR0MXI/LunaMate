use std::{collections::HashSet, time::Duration};

use futures::{StreamExt as _, channel::mpsc, stream};
use genai::{
    adapter::AdapterKind,
    chat::{ChatStreamEvent as GenaiStreamEvent, MessageContent, StreamEnd, ToolCall},
    resolver::AuthData,
};
use rust_i18n::t;
use tokio::time::{Instant, sleep, timeout};

use crate::{
    agent::{
        media::prepare_dynamic_image,
        service::*,
        session::{ChatContextMessage, ChatRole},
    },
    config::{LLM_PROVIDERS, LlmModelConfig, LlmProvider},
};

#[test]
fn request_keeps_system_prompt_separate_from_history() {
    let request = ChatServiceRequest {
        model: LlmModelConfig {
            id: "local".to_owned(),
            label: "Local".to_owned(),
            provider: LlmProvider::Ollama,
            model: "qwen3:8b".to_owned(),
            endpoint: Some("http://localhost:11434/".to_owned()),
            api_key: None,
        },
        system_prompt: "persona".to_owned(),
        messages: vec![ChatContextMessage {
            role: ChatRole::User,
            content: "hello".to_owned(),
            image: None,
        }],
        screenshot_permission_revision: None,
    };
    let built = build_request(
        request.system_prompt,
        request.messages,
        request.screenshot_permission_revision.is_some(),
    );

    assert_eq!(built.system.as_deref(), Some("persona"));
    assert_eq!(built.messages.len(), 1);
    assert!(built.tools.is_none());
}

#[test]
fn screenshot_tool_is_registered_only_when_permission_is_enabled() {
    let disabled = build_request(String::new(), Vec::new(), false);
    assert!(disabled.tools.is_none());

    let enabled = build_request(String::new(), Vec::new(), true);
    let tools = enabled.tools.expect("开启权限后应当注册截屏工具");
    assert_eq!(tools.len(), 1);
    assert_eq!(tools[0].name.as_str(), SCREEN_CAPTURE_TOOL);
}

#[test]
fn cohere_adapter_is_rejected_for_binary_and_tool_requests() {
    assert!(!provider_supports_binary_and_tools(LlmProvider::Cohere));
    assert!(provider_supports_binary_and_tools(LlmProvider::OpenAi));
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
fn every_stable_provider_maps_to_a_distinct_genai_adapter() {
    let mut adapters = HashSet::with_capacity(LLM_PROVIDERS.len());

    for provider in LLM_PROVIDERS {
        assert!(
            adapters.insert(adapter_kind_for_test(provider)),
            "{provider:?} 与其他 Provider 共用同一个 genai adapter"
        );
    }

    // 持久化的是 LunaMate 稳定 ID，映射本身必须覆盖整个目录且逐项固定。
    assert_eq!(adapters.len(), LLM_PROVIDERS.len());
    assert_eq!(
        adapter_kind_for_test(LlmProvider::Ollama),
        AdapterKind::Ollama
    );
    assert_eq!(
        adapter_kind_for_test(LlmProvider::OpenAi),
        AdapterKind::OpenAI
    );
    assert_eq!(
        adapter_kind_for_test(LlmProvider::GithubModels),
        AdapterKind::GithubCopilot
    );
}

#[test]
fn history_without_pixels_degrades_to_a_text_placeholder() {
    // 快照恢复的历史消息只保留元数据；请求构建不能因此丢失该轮次的文本。
    let restored: crate::agent::media::ImageAttachment =
        serde_json::from_str(r#"{"name":"old.jpg","width":8,"height":8}"#)
            .expect("历史图片快照应当可以反序列化");
    let request = build_request(
        String::new(),
        vec![ChatContextMessage {
            role: ChatRole::User,
            content: "what is this".to_owned(),
            image: Some(restored),
        }],
        false,
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
                role: ChatRole::User,
                content: "hi".to_owned(),
                image: None,
            },
            ChatContextMessage {
                role: ChatRole::Assistant,
                content: "hello".to_owned(),
                image: None,
            },
        ],
        false,
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
            role: ChatRole::User,
            content: "inspect my screen".to_owned(),
            image: None,
        }],
        true,
    );

    append_stateless_capture_result(&mut request, None);

    assert_eq!(request.messages.len(), 1);
    assert!(request.messages[0].content.binaries().is_empty());
    assert!(
        request.messages[0]
            .content
            .texts()
            .iter()
            .any(|text| text.contains("could not provide a screenshot"))
    );
}

#[test]
fn capture_handoff_creates_a_user_turn_when_the_request_has_no_messages() {
    let mut request = build_request(String::new(), Vec::new(), true);

    append_stateless_capture_result(&mut request, None);

    assert_eq!(request.messages.len(), 1);
    assert!(!request.messages[0].content.texts().is_empty());
}

#[test]
fn user_image_is_encoded_as_multipart_content() {
    let image = prepare_dynamic_image(image::DynamicImage::new_rgb8(10, 6), "input.jpg".to_owned())
        .expect("测试图片应当可以规范化");
    let request = build_request(
        String::new(),
        vec![ChatContextMessage {
            role: ChatRole::User,
            content: "inspect".to_owned(),
            image: Some(image),
        }],
        false,
    );

    assert_eq!(request.messages[0].content.first_text(), Some("inspect"));
    assert_eq!(request.messages[0].content.binaries().len(), 1);
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
            role: ChatRole::User,
            content: "inspect my screen".to_owned(),
            image: None,
        }],
        true,
    );

    append_stateless_capture_result(&mut request, Some(&image));

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
        provider: LlmProvider::OpenAi,
        model: "gpt-5-mini".to_owned(),
        endpoint: None,
        api_key: Some("not-an-environment-name/key".to_owned()),
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
    )
    .await;

    let failure = result.expect_err("缺少 End 的流不能视为完成");
    assert_eq!(failure.message, t!("chat.error.stream_ended").to_string());
    assert!(failure.response_started);
    assert!(
        matches!(receiver.next().await, Some(ChatStreamEvent::Delta(text)) if text == "partial")
    );
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
    )
    .await;

    let failure = result.expect_err("空终止事件必须失败");
    assert!(!failure.retryable);
    assert_eq!(failure.message, t!("chat.error.empty_response").to_string());
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
    )
    .await
    .expect("纯工具响应不应被视为空正文");

    let StreamOutcome::ToolUse {
        assistant_message,
        calls,
    } = outcome
    else {
        panic!("应当返回完整工具调用");
    };
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].fn_name, SCREEN_CAPTURE_TOOL);
    assert_eq!(
        assistant_message.content.first_reasoning_content(),
        Some("reasoning handoff")
    );
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
    )
    .await
    .expect("签名工具响应应当形成续轮消息");

    let StreamOutcome::ToolUse {
        assistant_message,
        calls,
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
    )
    .await
    .expect("含 Start 事件的正常流应当完成");

    assert!(matches!(outcome, StreamOutcome::Complete));
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
    )
    .await
    .expect_err("可见正文必须受字节上限约束");

    assert_eq!(
        failure.message,
        t!("chat.error.invalid_response").to_string()
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
    )
    .await
    .expect_err("超过工具调用上限必须失败");

    assert_eq!(
        failure.message,
        t!("chat.error.invalid_response").to_string()
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
    )
    .await
    .expect_err("终止事件中的隐藏内容必须受上限约束");

    assert_eq!(
        failure.message,
        t!("chat.error.invalid_response").to_string()
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
    )
    .await
    .expect_err("终止事件中的工具调用数量必须受限");

    assert_eq!(
        failure.message,
        t!("chat.error.invalid_response").to_string()
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
    )
    .await
    .expect("接收端关闭不应视为 Provider 失败");

    assert!(matches!(outcome, StreamOutcome::Complete));
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
    )
    .await
    .expect_err("隐藏续轮内容必须受字节上限约束");

    assert_eq!(
        failure.message,
        t!("chat.error.invalid_response").to_string()
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
    )
    .await
    .expect_err("推理片段不能替代首段可见正文");

    assert_eq!(
        failure.message,
        t!("chat.error.first_content_timeout").to_string()
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
    )
    .await
    .expect_err("正文后的语义流空闲必须超时");

    assert_eq!(failure.message, t!("chat.error.idle_timeout").to_string());
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
    )
    .await
    .expect_err("总时限到达后必须返回明确错误");
    assert_eq!(failure.message, t!("chat.error.total_timeout").to_string());
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
        matches!(receiver.next().await, Some(ChatStreamEvent::Failed(message)) if message == t!("chat.error.total_timeout"))
    );
}
