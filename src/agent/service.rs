//! 将 Provider 无关的会话快照转换为 `genai` 流，并输出受限批次事件。

use std::{future::Future, pin::Pin, time::Duration};

use futures::{SinkExt as _, StreamExt as _, channel::mpsc};
use genai::{
    Client, ModelIden, WebConfig,
    adapter::AdapterKind,
    chat::{ChatMessage as GenaiMessage, ChatRequest, ChatStreamEvent as GenaiStreamEvent},
    resolver::{AuthData, Endpoint},
};
use parking_lot::Mutex;
use rust_i18n::t;
use tokio::time::{Instant, sleep, timeout_at};

use crate::config::{LlmModelConfig, LlmProvider};

use super::session::{ChatContextMessage, ChatRole};

const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const READ_TIMEOUT: Duration = Duration::from_secs(45);
const FIRST_CONTENT_TIMEOUT: Duration = Duration::from_secs(30);
const STREAM_IDLE_TIMEOUT: Duration = Duration::from_secs(45);
const TOTAL_RESPONSE_TIMEOUT: Duration = Duration::from_secs(5 * 60);
const TERMINAL_EVENT_GRACE: Duration = Duration::from_millis(100);
const FLUSH_INTERVAL: Duration = Duration::from_millis(40);
pub(super) const FLUSH_BYTES: usize = 512;
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
    provider: LlmProvider,
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

pub(super) struct AttemptFailure {
    pub(super) message: String,
    pub(super) retryable: bool,
    pub(super) response_started: bool,
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
    let model = ModelIden::new(adapter_kind(model.provider), model.model);
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
    let adapter = adapter_kind(model.provider);
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

const fn adapter_kind(provider: LlmProvider) -> AdapterKind {
    match provider {
        LlmProvider::OpenAi => AdapterKind::OpenAI,
        LlmProvider::OpenAiResponses => AdapterKind::OpenAIResp,
        LlmProvider::Gemini => AdapterKind::Gemini,
        LlmProvider::Anthropic => AdapterKind::Anthropic,
        LlmProvider::Fireworks => AdapterKind::Fireworks,
        LlmProvider::Together => AdapterKind::Together,
        LlmProvider::Groq => AdapterKind::Groq,
        LlmProvider::Aihubmix => AdapterKind::Aihubmix,
        LlmProvider::Mimo => AdapterKind::Mimo,
        LlmProvider::Moonshot => AdapterKind::Moonshot,
        LlmProvider::Nebius => AdapterKind::Nebius,
        LlmProvider::Xai => AdapterKind::Xai,
        LlmProvider::DeepSeek => AdapterKind::DeepSeek,
        LlmProvider::Zai => AdapterKind::Zai,
        LlmProvider::BigModel => AdapterKind::BigModel,
        LlmProvider::Aliyun => AdapterKind::Aliyun,
        LlmProvider::Baidu => AdapterKind::Baidu,
        LlmProvider::Cohere => AdapterKind::Cohere,
        LlmProvider::Ollama => AdapterKind::Ollama,
        LlmProvider::OllamaCloud => AdapterKind::OllamaCloud,
        LlmProvider::Vertex => AdapterKind::Vertex,
        LlmProvider::GithubModels => AdapterKind::GithubCopilot,
        LlmProvider::OpenCodeGo => AdapterKind::OpenCodeGo,
        LlmProvider::BedrockApi => AdapterKind::BedrockApi,
        LlmProvider::OpenRouter => AdapterKind::OpenRouter,
        LlmProvider::Minimax => AdapterKind::MiniMax,
    }
}

pub(super) fn auth_data(model: &LlmModelConfig) -> AuthData {
    model
        .api_key
        .clone()
        .map(AuthData::from_single)
        .unwrap_or(AuthData::None)
}

pub(super) fn build_request(
    system_prompt: String,
    messages: Vec<ChatContextMessage>,
) -> ChatRequest {
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

pub(super) async fn consume_stream<S>(
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

pub(super) async fn send_terminal_event(
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
