use std::time::Duration;

use futures::{StreamExt as _, channel::mpsc, stream};
use genai::{chat::ChatStreamEvent as GenaiStreamEvent, resolver::AuthData};
use rust_i18n::t;
use tokio::time::{Instant, sleep, timeout};

use crate::{
    agent::{
        service::*,
        session::{ChatContextMessage, ChatRole},
    },
    config::{LlmModelConfig, LlmProvider},
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
        }],
    };
    let built = build_request(request.system_prompt, request.messages);

    assert_eq!(built.system.as_deref(), Some("persona"));
    assert_eq!(built.messages.len(), 1);
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
