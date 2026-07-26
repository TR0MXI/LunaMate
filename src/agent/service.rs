//! 将 Provider 无关的会话快照转换为 `genai` 流，并输出受限批次事件。

use std::{collections::HashSet, future::Future, pin::Pin, sync::Arc, time::Duration};

use base64::{Engine as _, engine::general_purpose::STANDARD};
use futures::{SinkExt as _, StreamExt as _, channel::mpsc};
use genai::{
    Client, ModelIden, WebConfig,
    adapter::AdapterKind,
    chat::{
        ChatMessage as GenaiMessage, ChatOptions, ChatRequest, ChatStreamEvent as GenaiStreamEvent,
        ContentPart, MessageContent, ReasoningEffort as GenaiReasoningEffort, Tool, ToolCall,
        ToolResponse,
    },
    resolver::{AuthData, Endpoint},
};
use parking_lot::Mutex;
use rust_i18n::t;
use serde_json::json;
use tokio::time::{Instant, sleep, timeout_at};

use crate::config::{CONFIG, LlmAdvancedOptions, LlmModelConfig, LlmProvider, ReasoningEffort};

use super::{
    media::{ImageAttachment, capture_primary_screen},
    session::{ChatContextMessage, ChatRole},
};

const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const READ_TIMEOUT: Duration = Duration::from_secs(45);
const FIRST_CONTENT_TIMEOUT: Duration = Duration::from_secs(30);
const STREAM_IDLE_TIMEOUT: Duration = Duration::from_secs(45);
const TOTAL_RESPONSE_TIMEOUT: Duration = Duration::from_secs(5 * 60);
const TERMINAL_EVENT_GRACE: Duration = Duration::from_millis(100);
const FLUSH_INTERVAL: Duration = Duration::from_millis(40);
const SCREEN_CAPTURE_TIMEOUT: Duration = Duration::from_secs(30);
const MAX_STREAM_CONTENT_BYTES: usize = 64 * 1024;
pub(super) const MAX_HANDOFF_CONTENT_BYTES: usize = 256 * 1024;
const MAX_TOOL_CALLS: usize = 4;
pub(super) const FLUSH_BYTES: usize = 512;
const RETRY_DELAYS: [Duration; 2] = [Duration::from_millis(500), Duration::from_millis(1_500)];
pub(super) const SCREEN_CAPTURE_TOOL: &str = "capture_screen";
/// 交回截图时给模型的固定指引；两条 handoff 路径必须使用同一措辞。
const CAPTURE_HANDOFF_PROMPT: &str =
    "The authorized capture_screen tool produced this screenshot. Inspect it before answering.";

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
    pub(super) screenshot_permission_revision: Option<u64>,
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
    client: Arc<Mutex<Option<(ClientKey, Client)>>>,
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
            client: Arc::new(Mutex::new(None)),
        }
    }
}

fn client_for(cache: &Mutex<Option<(ClientKey, Client)>>, model: &LlmModelConfig) -> Client {
    let key = ClientKey {
        provider: model.provider,
        endpoint: model.endpoint.clone(),
        api_key: model.api_key.clone(),
    };
    let mut cached = cache.lock();
    if let Some((cached_key, client)) = cached.as_ref()
        && cached_key == &key
    {
        return client.clone();
    }
    let client = build_client(model);
    *cached = Some((key, client.clone()));
    client
}

impl ChatBackend for GenaiChatBackend {
    fn stream(
        &self,
        request: ChatServiceRequest,
        events: mpsc::Sender<ChatStreamEvent>,
    ) -> Pin<Box<dyn Future<Output = ()> + Send>> {
        // client 构建包含系统代理与 CA 加载等阻塞 I/O，推迟到 future 内部，避免阻塞 UI 线程。
        let cache = Arc::clone(&self.client);
        Box::pin(async move {
            let client = client_for(&cache, &request.model);
            stream_chat(request, events, client).await;
        })
    }
}

#[derive(Debug)]
pub(super) struct AttemptFailure {
    pub(super) message: String,
    pub(super) retryable: bool,
    pub(super) response_started: bool,
}

