//! 将 Provider 无关的会话快照转换为 `genai` 流，并输出受限批次事件。

use std::{future::Future, pin::Pin, time::Duration};

use futures::{SinkExt as _, StreamExt as _, channel::mpsc};
use genai::{
    Client, ModelIden, WebConfig,
    chat::{ChatMessage as GenaiMessage, ChatRequest, ChatStreamEvent as GenaiStreamEvent},
    resolver::{AuthData, Endpoint},
};
use parking_lot::Mutex;
use rust_i18n::t;
#[cfg(test)]
use tokio::time::timeout;
use tokio::time::{Instant, sleep, timeout_at};

use crate::config::LlmModelConfig;

use super::{ChatContextMessage, ChatRole};

const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const READ_TIMEOUT: Duration = Duration::from_secs(45);
const FIRST_CONTENT_TIMEOUT: Duration = Duration::from_secs(30);
const STREAM_IDLE_TIMEOUT: Duration = Duration::from_secs(45);
const TOTAL_RESPONSE_TIMEOUT: Duration = Duration::from_secs(5 * 60);
const TERMINAL_EVENT_GRACE: Duration = Duration::from_millis(100);
const FLUSH_INTERVAL: Duration = Duration::from_millis(40);
const FLUSH_BYTES: usize = 512;
const RETRY_DELAYS: [Duration; 2] = [Duration::from_millis(500), Duration::from_millis(1_500)];

/// 网络任务发送给聊天实体的有界事件。
pub(super) enum ChatStreamEvent {
    Delta(String),
    Finished,
    Failed(String),
}

/// 一次请求所需的不可变模型、提示词和上下文快照。
pub(super) struct ChatServiceRequest {
    pub(super) model: LlmModelConfig,
    pub(super) system_prompt: String,
    pub(super) messages: Vec<ChatContextMessage>,
}

/// 可由本地 fake 替换的流式请求边界。
pub(super) trait ChatBackend: Send + Sync {
    fn stream(
        &self,
        request: ChatServiceRequest,
        events: mpsc::Sender<ChatStreamEvent>,
    ) -> Pin<Box<dyn Future<Output = ()> + Send>>;
}

/// 使用锁定版 `genai` 执行真实 Provider 请求。
pub(super) struct GenaiChatBackend {
    client: Mutex<Option<(ClientKey, Client)>>,
}

#[derive(Eq, PartialEq)]
struct ClientKey {
    provider: crate::config::LlmProvider,
    endpoint: Option<String>,
    api_key: Option<String>,
}

impl GenaiChatBackend {
    /// 创建带连接池复用的 Provider 后端；凭据只保存在进程内缓存中。
    pub(super) fn new() -> Self {
        Self {
            client: Mutex::new(None),
        }
    }

    fn client_for(&self, model: &LlmModelConfig) -> Client {
        let key = ClientKey {
            provider: model.provider,
            endpoint: model.endpoint.clone(),
            api_key: model.api_key.clone(),
        };
        let mut cached = self.client.lock();
        if let Some((cached_key, client)) = cached.as_ref()
            && cached_key == &key
        {
            return client.clone();
        }
        let client = build_client(model);
        *cached = Some((key, client.clone()));
        client
    }
}

impl ChatBackend for GenaiChatBackend {
    fn stream(
        &self,
        request: ChatServiceRequest,
        events: mpsc::Sender<ChatStreamEvent>,
    ) -> Pin<Box<dyn Future<Output = ()> + Send>> {
        let client = self.client_for(&request.model);
        Box::pin(stream_chat(request, events, client))
    }
}

struct AttemptFailure {
    message: String,
    retryable: bool,
    response_started: bool,
}

