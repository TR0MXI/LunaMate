//! 将 Provider 无关的会话快照转换为 `genai` 流，并输出受限批次事件。

use std::{collections::HashSet, future::Future, pin::Pin, sync::Arc, time::Duration};

use base64::{Engine as _, engine::general_purpose::STANDARD};
use futures::{SinkExt as _, StreamExt as _, channel::mpsc};
use genai::{
    Client, ModelIden, WebConfig,
    chat::{
        ChatMessage as GenaiMessage, ChatOptions, ChatRequest, ChatStreamEvent as GenaiStreamEvent,
        ContentPart, MessageContent, Tool, ToolCall, ToolResponse,
    },
    resolver::{AuthData, Endpoint},
};
use rust_i18n::t;
use serde_json::json;
use tokio::time::{Instant, sleep, timeout_at};

use crate::{
    config::{AppLanguage, LlmAdvancedOptions, LlmModelConfig, LlmProvider, llm_provider_id},
    media::{ImageAttachment, ImageInputError},
    memory::{AssistantTrace, ToolExecutionTrace},
    session::{ChatContextMessage, ChatRole},
    tools::{AgentOutfitRequest, AgentOutfitResult, OutfitOption},
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
const MAX_OUTFIT_TOOL_OPTIONS: usize = 128;
const MAX_OUTFIT_NAME_BYTES: usize = 512;
pub(super) const FLUSH_BYTES: usize = 512;
const RETRY_DELAYS: [Duration; 2] = [Duration::from_millis(500), Duration::from_millis(1_500)];
pub(super) const SCREEN_CAPTURE_TOOL: &str = "capture_screen";
pub(super) const CHANGE_OUTFIT_TOOL: &str = "change_outfit";

/// 宿主提供的单次截图授权与捕获能力。
pub trait ScreenshotCapability: Send + Sync {
    fn is_authorized(&self) -> bool;

    fn wait_for_revocation(&self) -> Pin<Box<dyn Future<Output = ()> + Send + 'static>>;

    fn capture(
        &self,
    ) -> Pin<Box<dyn Future<Output = Result<ImageAttachment, ImageInputError>> + Send + 'static>>;
}

/// 网络任务发送给聊天实体的有界事件。
pub(crate) enum ChatStreamEvent {
    Delta(String),
    ChangeOutfit(AgentOutfitRequest),
    Trace(AssistantTrace),
    Finished,
    Failed(String),
}

/// 一次请求所需的不可变模型、提示词和上下文快照。
pub(crate) struct ChatServiceRequest {
    pub(crate) model: ModelIden,
    pub(crate) options: Option<ChatOptions>,
    pub(crate) system_prompt: String,
    pub(crate) messages: Vec<ChatContextMessage>,
    pub(crate) screenshot_capability: Option<Arc<dyn ScreenshotCapability>>,
    pub(crate) outfits: Vec<OutfitOption>,
    pub(crate) outfit_revision: u64,
    pub(crate) language: AppLanguage,
}

/// 根据当前模型连接配置构造可并发复用的 `genai` Client。
///
/// Client 本身是不可变的 `Send + Sync` 句柄；调用方需要热更新连接配置时应构造新实例并
/// 替换自己的运行时快照，而不是在网络请求期间持锁。
pub fn client_from_model(model: &LlmModelConfig) -> Client {
    build_client(model)
}

/// 把宿主模型配置解析为 Agent 直接持有的模型标识和单次请求选项。
pub fn model_and_options_from_config(model: &LlmModelConfig) -> (ModelIden, Option<ChatOptions>) {
    (
        ModelIden::new(model.provider, model.model.clone()),
        base_chat_options(&model.advanced),
    )
}

/// 使用调用方持有的 Client 执行一次完整流式请求。
///
/// 该入口供核心 [`crate::Agent`] 直接组合 Client，避免再通过只负责缓存 Client 的后端包装。
pub(crate) async fn stream_with_client(
    client: Client,
    request: ChatServiceRequest,
    events: mpsc::Sender<ChatStreamEvent>,
) {
    stream_chat(request, events, client).await;
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
    Complete {
        reasoning: Option<String>,
    },
    ToolUse {
        assistant_message: GenaiMessage,
        calls: Vec<ToolCall>,
        reasoning: Option<String>,
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
        options,
        system_prompt,
        messages,
        screenshot_capability,
        outfits,
        outfit_revision,
        language,
    } = request;
    let total_deadline = Instant::now() + TOTAL_RESPONSE_TIMEOUT;
    if !provider_supports_binary_and_tools(model.adapter_kind)
        && messages.iter().any(|message| {
            message
                .image
                .as_ref()
                .and_then(ImageAttachment::bytes)
                .is_some()
        })
    {
        log::warn!(
            "Agent 请求使用了 Provider 不支持的图片能力：provider={}",
            llm_provider_id(model.adapter_kind)
        );
        let _ = send_terminal_event(
            &mut events,
            ChatStreamEvent::Failed(
                t!(
                    "chat.error.provider_image_unsupported",
                    locale = language.id()
                )
                .to_string(),
            ),
            total_deadline,
        )
        .await;
        return;
    }
    let screenshot_capability =
        screenshot_capability.filter(|capability| capability.is_authorized());
    let register_screenshot_tool =
        screenshot_capability.is_some() && provider_supports_binary_and_tools(model.adapter_kind);
    let outfits = bounded_outfits(outfits);
    let registered_outfits = outfit_tool_options(model.adapter_kind, &outfits);
    let register_outfit_tool = !registered_outfits.is_empty();
    let register_any_tool = register_screenshot_tool || register_outfit_tool;
    let base_options = options;
    let mut chat_request = build_request(
        system_prompt,
        messages,
        register_screenshot_tool,
        registered_outfits,
        language,
    );
    let mut used_screen_capture = false;
    let mut used_tools = false;
    let mut trace_reasoning = String::new();
    let mut tool_executions = Vec::new();
    loop {
        let required_screenshot = ((!used_tools && register_screenshot_tool)
            || used_screen_capture)
            .then(|| screenshot_capability.clone())
            .flatten();
        let capture_tool_handoff = register_any_tool || used_tools;
        let attempt = StreamAttempt {
            model: model.clone(),
            request: chat_request.clone(),
            options: base_options.clone(),
            total_deadline,
            screenshot_capability: required_screenshot,
            capture_tool_handoff,
            language,
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
            StreamOutcome::Complete { reasoning } => {
                append_trace_reasoning(&mut trace_reasoning, reasoning);
                let trace = AssistantTrace::new(
                    (!trace_reasoning.is_empty()).then_some(trace_reasoning),
                    tool_executions,
                );
                send_completion_events(&mut events, trace, total_deadline).await;
                return;
            }
            StreamOutcome::ToolUse {
                assistant_message,
                calls,
                reasoning,
            } => {
                append_trace_reasoning(&mut trace_reasoning, reasoning);
                if used_tools {
                    log::warn!("Agent 工具循环超过单轮上限，已拒绝继续执行");
                    let _ = send_terminal_event(
                        &mut events,
                        ChatStreamEvent::Failed(
                            t!("chat.error.tool_loop", locale = language.id()).to_string(),
                        ),
                        total_deadline,
                    )
                    .await;
                    return;
                }
                let mut continuation = execute_tool_calls(
                    &calls,
                    total_deadline,
                    screenshot_capability.as_ref(),
                    &outfits,
                    outfit_revision,
                    &mut events,
                )
                .await;
                if !screenshot_capability
                    .as_ref()
                    .is_some_and(|capability| capability.is_authorized())
                {
                    continuation.revoke_image();
                }
                tool_executions.extend(tool_execution_traces(&calls, &continuation.trace_results));
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
                                ContentPart::from_text(screen_capture_handoff_prompt(language)),
                                part,
                            ])));
                    }
                } else {
                    // genai 0.6.5 的部分适配器会接收签名流却无法在请求中回写；把本地结果并入原用户轮次可安全重试而不伪造 handoff。
                    append_stateless_tool_results(
                        &mut chat_request,
                        &continuation.stateless_results,
                        language,
                    );
                    if continuation.screen_capture_requested {
                        append_stateless_capture_result(
                            &mut chat_request,
                            continuation.image.as_ref(),
                            language,
                        );
                    }
                }
                chat_request.tools = None;
                used_screen_capture = continuation.screen_capture_requested;
                used_tools = true;
            }
        }
    }
}

