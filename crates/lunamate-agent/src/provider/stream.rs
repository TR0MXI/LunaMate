//! 执行单次 Provider 流、有限重试、超时和有界事件批处理。

use std::{collections::HashSet, sync::Arc, time::Duration};

use futures::{SinkExt as _, StreamExt as _, channel::mpsc};
use genai::{
    Client, ModelIden,
    chat::{
        ChatMessage as GenaiMessage, ChatOptions, ChatRequest, ChatStreamEvent as GenaiStreamEvent,
        ContentPart, MessageContent, ToolCall,
    },
};
use rust_i18n::t;
use tokio::time::{Instant, sleep, timeout_at};

use crate::{config::AppLanguage, memory::AssistantTrace};

use super::{ChatStreamEvent, ScreenshotCapability};

const FIRST_CONTENT_TIMEOUT: Duration = Duration::from_secs(30);
const STREAM_IDLE_TIMEOUT: Duration = Duration::from_secs(45);
const TERMINAL_EVENT_GRACE: Duration = Duration::from_millis(100);
const FLUSH_INTERVAL: Duration = Duration::from_millis(40);
const MAX_STREAM_CONTENT_BYTES: usize = 64 * 1024;
pub(crate) const MAX_HANDOFF_CONTENT_BYTES: usize = 256 * 1024;
const MAX_TOOL_CALLS: usize = 4;
pub(crate) const FLUSH_BYTES: usize = 512;
const RETRY_DELAYS: [Duration; 2] = [Duration::from_millis(500), Duration::from_millis(1_500)];

#[derive(Debug)]
pub(crate) struct AttemptFailure {
    pub(crate) message: String,
    pub(crate) retryable: bool,
    pub(crate) response_started: bool,
}

/// 单次 Provider 流结束后的语义结果。
#[derive(Debug)]
pub(crate) enum StreamOutcome {
    Complete {
        reasoning: Option<String>,
    },
    ToolUse {
        assistant_message: GenaiMessage,
        calls: Vec<ToolCall>,
        reasoning: Option<String>,
    },
}

/// 一次流式尝试的全部不变输入。
///
/// 重试与截图工具循环都会重复使用同一份能力快照，集中保存可以避免参数错位。
pub(super) struct StreamAttempt {
    pub(super) model: ModelIden,
    pub(super) request: ChatRequest,
    pub(super) options: Option<ChatOptions>,
    pub(super) total_deadline: Instant,
    pub(super) screenshot_capability: Option<Arc<dyn ScreenshotCapability>>,
    pub(super) capture_tool_handoff: bool,
    pub(super) language: AppLanguage,
}

pub(super) async fn stream_with_retry(
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
                    "event=provider_retry_scheduled attempt={retry_attempt} delay_ms={} remaining_ms={}",
                    delay.as_millis(),
                    remaining.as_millis()
                );
                sleep(delay).await;
            }
            Err(failure) => return Err(failure),
        }
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

pub(crate) async fn consume_stream<S>(
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

pub(crate) async fn send_terminal_event(
    events: &mut mpsc::Sender<ChatStreamEvent>,
    event: ChatStreamEvent,
    total_deadline: Instant,
) -> bool {
    send_event(events, event, terminal_delivery_deadline(total_deadline)).await
}

pub(crate) async fn send_completion_events(
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