/// 单次 Provider 流结束后的语义结果。
#[derive(Debug)]
pub(super) enum StreamOutcome {
    Complete,
    ToolUse {
        assistant_message: GenaiMessage,
        calls: Vec<ToolCall>,
    },
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
        screenshot_permission_revision,
    } = request;
    let total_deadline = Instant::now() + TOTAL_RESPONSE_TIMEOUT;
    if !provider_supports_binary_and_tools(model.provider)
        && messages.iter().any(|message| {
            message
                .image
                .as_ref()
                .and_then(ImageAttachment::bytes)
                .is_some()
        })
    {
        let _ = send_terminal_event(
            &mut events,
            ChatStreamEvent::Failed(t!("chat.error.provider_image_unsupported").to_string()),
            total_deadline,
        )
        .await;
        return;
    }
    let screenshot_permission_revision = screenshot_permission_revision
        .filter(|revision| CONFIG.agent_screenshot_permission_is_current(*revision));
    let register_screenshot_tool = screenshot_permission_revision.is_some()
        && provider_supports_binary_and_tools(model.provider);
    let base_options = base_chat_options(&model.advanced);
    let model = ModelIden::new(adapter_kind(model.provider), model.model);
    let mut chat_request = build_request(system_prompt, messages, register_screenshot_tool);
    let mut used_screen_capture = false;
    loop {
        let required_permission = (register_screenshot_tool || used_screen_capture)
            .then_some(screenshot_permission_revision)
            .flatten();
        let capture_tool_handoff = register_screenshot_tool || used_screen_capture;
        let attempt = StreamAttempt {
            model: model.clone(),
            request: chat_request.clone(),
            options: base_options.clone(),
            total_deadline,
            screenshot_permission_revision: required_permission,
            capture_tool_handoff,
        };
        let outcome = match stream_with_retry(&client, &attempt, &mut events).await {
            Ok(outcome) => outcome,
            Err(failure) => {
                let _ = send_terminal_event(
                    &mut events,
                    ChatStreamEvent::Failed(failure.message),
                    total_deadline,
                )
                .await;
                return;
            }
        };
        match outcome {
            StreamOutcome::Complete => {
                let _ = send_terminal_event(&mut events, ChatStreamEvent::Finished, total_deadline)
                    .await;
                return;
            }
            StreamOutcome::ToolUse {
                assistant_message,
                calls,
            } => {
                if used_screen_capture {
                    let _ = send_terminal_event(
                        &mut events,
                        ChatStreamEvent::Failed(t!("chat.error.tool_loop").to_string()),
                        total_deadline,
                    )
                    .await;
                    return;
                }
                let mut continuation =
                    execute_tool_calls(&calls, total_deadline, screenshot_permission_revision)
                        .await;
                if !screenshot_permission_revision
                    .is_some_and(|revision| CONFIG.agent_screenshot_permission_is_current(revision))
                {
                    continuation.revoke_image();
                }
                if assistant_message.content.thought_signatures().is_empty() {
                    chat_request.messages.push(assistant_message);
                    chat_request.messages.push(GenaiMessage::tool(
                        MessageContent::from_tool_responses(continuation.responses),
                    ));
                    if let Some(image) = continuation.image
                        && let Some(part) = image_content_part(&image)
                    {
                        chat_request
                            .messages
                            .push(GenaiMessage::user(MessageContent::from_parts(vec![
                                ContentPart::from_text(CAPTURE_HANDOFF_PROMPT),
                                part,
                            ])));
                    }
                } else {
                    // genai 0.6.5 的部分适配器会接收签名流却无法在请求中回写；把截图并入原用户轮次可安全重试而不伪造 handoff。
                    append_stateless_capture_result(&mut chat_request, continuation.image.as_ref());
                }
                chat_request.tools = None;
                used_screen_capture = true;
            }
        }
    }
}

/// 一次流式尝试的全部不变输入。
///
/// 重试与截图工具循环都会重复使用同一份配置，集中保存可以避免在多层调用之间
/// 逐个传递并意外错位。
struct StreamAttempt {
    model: ModelIden,
    request: ChatRequest,
    options: Option<ChatOptions>,
    total_deadline: Instant,
    screenshot_permission_revision: Option<u64>,
    capture_tool_handoff: bool,
}