/// 一次流式尝试的全部不变输入。
///
/// 重试与截图工具循环都会重复使用同一份能力快照，集中保存可以避免参数错位。
struct StreamAttempt {
    model: ModelIden,
    request: ChatRequest,
    options: Option<ChatOptions>,
    total_deadline: Instant,
    screenshot_capability: Option<Arc<dyn ScreenshotCapability>>,
    capture_tool_handoff: bool,
    language: AppLanguage,
}

async fn stream_with_retry(
    client: &Client,
    attempt: &StreamAttempt,
    events: &mut mpsc::Sender<ChatStreamEvent>,
) -> Result<StreamOutcome, AttemptFailure> {
    let mut retry_delays = RETRY_DELAYS.into_iter();
    let mut retry_attempt = 0_u8;
    let total_deadline = attempt.total_deadline;
    loop {
        if attempt
            .screenshot_capability
            .as_ref()
            .is_some_and(|capability| !capability.is_authorized())
        {
            return Err(screenshot_permission_revoked_failure(attempt.language));
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
                retry_attempt = retry_attempt.saturating_add(1);
                log::debug!(
                    "Provider 请求将在退避后重试：attempt={retry_attempt}, delay_ms={}, remaining_ms={}",
                    delay.as_millis(),
                    remaining.as_millis()
                );
                sleep(delay).await;
            }
            Err(failure) => return Err(failure),
        }
    }
}

