use std::time::Duration;

use futures::{StreamExt as _, channel::mpsc, stream};
use genai::chat::{ChatStreamEvent as GenaiStreamEvent, MessageContent, StreamEnd, ToolCall};
use rust_i18n::t;
use tokio::time::Instant;

use crate::{config::AppLanguage, memory::AssistantTrace, provider::*};

use super::LANGUAGE;

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