async fn stream_with_retry(
    client: &Client,
    attempt: &StreamAttempt,
    events: &mut mpsc::Sender<ChatStreamEvent>,
) -> Result<StreamOutcome, AttemptFailure> {
    let mut retry_delays = RETRY_DELAYS.into_iter();
    let total_deadline = attempt.total_deadline;
    loop {
        if attempt
            .screenshot_permission_revision
            .is_some_and(|revision| !CONFIG.agent_screenshot_permission_is_current(revision))
        {
            return Err(screenshot_permission_revoked_failure());
        }
        match stream_once(client, attempt, events).await {
            Ok(outcome) => return Ok(outcome),
            Err(failure) if failure.retryable && !failure.response_started => {
                let Some(delay) = retry_delays.next() else {
                    return Err(failure);
                };
                let remaining = total_deadline.saturating_duration_since(Instant::now());
                if delay >= remaining {
                    return Err(failure);
                }
                sleep(delay).await;
            }
            Err(failure) => return Err(failure),
        }
    }
}

/// 把供应商高级参数翻译为 `genai` 请求选项；全部未设置时返回 `None` 以沿用 Provider 默认值。
pub(super) fn base_chat_options(advanced: &LlmAdvancedOptions) -> Option<ChatOptions> {
    let LlmAdvancedOptions {
        reasoning_effort,
        max_output_tokens,
        temperature,
        top_p,
    } = *advanced;
    if reasoning_effort.is_none()
        && max_output_tokens.is_none()
        && temperature.is_none()
        && top_p.is_none()
    {
        return None;
    }

    let mut options = ChatOptions::default();
    if let Some(effort) = reasoning_effort {
        options = options.with_reasoning_effort(genai_reasoning_effort(effort));
    }
    if let Some(tokens) = max_output_tokens {
        options = options.with_max_tokens(tokens);
    }
    if let Some(temperature) = temperature {
        options = options.with_temperature(temperature);
    }
    if let Some(top_p) = top_p {
        options = options.with_top_p(top_p);
    }
    Some(options)
}

const fn genai_reasoning_effort(effort: ReasoningEffort) -> GenaiReasoningEffort {
    match effort {
        ReasoningEffort::Off => GenaiReasoningEffort::None,
        ReasoningEffort::Minimal => GenaiReasoningEffort::Minimal,
        ReasoningEffort::Low => GenaiReasoningEffort::Low,
        ReasoningEffort::Medium => GenaiReasoningEffort::Medium,
        ReasoningEffort::High => GenaiReasoningEffort::High,
        ReasoningEffort::XHigh => GenaiReasoningEffort::XHigh,
        ReasoningEffort::Max => GenaiReasoningEffort::Max,
        ReasoningEffort::Budget(tokens) => GenaiReasoningEffort::Budget(tokens),
    }
}

/// 构建 Provider client；内部会同步加载系统代理与 CA 存储，只能在后台任务中调用。
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