/// 把供应商高级参数翻译为 `genai` 请求选项；全部未设置时返回 `None` 以沿用 Provider 默认值。
pub(super) fn base_chat_options(advanced: &LlmAdvancedOptions) -> Option<ChatOptions> {
    if advanced.reasoning_effort.is_none()
        && advanced.max_output_tokens.is_none()
        && advanced.temperature.is_none()
        && advanced.top_p.is_none()
    {
        return None;
    }

    let mut options = ChatOptions::default();
    if let Some(effort) = advanced.reasoning_effort.clone() {
        options = options.with_reasoning_effort(effort);
    }
    if let Some(tokens) = advanced.max_output_tokens {
        options = options.with_max_tokens(tokens);
    }
    if let Some(temperature) = advanced.temperature {
        options = options.with_temperature(temperature);
    }
    if let Some(top_p) = advanced.top_p {
        options = options.with_top_p(top_p);
    }
    Some(options)
}

/// 构建 Provider client；内部会同步加载系统代理与 CA 存储，只能在后台任务中调用。
fn build_client(model: &LlmModelConfig) -> Client {
    let auth = auth_data(model);
    let mut builder = Client::builder()
        .with_adapter_kind(model.provider)
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

pub(super) const fn provider_supports_binary_and_tools(provider: LlmProvider) -> bool {
    // genai 0.6.5 的 Cohere adapter 会静默丢弃 Binary 和 tools，必须在本层拒绝降级。
    !matches!(provider, LlmProvider::Cohere)
}

pub(super) fn outfit_tool_options(
    provider: LlmProvider,
    outfits: &[OutfitOption],
) -> &[OutfitOption] {
    if outfits.len() > 1 && provider_supports_binary_and_tools(provider) {
        outfits
    } else {
        &[]
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
    allow_agent_screenshot: bool,
    outfits: &[OutfitOption],
    language: AppLanguage,
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
                        "{}\n\n[{}]",
                        message.content,
                        t!("chat.history_image_unavailable", locale = language.id())
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
    let mut tools = Vec::with_capacity(2);
    if allow_agent_screenshot {
        tools.push(screen_capture_tool(language));
    }
    if outfits.len() > 1 {
        tools.push(change_outfit_tool(outfits, language));
    }
    if !tools.is_empty() {
        chat_request = chat_request.with_tools(tools);
    }
    chat_request
}

fn screen_capture_tool(language: AppLanguage) -> Tool {
    Tool::new(SCREEN_CAPTURE_TOOL)
        .with_description(
            t!(
                "chat.tool.screen_capture_description",
                locale = language.id()
            )
            .to_string(),
        )
        .with_schema(json!({
            "type": "object",
            "properties": {},
            "required": [],
            "additionalProperties": false
        }))
}

fn change_outfit_tool(outfits: &[OutfitOption], language: AppLanguage) -> Tool {
    let labels = outfits.iter().map(OutfitOption::label).collect::<Vec<_>>();
    Tool::new(CHANGE_OUTFIT_TOOL)
        .with_description(
            t!(
                "chat.tool.change_outfit_description",
                locale = language.id()
            )
            .to_string(),
        )
        .with_schema(json!({
            "type": "object",
            "properties": {
                "outfit": {
                    "type": "string",
                    "description": t!(
                        "chat.tool.change_outfit_argument",
                        locale = language.id()
                    ).to_string(),
                    "enum": labels
                }
            },
            "required": ["outfit"],
            "additionalProperties": false
        }))
}

fn bounded_outfits(outfits: Vec<OutfitOption>) -> Vec<OutfitOption> {
    let mut ids = HashSet::new();
    let mut labels = HashSet::new();
    outfits
        .into_iter()
        .filter(|outfit| {
            !outfit.id().is_empty()
                && outfit.id().len() <= MAX_OUTFIT_NAME_BYTES
                && !outfit.label().is_empty()
                && outfit.label().len() <= MAX_OUTFIT_NAME_BYTES
                && ids.insert(outfit.id().to_owned())
                && labels.insert(outfit.label().to_owned())
        })
        .take(MAX_OUTFIT_TOOL_OPTIONS)
        .collect()
}

fn screen_capture_handoff_prompt(language: AppLanguage) -> String {
    t!("chat.tool.screen_capture_handoff", locale = language.id()).to_string()
}

fn image_content_part(image: &ImageAttachment) -> Option<ContentPart> {
    let bytes = image.bytes()?;
    Some(ContentPart::from_binary_base64(
        "image/jpeg",
        STANDARD.encode(bytes),
        Some("image.jpg".to_owned()),
    ))
}

pub(super) fn append_stateless_capture_result(
    request: &mut ChatRequest,
    image: Option<&ImageAttachment>,
    language: AppLanguage,
) {
    let prompt = if image.is_some() {
        screen_capture_handoff_prompt(language)
    } else {
        t!(
            "chat.tool.screen_capture_unavailable",
            locale = language.id()
        )
        .to_string()
    };
    let mut parts = vec![ContentPart::from_text(format!("\n\n{prompt}"))];
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

fn append_stateless_tool_results(
    request: &mut ChatRequest,
    results: &[String],
    language: AppLanguage,
) {
    if results.is_empty() {
        return;
    }
    let prompt = t!(
        "chat.tool.result_handoff",
        locale = language.id(),
        results = results.join("\n")
    )
    .to_string();
    let part = ContentPart::from_text(format!("\n\n{prompt}"));
    if let Some(message) = request.messages.last_mut() {
        message.content.extend(vec![part]);
    } else {
        request
            .messages
            .push(GenaiMessage::user(MessageContent::from_parts(vec![part])));
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
        screenshot_capability,
        capture_tool_handoff,
        language,
    } = attempt;
    let (total_deadline, screenshot_capability, capture_tool_handoff, language) = (
        *total_deadline,
        screenshot_capability.clone(),
        *capture_tool_handoff,
        *language,
    );
    let (model, request) = (model.clone(), request.clone());
    if screenshot_capability
        .as_ref()
        .is_some_and(|capability| !capability.is_authorized())
    {
        return Err(screenshot_permission_revoked_failure(language));
    }
    // 可读推理对普通回复和工具轮次都属于展示数据，因此每次流都要求 genai 在终态汇总；
    // 工具 handoff 仍额外捕获正文与调用协议，二者不会进入持久化详情。
    let mut options = base_options
        .clone()
        .unwrap_or_default()
        .with_capture_reasoning_content(true);
    if capture_tool_handoff {
        options = options
            .with_capture_content(true)
            .with_capture_tool_calls(true);
    }
    let options = Some(options);
    let first_content_deadline = (Instant::now() + FIRST_CONTENT_TIMEOUT).min(total_deadline);
    let response_result = if let Some(capability) = screenshot_capability.as_ref() {
        tokio::select! {
            biased;
            () = capability.wait_for_revocation() => {
                return Err(screenshot_permission_revoked_failure(language));
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
        Ok(Err(error)) => return Err(attempt_failure(error, language)),
        Err(_) => {
            return Err(AttemptFailure {
                message: t!("chat.error.first_content_timeout", locale = language.id()).to_string(),
                retryable: true,
                response_started: false,
            });
        }
    };
    if let Some(capability) = screenshot_capability.as_ref() {
        tokio::select! {
            biased;
            () = capability.wait_for_revocation() => {
                Err(screenshot_permission_revoked_failure(language))
            }
            result = consume_stream(
                response.stream,
                first_content_deadline,
                STREAM_IDLE_TIMEOUT,
                total_deadline,
                events,
                language,
            ) => result,
        }
    } else {
        consume_stream(
            response.stream,
            first_content_deadline,
            STREAM_IDLE_TIMEOUT,
            total_deadline,
            events,
            language,
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
    language: AppLanguage,
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
                message: t!("chat.error.total_timeout", locale = language.id()).to_string(),
                retryable: false,
                response_started,
            });
        }
        if !response_started && now >= first_content_deadline {
            if !flush(&mut pending, events, total_deadline).await {
                return Ok(complete_stream(captured_reasoning));
            }
            return Err(AttemptFailure {
                message: t!("chat.error.first_content_timeout", locale = language.id()).to_string(),
                retryable: true,
                response_started: false,
            });
        }
        if idle_deadline.is_some_and(|deadline| now >= deadline) {
            if !flush(&mut pending, events, total_deadline).await {
                return Ok(complete_stream(captured_reasoning));
            }
            return Err(AttemptFailure {
                message: t!("chat.error.idle_timeout", locale = language.id()).to_string(),
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
                        return Err(invalid_captured_response_failure(
                            response_started,
                            language,
                        ));
                    }
                    if pending.is_empty() {
                        flush_deadline = Some(Instant::now() + FLUSH_INTERVAL);
                    }
                    pending.push_str(&chunk.content);
                    if pending.len() >= FLUSH_BYTES
                        && !flush(&mut pending, events, total_deadline).await
                    {
                        return Ok(complete_stream(captured_reasoning));
                    }
                    if pending.is_empty() {
                        flush_deadline = None;
                    }
                }
            }
            Ok(Some(Ok(GenaiStreamEvent::End(end)))) => {
                let delivery_deadline = terminal_delivery_deadline(total_deadline);
                if !flush(&mut pending, events, delivery_deadline).await {
                    return Ok(complete_stream(captured_reasoning));
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
                    return Err(invalid_captured_response_failure(
                        response_started,
                        language,
                    ));
                }
                let mut calls = Vec::new();
                if let Some(content) = end.captured_content.as_ref() {
                    for part in content.parts() {
                        let Some(call) = part.as_tool_call() else {
                            continue;
                        };
                        if calls.len() >= MAX_TOOL_CALLS || call.size() > MAX_HANDOFF_CONTENT_BYTES
                        {
                            return Err(invalid_captured_response_failure(
                                response_started,
                                language,
                            ));
                        }
                        calls.push(call.clone());
                    }
                }
                let reasoning =
                    readable_reasoning(end.captured_reasoning_content, captured_reasoning);
                if !calls.is_empty() {
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
                            .with_reasoning_content(reasoning.clone()),
                        calls,
                        reasoning,
                    });
                }
                if !produced_content {
                    return Err(AttemptFailure {
                        message: t!("chat.error.empty_response", locale = language.id())
                            .to_string(),
                        retryable: false,
                        response_started,
                    });
                }
                return Ok(StreamOutcome::Complete { reasoning });
            }
            Ok(Some(Ok(GenaiStreamEvent::ReasoningChunk(chunk)))) => {
                if !reserve_bounded_bytes(
                    &mut hidden_bytes,
                    chunk.content.len(),
                    MAX_HANDOFF_CONTENT_BYTES,
                ) {
                    return Err(invalid_captured_response_failure(
                        response_started,
                        language,
                    ));
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
                    return Err(invalid_captured_response_failure(
                        response_started,
                        language,
                    ));
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
                    return Err(invalid_captured_response_failure(
                        response_started,
                        language,
                    ));
                }
                observed_tool_calls.insert((call.call_id, call.fn_name));
                if observed_tool_calls.len() > MAX_TOOL_CALLS {
                    return Err(invalid_captured_response_failure(
                        response_started,
                        language,
                    ));
                }
            }
            Ok(Some(Ok(GenaiStreamEvent::Start))) => {
                if response_started {
                    idle_deadline = Some(Instant::now() + idle_timeout);
                }
            }
            Ok(Some(Err(error))) => {
                if !flush(&mut pending, events, total_deadline).await {
                    return Ok(complete_stream(captured_reasoning));
                }
                let mut failure = attempt_failure(error, language);
                failure.response_started = response_started;
                return Err(failure);
            }
            Ok(None) => {
                if !flush(&mut pending, events, total_deadline).await {
                    return Ok(complete_stream(captured_reasoning));
                }
                return Err(AttemptFailure {
                    message: t!("chat.error.stream_ended", locale = language.id()).to_string(),
                    retryable: !response_started,
                    response_started,
                });
            }
            Err(_) => {
                if !pending.is_empty() && !flush(&mut pending, events, total_deadline).await {
                    return Ok(complete_stream(captured_reasoning));
                }
                flush_deadline = None;
            }
        }
    }
}