/// 执行流式聊天，并且只在首段正文前进行有限退避重试。
async fn stream_chat(
    request: ChatServiceRequest,
    mut events: mpsc::Sender<ChatStreamEvent>,
    client: Client,
) {
    let ChatServiceRequest {
        model,
        system_prompt,
        messages,
    } = request;
    let total_deadline = Instant::now() + TOTAL_RESPONSE_TIMEOUT;
    let model = ModelIden::new(model.provider.adapter_kind(), model.model);
    let chat_request = build_request(system_prompt, messages);
    let mut retry_delays = RETRY_DELAYS.into_iter();
    loop {
        match stream_once(
            &client,
            model.clone(),
            chat_request.clone(),
            total_deadline,
            &mut events,
        )
        .await
        {
            Ok(()) => return,
            Err(failure) if failure.retryable && !failure.response_started => {
                let Some(delay) = retry_delays.next() else {
                    let _ = send_terminal_event(
                        &mut events,
                        ChatStreamEvent::Failed(failure.message),
                        total_deadline,
                    )
                    .await;
                    return;
                };
                let remaining = total_deadline.saturating_duration_since(Instant::now());
                if delay >= remaining {
                    let _ = send_terminal_event(
                        &mut events,
                        ChatStreamEvent::Failed(failure.message),
                        total_deadline,
                    )
                    .await;
                    return;
                }
                sleep(delay).await;
            }
            Err(failure) => {
                let _ = send_terminal_event(
                    &mut events,
                    ChatStreamEvent::Failed(failure.message),
                    total_deadline,
                )
                .await;
                return;
            }
        }
    }
}

fn build_client(model: &LlmModelConfig) -> Client {
    let adapter = model.provider.adapter_kind();
    let auth = auth_data(model);
    let mut builder = Client::builder()
        .with_adapter_kind(adapter)
        .with_auth_resolver_fn(move |_| Ok(Some(auth.clone())))
        .with_web_config(WebConfig {
            timeout: None,
            connect_timeout: Some(CONNECT_TIMEOUT),
            read_timeout: Some(READ_TIMEOUT),
            ..WebConfig::default()
        });
    if let Some(endpoint) = &model.endpoint {
        let endpoint = Endpoint::from_owned(endpoint.clone());
        builder =
            builder.with_service_target_resolver_fn(move |mut target: genai::ServiceTarget| {
                target.endpoint = endpoint.clone();
                Ok(target)
            });
    }
    builder.build()
}

fn auth_data(model: &LlmModelConfig) -> AuthData {
    model
        .api_key
        .clone()
        .map(AuthData::from_single)
        .unwrap_or(AuthData::None)
}

fn build_request(system_prompt: String, messages: Vec<ChatContextMessage>) -> ChatRequest {
    let messages = messages
        .into_iter()
        .map(|message| match message.role {
            ChatRole::User => GenaiMessage::user(message.content),
            ChatRole::Assistant => GenaiMessage::assistant(message.content),
        })
        .collect::<Vec<_>>();
    let chat_request = ChatRequest::from_messages(messages);
    if system_prompt.trim().is_empty() {
        chat_request
    } else {
        chat_request.with_system(system_prompt)
    }
}

async fn stream_once(
    client: &Client,
    model: ModelIden,
    request: ChatRequest,
    total_deadline: Instant,
    events: &mut mpsc::Sender<ChatStreamEvent>,
) -> Result<(), AttemptFailure> {
    let first_content_deadline = (Instant::now() + FIRST_CONTENT_TIMEOUT).min(total_deadline);
    let response = match timeout_at(
        first_content_deadline,
        client.exec_chat_stream(model, request, None),
    )
    .await
    {
        Ok(Ok(response)) => response,
        Ok(Err(error)) => return Err(attempt_failure(error)),
        Err(_) => {
            return Err(AttemptFailure {
                message: t!("chat.error.first_content_timeout").to_string(),
                retryable: true,
                response_started: false,
            });
        }
    };
    consume_stream(
        response.stream,
        first_content_deadline,
        STREAM_IDLE_TIMEOUT,
        total_deadline,
        events,
    )
    .await
}