/// 暴露 Provider 到 genai adapter 的映射，供测试校验目录完整性与唯一性。
#[cfg(test)]
pub(super) const fn adapter_kind_for_test(provider: LlmProvider) -> AdapterKind {
    adapter_kind(provider)
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

pub(super) const fn provider_supports_binary_and_tools(provider: LlmProvider) -> bool {
    // genai 0.6.5 的 Cohere adapter 会静默丢弃 Binary 和 tools，必须在本层拒绝降级。
    !matches!(provider, LlmProvider::Cohere)
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
    allow_agent_screenshot: bool,
) -> ChatRequest {
    let messages = messages
        .into_iter()
        .map(|message| match message.role {
            ChatRole::User => match message.image.as_ref() {
                Some(image) => match image_content_part(image) {
                    Some(image) => GenaiMessage::user(MessageContent::from_parts(vec![
                        ContentPart::from_text(message.content),
                        image,
                    ])),
                    None => GenaiMessage::user(format!(
                        "{}\n\n[The image from this historical message is no longer available.]",
                        message.content
                    )),
                },
                None => GenaiMessage::user(message.content),
            },
            ChatRole::Assistant => GenaiMessage::assistant(message.content),
        })
        .collect::<Vec<_>>();
    let mut chat_request = ChatRequest::from_messages(messages);
    if !system_prompt.trim().is_empty() {
        chat_request = chat_request.with_system(system_prompt);
    }
    if allow_agent_screenshot {
        chat_request = chat_request.with_tools([screen_capture_tool()]);
    }
    chat_request
}

fn screen_capture_tool() -> Tool {
    Tool::new(SCREEN_CAPTURE_TOOL)
        .with_description(
            "Capture the user's screen as a still image when current visual context is necessary to answer. Do not call it speculatively or more than once.",
        )
        .with_schema(json!({
            "type": "object",
            "properties": {},
            "required": [],
            "additionalProperties": false
        }))
}

fn image_content_part(image: &ImageAttachment) -> Option<ContentPart> {
    let bytes = image.bytes()?;
    Some(ContentPart::from_binary_base64(
        "image/jpeg",
        STANDARD.encode(bytes),
        Some(image.name().to_owned()),
    ))
}

pub(super) fn append_stateless_capture_result(
    request: &mut ChatRequest,
    image: Option<&ImageAttachment>,
) {
    let mut parts = vec![ContentPart::from_text(if image.is_some() {
        format!("\n\n{CAPTURE_HANDOFF_PROMPT}")
    } else {
        "\n\nThe requested capture_screen tool could not provide a screenshot. Continue without it."
            .to_owned()
    })];
    if let Some(part) = image.and_then(image_content_part) {
        parts.push(part);
    }
    if let Some(message) = request.messages.last_mut() {
        message.content.extend(parts);
    } else {
        request
            .messages
            .push(GenaiMessage::user(MessageContent::from_parts(parts)));
    }
}

async fn stream_once(
    client: &Client,
    attempt: &StreamAttempt,
    events: &mut mpsc::Sender<ChatStreamEvent>,
) -> Result<StreamOutcome, AttemptFailure> {
    let StreamAttempt {
        model,
        request,
        options: base_options,
        total_deadline,
        screenshot_permission_revision,
        capture_tool_handoff,
    } = attempt;
    let (total_deadline, screenshot_permission_revision, capture_tool_handoff) = (
        *total_deadline,
        *screenshot_permission_revision,
        *capture_tool_handoff,
    );
    let (model, request) = (model.clone(), request.clone());
    if screenshot_permission_revision
        .is_some_and(|revision| !CONFIG.agent_screenshot_permission_is_current(revision))
    {
        return Err(screenshot_permission_revoked_failure());
    }
    let options = match (base_options.clone(), capture_tool_handoff) {
        (options, false) => options,
        // 截图 handoff 需要拿回已捕获的正文、思考与工具调用，用户高级参数保持不变。
        (options, true) => Some(
            options
                .unwrap_or_default()
                .with_capture_content(true)
                .with_capture_reasoning_content(true)
                .with_capture_tool_calls(true),
        ),
    };
    let first_content_deadline = (Instant::now() + FIRST_CONTENT_TIMEOUT).min(total_deadline);
    let response_result = if let Some(revision) = screenshot_permission_revision {
        tokio::select! {
            biased;
            () = wait_for_screenshot_permission_revocation(revision) => {
                return Err(screenshot_permission_revoked_failure());
            }
            result = timeout_at(
                first_content_deadline,
                client.exec_chat_stream(model, request, options.as_ref()),
            ) => result,
        }
    } else {
        timeout_at(
            first_content_deadline,
            client.exec_chat_stream(model, request, options.as_ref()),
        )
        .await
    };
    let response = match response_result {
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
    if let Some(revision) = screenshot_permission_revision {
        tokio::select! {
            biased;
            () = wait_for_screenshot_permission_revocation(revision) => {
                Err(screenshot_permission_revoked_failure())
            }
            result = consume_stream(
                response.stream,
                first_content_deadline,
                STREAM_IDLE_TIMEOUT,
                total_deadline,
                events,
            ) => result,
        }
    } else {
        consume_stream(
            response.stream,
            first_content_deadline,
            STREAM_IDLE_TIMEOUT,
            total_deadline,
            events,
        )
        .await
    }
}

pub(super) async fn consume_stream<S>(
    mut stream: S,
    first_content_deadline: Instant,
    idle_timeout: Duration,
    total_deadline: Instant,
    events: &mut mpsc::Sender<ChatStreamEvent>,
) -> Result<StreamOutcome, AttemptFailure>
where
    S: futures::Stream<Item = Result<GenaiStreamEvent, genai::Error>> + Unpin,
{
    let mut response_started = false;
    let mut produced_content = false;
    let mut pending = String::new();
    let mut flush_deadline = None;
    let mut idle_deadline = None;
    let mut visible_bytes = 0_usize;
    let mut hidden_bytes = 0_usize;
    let mut captured_reasoning = String::new();
    let mut captured_thought_signature = String::new();
    let mut observed_tool_calls = HashSet::new();

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
                return Ok(StreamOutcome::Complete);
            }
            return Err(AttemptFailure {
                message: t!("chat.error.first_content_timeout").to_string(),
                retryable: true,
                response_started: false,
            });
        }
        if idle_deadline.is_some_and(|deadline| now >= deadline) {
            if !flush(&mut pending, events, total_deadline).await {
                return Ok(StreamOutcome::Complete);
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
                    if !reserve_bounded_bytes(
                        &mut visible_bytes,
                        chunk.content.len(),
                        MAX_STREAM_CONTENT_BYTES,
                    ) {
                        let _ = flush(&mut pending, events, total_deadline).await;
                        return Err(invalid_captured_response_failure(response_started));
                    }
                    if pending.is_empty() {
                        flush_deadline = Some(Instant::now() + FLUSH_INTERVAL);
                    }
                    pending.push_str(&chunk.content);
                    if pending.len() >= FLUSH_BYTES
                        && !flush(&mut pending, events, total_deadline).await
                    {
                        return Ok(StreamOutcome::Complete);
                    }
                    if pending.is_empty() {
                        flush_deadline = None;
                    }
                }
            }
            Ok(Some(Ok(GenaiStreamEvent::End(end)))) => {
                let delivery_deadline = terminal_delivery_deadline(total_deadline);
                if !flush(&mut pending, events, delivery_deadline).await {
                    return Ok(StreamOutcome::Complete);
                }
                if end
                    .captured_reasoning_content
                    .as_ref()
                    .is_some_and(|reasoning| reasoning.len() > MAX_HANDOFF_CONTENT_BYTES)
                    || end.captured_content.as_ref().is_some_and(|content| {
                        !message_content_is_bounded(
                            content,
                            MAX_STREAM_CONTENT_BYTES + MAX_HANDOFF_CONTENT_BYTES,
                        )
                    })
                {
                    return Err(invalid_captured_response_failure(response_started));
                }
                let mut calls = Vec::new();
                if let Some(content) = end.captured_content.as_ref() {
                    for part in content.parts() {
                        let Some(call) = part.as_tool_call() else {
                            continue;
                        };
                        if calls.len() >= MAX_TOOL_CALLS || call.size() > MAX_HANDOFF_CONTENT_BYTES
                        {
                            return Err(invalid_captured_response_failure(response_started));
                        }
                        calls.push(call.clone());
                    }
                }
                if !calls.is_empty() {
                    let reasoning = end
                        .captured_reasoning_content
                        .filter(|reasoning| !reasoning.is_empty())
                        .or_else(|| (!captured_reasoning.is_empty()).then_some(captured_reasoning));
                    let mut content = end
                        .captured_content
                        .unwrap_or_else(|| MessageContent::from_tool_calls(calls.clone()));
                    if content.thought_signatures().is_empty()
                        && !captured_thought_signature.is_empty()
                    {
                        content.prepend(ContentPart::ThoughtSignature(
                            captured_thought_signature.clone(),
                        ));
                        if let Some(first_call) = calls.first_mut() {
                            first_call.thought_signatures = Some(vec![captured_thought_signature]);
                        }
                    }
                    return Ok(StreamOutcome::ToolUse {
                        assistant_message: GenaiMessage::assistant(content)
                            .with_reasoning_content(reasoning),
                        calls,
                    });
                }
                if !produced_content {
                    return Err(AttemptFailure {
                        message: t!("chat.error.empty_response").to_string(),
                        retryable: false,
                        response_started,
                    });
                }
                return Ok(StreamOutcome::Complete);
            }
            Ok(Some(Ok(GenaiStreamEvent::ReasoningChunk(chunk)))) => {
                if !reserve_bounded_bytes(
                    &mut hidden_bytes,
                    chunk.content.len(),
                    MAX_HANDOFF_CONTENT_BYTES,
                ) {
                    return Err(invalid_captured_response_failure(response_started));
                }
                captured_reasoning.push_str(&chunk.content);
                if response_started {
                    idle_deadline = Some(Instant::now() + idle_timeout);
                }
            }
            Ok(Some(Ok(GenaiStreamEvent::ThoughtSignatureChunk(chunk)))) => {
                if !reserve_bounded_bytes(
                    &mut hidden_bytes,
                    chunk.content.len(),
                    MAX_HANDOFF_CONTENT_BYTES,
                ) {
                    return Err(invalid_captured_response_failure(response_started));
                }
                captured_thought_signature.push_str(&chunk.content);
                if response_started {
                    idle_deadline = Some(Instant::now() + idle_timeout);
                }
            }
            Ok(Some(Ok(GenaiStreamEvent::ToolCallChunk(chunk)))) => {
                response_started = true;
                idle_deadline = Some(Instant::now() + idle_timeout);
                let call = chunk.tool_call;
                if !reserve_bounded_bytes(&mut hidden_bytes, call.size(), MAX_HANDOFF_CONTENT_BYTES)
                {
                    return Err(invalid_captured_response_failure(response_started));
                }
                observed_tool_calls.insert((call.call_id, call.fn_name));
                if observed_tool_calls.len() > MAX_TOOL_CALLS {
                    return Err(invalid_captured_response_failure(response_started));
                }
            }
            Ok(Some(Ok(GenaiStreamEvent::Start))) => {
                if response_started {
                    idle_deadline = Some(Instant::now() + idle_timeout);
                }
            }
            Ok(Some(Err(error))) => {
                if !flush(&mut pending, events, total_deadline).await {
                    return Ok(StreamOutcome::Complete);
                }
                let mut failure = attempt_failure(error);
                failure.response_started = response_started;
                return Err(failure);
            }
            Ok(None) => {
                if !flush(&mut pending, events, total_deadline).await {
                    return Ok(StreamOutcome::Complete);
                }
                return Err(AttemptFailure {
                    message: t!("chat.error.stream_ended").to_string(),
                    retryable: !response_started,
                    response_started,
                });
            }
            Err(_) => {
                if !pending.is_empty() && !flush(&mut pending, events, total_deadline).await {
                    return Ok(StreamOutcome::Complete);
                }
                flush_deadline = None;
            }
        }
    }
}