fn readable_reasoning(terminal: Option<String>, streamed: String) -> Option<String> {
    terminal
        .filter(|reasoning| !reasoning.trim().is_empty())
        .or_else(|| (!streamed.trim().is_empty()).then_some(streamed))
}

fn complete_stream(streamed_reasoning: String) -> StreamOutcome {
    StreamOutcome::Complete {
        reasoning: readable_reasoning(None, streamed_reasoning),
    }
}

fn append_trace_reasoning(target: &mut String, reasoning: Option<String>) {
    let Some(reasoning) = reasoning.filter(|reasoning| !reasoning.trim().is_empty()) else {
        return;
    };
    if !target.is_empty() {
        target.push_str("\n\n");
    }
    target.push_str(&reasoning);
}

fn screenshot_permission_revoked_failure(language: AppLanguage) -> AttemptFailure {
    AttemptFailure {
        message: t!(
            "chat.error.screen_permission_revoked",
            locale = language.id()
        )
        .to_string(),
        retryable: false,
        response_started: false,
    }
}

fn invalid_captured_response_failure(
    response_started: bool,
    language: AppLanguage,
) -> AttemptFailure {
    AttemptFailure {
        message: t!("chat.error.invalid_response", locale = language.id()).to_string(),
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
    screen_capture_requested: bool,
    stateless_results: Vec<String>,
    trace_results: Vec<serde_json::Value>,
}

impl ToolContinuation {
    fn revoke_image(&mut self) {
        if self.image.take().is_none() {
            return;
        }
        log::info!("Agent 截图在上传前因权限撤销而被丢弃");
        for (index, response) in self.responses.iter_mut().enumerate() {
            if response.fn_name.as_deref() == Some(SCREEN_CAPTURE_TOOL) {
                let content = json!({"status": "error", "code": "permission_revoked"});
                response.content = content.to_string();
                if let Some(result) = self.stateless_results.get_mut(index) {
                    *result = stateless_tool_result(SCREEN_CAPTURE_TOOL, &content);
                }
                if let Some(result) = self.trace_results.get_mut(index) {
                    *result = content;
                }
            }
        }
    }
}

async fn execute_tool_calls(
    calls: &[ToolCall],
    total_deadline: Instant,
    screenshot_capability: Option<&Arc<dyn ScreenshotCapability>>,
    outfits: &[OutfitOption],
    outfit_revision: u64,
    events: &mut mpsc::Sender<ChatStreamEvent>,
) -> ToolContinuation {
    let mut responses = Vec::with_capacity(calls.len());
    let mut image = None;
    let mut screen_capture_requested = false;
    let mut stateless_results = Vec::with_capacity(calls.len());
    let mut trace_results = Vec::with_capacity(calls.len());
    for call in calls {
        let content = match call.fn_name.as_str() {
            SCREEN_CAPTURE_TOOL => {
                screen_capture_requested = true;
                if !tool_arguments_are_empty(&call.fn_arguments) {
                    json!({"status": "error", "code": "invalid_arguments"})
                } else if !screenshot_capability
                    .is_some_and(|capability| capability.is_authorized())
                {
                    json!({"status": "error", "code": "permission_disabled"})
                } else if image.is_some() {
                    json!({"status": "error", "code": "screen_already_captured"})
                } else {
                    match screenshot_capability.filter(|capability| capability.is_authorized()) {
                        Some(capability) => {
                            let capture_started = Instant::now();
                            log::info!("Agent 截图请求已授权执行");
                            match capture_screen(Arc::clone(capability), total_deadline).await {
                                Ok(captured) if capability.is_authorized() => {
                                    log::info!(
                                        "Agent 截图已完成：width={}, height={}, encoded_bytes={}, elapsed_ms={}",
                                        captured.width(),
                                        captured.height(),
                                        captured.bytes().map_or(0, <[u8]>::len),
                                        capture_started.elapsed().as_millis()
                                    );
                                    let content = json!({
                                        "status": "ok",
                                        "image": "attached_in_next_user_message",
                                        "width": captured.width(),
                                        "height": captured.height()
                                    });
                                    image = Some(captured);
                                    content
                                }
                                Ok(_) => {
                                    log::warn!(
                                        "Agent 截图完成后权限已撤销：elapsed_ms={}",
                                        capture_started.elapsed().as_millis()
                                    );
                                    json!({"status": "error", "code": "permission_revoked"})
                                }
                                Err(_code) if !capability.is_authorized() => {
                                    log::warn!(
                                        "Agent 截图失败前权限已撤销：elapsed_ms={}",
                                        capture_started.elapsed().as_millis()
                                    );
                                    json!({"status": "error", "code": "permission_revoked"})
                                }
                                Err(code) => {
                                    log::warn!(
                                        "Agent 截图失败：result={code}, elapsed_ms={}",
                                        capture_started.elapsed().as_millis()
                                    );
                                    json!({"status": "error", "code": code})
                                }
                            }
                        }
                        None => json!({"status": "error", "code": "permission_disabled"}),
                    }
                }
            }
            CHANGE_OUTFIT_TOOL => match outfit_argument(&call.fn_arguments, outfits) {
                Ok(outfit) => match request_outfit_change(
                    outfit.id().to_owned(),
                    outfit_revision,
                    total_deadline,
                    events,
                )
                .await
                {
                    Ok(()) => json!({"status": "ok", "outfit": outfit.label()}),
                    Err(code) => json!({"status": "error", "code": code}),
                },
                Err(code) => json!({"status": "error", "code": code}),
            },
            _ => json!({"status": "error", "code": "unknown_tool"}),
        };
        stateless_results.push(stateless_tool_result(&call.fn_name, &content));
        trace_results.push(content.clone());
        responses.push(ToolResponse::from_tool_call(call, content.to_string()));
    }
    ToolContinuation {
        responses,
        image,
        screen_capture_requested,
        stateless_results,
        trace_results,
    }
}

fn tool_execution_traces(
    calls: &[ToolCall],
    results: &[serde_json::Value],
) -> Vec<ToolExecutionTrace> {
    calls
        .iter()
        .zip(results)
        .map(|(call, result)| {
            ToolExecutionTrace::new(
                call.fn_name.clone(),
                call.fn_arguments.clone(),
                result.clone(),
            )
        })
        .collect()
}

#[cfg(test)]
pub(super) async fn execute_tool_traces_for_test(calls: &[ToolCall]) -> Vec<ToolExecutionTrace> {
    let (mut events, _receiver) = mpsc::channel(1);
    let continuation = execute_tool_calls(
        calls,
        Instant::now() + Duration::from_secs(1),
        None,
        &[],
        0,
        &mut events,
    )
    .await;
    tool_execution_traces(calls, &continuation.trace_results)
}

#[cfg(test)]
pub(super) async fn execute_screenshot_tool_for_test(
    call: ToolCall,
    capability: Option<Arc<dyn ScreenshotCapability>>,
) -> (serde_json::Value, bool) {
    let (mut events, _receiver) = mpsc::channel(1);
    let continuation = execute_tool_calls(
        &[call],
        Instant::now() + Duration::from_secs(1),
        capability.as_ref(),
        &[],
        0,
        &mut events,
    )
    .await;
    (
        continuation
            .trace_results
            .into_iter()
            .next()
            .unwrap_or(serde_json::Value::Null),
        continuation.image.is_some(),
    )
}

pub(super) fn outfit_argument<'a>(
    arguments: &serde_json::Value,
    outfits: &'a [OutfitOption],
) -> Result<&'a OutfitOption, &'static str> {
    let Some(arguments) = arguments.as_object() else {
        return Err("invalid_arguments");
    };
    if arguments.len() != 1 {
        return Err("invalid_arguments");
    }
    let Some(outfit) = arguments.get("outfit").and_then(serde_json::Value::as_str) else {
        return Err("invalid_arguments");
    };
    outfits
        .iter()
        .find(|available| available.label() == outfit)
        .ok_or("outfit_unavailable")
}