async fn consume_stream<S>(
    mut stream: S,
    first_content_deadline: Instant,
    idle_timeout: Duration,
    total_deadline: Instant,
    events: &mut mpsc::Sender<ChatStreamEvent>,
) -> Result<(), AttemptFailure>
where
    S: futures::Stream<Item = Result<GenaiStreamEvent, genai::Error>> + Unpin,
{
    let mut response_started = false;
    let mut produced_content = false;
    let mut pending = String::new();
    let mut flush_deadline = None;
    let mut idle_deadline = None;

    loop {
        let now = Instant::now();
        if now >= total_deadline {
            let delivery_deadline = terminal_delivery_deadline(total_deadline);
            let _ = flush(&mut pending, events, delivery_deadline).await;
            return Err(AttemptFailure {
                message: t!("chat.error.total_timeout").to_string(),
                retryable: false,
                response_started,
            });
        }
        if !response_started && now >= first_content_deadline {
            if !flush(&mut pending, events, total_deadline).await {
                return Ok(());
            }
            return Err(AttemptFailure {
                message: t!("chat.error.first_content_timeout").to_string(),
                retryable: true,
                response_started: false,
            });
        }
        if idle_deadline.is_some_and(|deadline| now >= deadline) {
            if !flush(&mut pending, events, total_deadline).await {
                return Ok(());
            }
            return Err(AttemptFailure {
                message: t!("chat.error.idle_timeout").to_string(),
                retryable: false,
                response_started,
            });
        }

        let mut wake_at = total_deadline;
        if !response_started && first_content_deadline < wake_at {
            wake_at = first_content_deadline;
        }
        if let Some(flush_at) = flush_deadline
            && flush_at < wake_at
        {
            wake_at = flush_at;
        }
        if let Some(idle_at) = idle_deadline
            && idle_at < wake_at
        {
            wake_at = idle_at;
        }
        match timeout_at(wake_at, stream.next()).await {
            Ok(Some(Ok(GenaiStreamEvent::Chunk(chunk)))) => {
                if response_started {
                    idle_deadline = Some(Instant::now() + idle_timeout);
                }
                if !chunk.content.is_empty() {
                    response_started = true;
                    produced_content = true;
                    idle_deadline = Some(Instant::now() + idle_timeout);
                    if pending.is_empty() {
                        flush_deadline = Some(Instant::now() + FLUSH_INTERVAL);
                    }
                    pending.push_str(&chunk.content);
                    if pending.len() >= FLUSH_BYTES
                        && !flush(&mut pending, events, total_deadline).await
                    {
                        return Ok(());
                    }
                    if pending.is_empty() {
                        flush_deadline = None;
                    }
                }
            }
            Ok(Some(Ok(GenaiStreamEvent::End(_)))) => {
                let delivery_deadline = terminal_delivery_deadline(total_deadline);
                if !flush(&mut pending, events, delivery_deadline).await {
                    return Ok(());
                }
                if !produced_content {
                    return Err(AttemptFailure {
                        message: t!("chat.error.empty_response").to_string(),
                        retryable: false,
                        response_started,
                    });
                }
                let _ =
                    send_terminal_event(events, ChatStreamEvent::Finished, total_deadline).await;
                return Ok(());
            }
            Ok(Some(Ok(GenaiStreamEvent::ReasoningChunk(_))))
            | Ok(Some(Ok(GenaiStreamEvent::ThoughtSignatureChunk(_))))
            | Ok(Some(Ok(GenaiStreamEvent::ToolCallChunk(_)))) => {
                if response_started {
                    idle_deadline = Some(Instant::now() + idle_timeout);
                }
            }
            Ok(Some(Ok(GenaiStreamEvent::Start))) => {
                if response_started {
                    idle_deadline = Some(Instant::now() + idle_timeout);
                }
            }
            Ok(Some(Err(error))) => {
                if !flush(&mut pending, events, total_deadline).await {
                    return Ok(());
                }
                let mut failure = attempt_failure(error);
                failure.response_started = response_started;
                return Err(failure);
            }
            Ok(None) => {
                if !flush(&mut pending, events, total_deadline).await {
                    return Ok(());
                }
                return Err(AttemptFailure {
                    message: t!("chat.error.stream_ended").to_string(),
                    retryable: !response_started,
                    response_started,
                });
            }
            Err(_) => {
                if !pending.is_empty() && !flush(&mut pending, events, total_deadline).await {
                    return Ok(());
                }
                flush_deadline = None;
            }
        }
    }
}