async fn wait_for_screenshot_permission_revocation(revision: u64) {
    let mut revisions = CONFIG.subscribe_agent_screenshot_permission_revision();
    loop {
        if *revisions.borrow() != revision
            || !CONFIG.agent_screenshot_permission_is_current(revision)
        {
            return;
        }
        if revisions.changed().await.is_err() {
            return;
        }
    }
}

fn screenshot_permission_revoked_failure() -> AttemptFailure {
    AttemptFailure {
        message: t!("chat.error.screen_permission_revoked").to_string(),
        retryable: false,
        response_started: false,
    }
}

fn invalid_captured_response_failure(response_started: bool) -> AttemptFailure {
    AttemptFailure {
        message: t!("chat.error.invalid_response").to_string(),
        retryable: false,
        response_started,
    }
}

fn reserve_bounded_bytes(total: &mut usize, additional: usize, maximum: usize) -> bool {
    let Some(next) = total.checked_add(additional) else {
        return false;
    };
    if next > maximum {
        return false;
    }
    *total = next;
    true
}

fn message_content_is_bounded(content: &MessageContent, maximum: usize) -> bool {
    let mut total = 0_usize;
    content
        .parts()
        .iter()
        .all(|part| reserve_bounded_bytes(&mut total, part.size(), maximum))
}