pub(super) async fn request_outfit_change(
    outfit: String,
    outfit_revision: u64,
    total_deadline: Instant,
    events: &mut mpsc::Sender<ChatStreamEvent>,
) -> Result<(), &'static str> {
    let (request, result) = AgentOutfitRequest::channel(outfit, outfit_revision);
    log::debug!("Agent 换装工具请求已投递：outfit_revision={outfit_revision}");
    match timeout_at(
        total_deadline,
        events.send(ChatStreamEvent::ChangeOutfit(request)),
    )
    .await
    {
        Ok(Ok(())) => {}
        Ok(Err(_)) => return Err("receiver_unavailable"),
        Err(_) => return Err("change_timeout"),
    }
    match timeout_at(total_deadline, result.recv()).await {
        Ok(Ok(AgentOutfitResult::Applied)) => {
            log::debug!("Agent 换装工具请求已完成：outfit_revision={outfit_revision}");
            Ok(())
        }
        Ok(Ok(AgentOutfitResult::Failed)) => {
            log::warn!("Agent 换装工具请求失败：outfit_revision={outfit_revision}");
            Err("change_failed")
        }
        Ok(Err(_)) => Err("receiver_unavailable"),
        Err(_) => Err("change_timeout"),
    }
}

fn stateless_tool_result(name: &str, content: &serde_json::Value) -> String {
    format!("{name}: {content}")
}