async fn flush(
    pending: &mut String,
    events: &mut mpsc::Sender<ChatStreamEvent>,
    total_deadline: Instant,
) -> bool {
    if pending.is_empty() {
        return true;
    }
    let chunk = std::mem::take(pending);
    send_event(events, ChatStreamEvent::Delta(chunk), total_deadline).await
}

async fn send_event(
    events: &mut mpsc::Sender<ChatStreamEvent>,
    event: ChatStreamEvent,
    total_deadline: Instant,
) -> bool {
    timeout_at(total_deadline, events.send(event))
        .await
        .is_ok_and(|result| result.is_ok())
}

async fn send_terminal_event(
    events: &mut mpsc::Sender<ChatStreamEvent>,
    event: ChatStreamEvent,
    total_deadline: Instant,
) -> bool {
    send_event(events, event, terminal_delivery_deadline(total_deadline)).await
}

fn terminal_delivery_deadline(total_deadline: Instant) -> Instant {
    total_deadline.max(Instant::now()) + TERMINAL_EVENT_GRACE
}

fn attempt_failure(error: genai::Error) -> AttemptFailure {
    let retryable = match http_status(&error) {
        Some(status) => status == 408 || status == 429 || (500..=599).contains(&status),
        None => matches!(
            &error,
            genai::Error::WebAdapterCall { .. }
                | genai::Error::WebModelCall { .. }
                | genai::Error::WebStream { .. }
        ),
    };
    AttemptFailure {
        message: safe_error_message(&error),
        retryable,
        response_started: false,
    }
}

fn http_status(error: &genai::Error) -> Option<u16> {
    match error {
        genai::Error::HttpError { status, .. } => Some(status.as_u16()),
        genai::Error::WebStream { error, .. } => {
            error.downcast_ref::<genai::Error>().and_then(http_status)
        }
        _ => None,
    }
}

// genai 的部分 Display/Debug 文本包含完整请求或响应正文，这里只映射稳定类别。
fn safe_error_message(error: &genai::Error) -> String {
    match error {
        genai::Error::RequiresApiKey { .. }
        | genai::Error::NoAuthResolver { .. }
        | genai::Error::NoAuthData { .. } => t!("chat.error.missing_api_key").to_string(),
        genai::Error::Resolver { .. } | genai::Error::ModelMapperFailed { .. } => {
            t!("chat.error.invalid_provider").to_string()
        }
        genai::Error::HttpError { status, .. } => {
            t!("chat.error.http", status = status.as_u16()).to_string()
        }
        genai::Error::WebStream { .. } if http_status(error).is_some() => t!(
            "chat.error.http",
            status = http_status(error).unwrap_or_default()
        )
        .to_string(),
        genai::Error::AdapterKindMismatch { .. } => t!("chat.error.provider_mismatch").to_string(),
        genai::Error::MessageRoleNotSupported { .. }
        | genai::Error::MessageContentTypeNotSupported { .. }
        | genai::Error::AdapterNotSupported { .. } => {
            t!("chat.error.unsupported_request").to_string()
        }
        genai::Error::WebAdapterCall { .. }
        | genai::Error::WebModelCall { .. }
        | genai::Error::WebStream { .. } => t!("chat.error.connection").to_string(),
        genai::Error::ChatResponseGeneration { .. }
        | genai::Error::ChatResponse { .. }
        | genai::Error::StreamParse { .. }
        | genai::Error::NoChatResponse { .. } => t!("chat.error.invalid_response").to_string(),
        _ => t!("chat.error.request_failed").to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::LlmProvider;
    use futures::stream;

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
}