struct ToolContinuation {
    responses: Vec<ToolResponse>,
    image: Option<ImageAttachment>,
}

impl ToolContinuation {
    fn revoke_image(&mut self) {
        if self.image.take().is_none() {
            return;
        }
        for response in &mut self.responses {
            if response.fn_name.as_deref() == Some(SCREEN_CAPTURE_TOOL) {
                response.content =
                    json!({"status": "error", "code": "permission_revoked"}).to_string();
            }
        }
    }
}

async fn execute_tool_calls(
    calls: &[ToolCall],
    total_deadline: Instant,
    permission_revision: Option<u64>,
) -> ToolContinuation {
    let mut responses = Vec::with_capacity(calls.len());
    let mut image = None;
    for call in calls {
        let content = if call.fn_name != SCREEN_CAPTURE_TOOL {
            json!({"status": "error", "code": "unknown_tool"})
        } else if !tool_arguments_are_empty(&call.fn_arguments) {
            json!({"status": "error", "code": "invalid_arguments"})
        } else if !permission_revision
            .is_some_and(|revision| CONFIG.agent_screenshot_permission_is_current(revision))
        {
            json!({"status": "error", "code": "permission_disabled"})
        } else if image.is_some() {
            json!({"status": "error", "code": "screen_already_captured"})
        } else {
            match permission_revision
                .filter(|revision| CONFIG.agent_screenshot_permission_is_current(*revision))
            {
                Some(revision) => match capture_screen(total_deadline, revision).await {
                    Ok(captured) if CONFIG.agent_screenshot_permission_is_current(revision) => {
                        let content = json!({
                            "status": "ok",
                            "image": "attached_in_next_user_message",
                            "width": captured.width(),
                            "height": captured.height()
                        });
                        image = Some(captured);
                        content
                    }
                    Ok(_) => json!({"status": "error", "code": "permission_revoked"}),
                    Err(code) => json!({"status": "error", "code": code}),
                },
                None => json!({"status": "error", "code": "permission_disabled"}),
            }
        };
        responses.push(ToolResponse::from_tool_call(call, content.to_string()));
    }
    ToolContinuation { responses, image }
}