fn tool_arguments_are_empty(arguments: &serde_json::Value) -> bool {
    arguments.is_null() || arguments.as_object().is_some_and(serde_json::Map::is_empty)
}

async fn capture_screen(
    capability: Arc<dyn ScreenshotCapability>,
    total_deadline: Instant,
) -> Result<ImageAttachment, &'static str> {
    let deadline = (Instant::now() + SCREEN_CAPTURE_TIMEOUT).min(total_deadline);
    if deadline <= Instant::now() {
        return Err("capture_timeout");
    }
    // JoinHandle 被丢弃时任务继续运行，确保门户临时文件等平台资源仍能完成清理。
    let capture = tokio::spawn(capability.capture());
    tokio::select! {
        biased;
        () = capability.wait_for_revocation() => {
            Err("permission_revoked")
        }
        result = timeout_at(deadline, capture) => capture_result(result),
    }
}

fn capture_result(
    result: Result<
        Result<Result<ImageAttachment, ImageInputError>, tokio::task::JoinError>,
        tokio::time::error::Elapsed,
    >,
) -> Result<ImageAttachment, &'static str> {
    match result {
        Ok(Ok(Ok(image))) => Ok(image),
        Ok(Ok(Err(_))) => Err("capture_failed_or_permission_denied"),
        Ok(Err(_)) => Err("capture_worker_failed"),
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

pub(super) async fn send_completion_events(
    events: &mut mpsc::Sender<ChatStreamEvent>,
    trace: AssistantTrace,
    total_deadline: Instant,
) {
    if !trace.is_empty()
        && !send_terminal_event(events, ChatStreamEvent::Trace(trace), total_deadline).await
    {
        return;
    }
    let _ = send_terminal_event(events, ChatStreamEvent::Finished, total_deadline).await;
}

fn terminal_delivery_deadline(total_deadline: Instant) -> Instant {
    total_deadline.max(Instant::now()) + TERMINAL_EVENT_GRACE
}

fn attempt_failure(error: genai::Error, language: AppLanguage) -> AttemptFailure {
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
        message: safe_error_message(&error, language),
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
fn safe_error_message(error: &genai::Error, language: AppLanguage) -> String {
    match error {
        genai::Error::RequiresApiKey { .. }
        | genai::Error::NoAuthResolver { .. }
        | genai::Error::NoAuthData { .. } => {
            t!("chat.error.missing_api_key", locale = language.id()).to_string()
        }
        genai::Error::Resolver { .. } | genai::Error::ModelMapperFailed { .. } => {
            t!("chat.error.invalid_provider", locale = language.id()).to_string()
        }
        genai::Error::HttpError { status, .. } => t!(
            "chat.error.http",
            locale = language.id(),
            status = status.as_u16()
        )
        .to_string(),
        genai::Error::WebStream { .. } if http_status(error).is_some() => t!(
            "chat.error.http",
            locale = language.id(),
            status = http_status(error).unwrap_or_default()
        )
        .to_string(),
        genai::Error::AdapterKindMismatch { .. } => {
            t!("chat.error.provider_mismatch", locale = language.id()).to_string()
        }
        genai::Error::MessageRoleNotSupported { .. }
        | genai::Error::MessageContentTypeNotSupported { .. }
        | genai::Error::AdapterNotSupported { .. } => {
            t!("chat.error.unsupported_request", locale = language.id()).to_string()
        }
        genai::Error::WebAdapterCall { .. }
        | genai::Error::WebModelCall { .. }
        | genai::Error::WebStream { .. } => {
            t!("chat.error.connection", locale = language.id()).to_string()
        }
        genai::Error::ChatResponseGeneration { .. }
        | genai::Error::ChatResponse { .. }
        | genai::Error::StreamParse { .. }
        | genai::Error::NoChatResponse { .. } => {
            t!("chat.error.invalid_response", locale = language.id()).to_string()
        }
        _ => t!("chat.error.request_failed", locale = language.id()).to_string(),
    }
}