fn tool_arguments_are_empty(arguments: &serde_json::Value) -> bool {
    arguments.is_null() || arguments.as_object().is_some_and(serde_json::Map::is_empty)
}

async fn capture_screen(
    total_deadline: Instant,
    permission_revision: u64,
) -> Result<ImageAttachment, &'static str> {
    let deadline = (Instant::now() + SCREEN_CAPTURE_TIMEOUT).min(total_deadline);
    if deadline <= Instant::now() {
        return Err("capture_timeout");
    }
    #[cfg(target_os = "linux")]
    let capture = tokio::spawn(capture_primary_screen());
    #[cfg(not(target_os = "linux"))]
    let capture = capture_primary_screen();
    tokio::pin!(capture);
    tokio::select! {
        biased;
        () = wait_for_screenshot_permission_revocation(permission_revision) => {
            Err("permission_revoked")
        }
        result = timeout_at(deadline, &mut capture) => capture_result(result),
    }
}

#[cfg(target_os = "linux")]
fn capture_result(
    result: Result<
        Result<Result<ImageAttachment, super::media::ImageInputError>, tokio::task::JoinError>,
        tokio::time::error::Elapsed,
    >,
) -> Result<ImageAttachment, &'static str> {
    // ashpd 0.13 在 portal 响应前不返回 Request；超时或撤权时让任务继续接收响应并删除临时文件。
    match result {
        Ok(Ok(Ok(image))) => Ok(image),
        Ok(Ok(Err(_))) => Err("capture_failed_or_permission_denied"),
        Ok(Err(_)) => Err("capture_worker_failed"),
        Err(_) => Err("capture_timeout"),
    }
}

#[cfg(not(target_os = "linux"))]
fn capture_result(
    result: Result<
        Result<ImageAttachment, super::media::ImageInputError>,
        tokio::time::error::Elapsed,
    >,
) -> Result<ImageAttachment, &'static str> {
    match result {
        Ok(Ok(image)) => Ok(image),
        Ok(Err(_)) => Err("capture_failed_or_permission_denied"),
        Err(_) => Err("capture_timeout"),
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
