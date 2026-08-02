//! 执行截图与换装工具，并生成可持久化的脱敏执行详情。

use std::{sync::Arc, time::Duration};

use futures::{SinkExt as _, channel::mpsc};
use genai::chat::{ToolCall, ToolResponse};
use serde_json::json;
use tokio::time::{Instant, timeout_at};

use crate::{
    media::{ImageAttachment, ImageInputError},
    memory::ToolExecutionTrace,
    tools::{AgentOutfitRequest, AgentOutfitResult, OutfitOption},
};

use super::{CHANGE_OUTFIT_TOOL, ChatStreamEvent, SCREEN_CAPTURE_TOOL, ScreenshotCapability};

const SCREEN_CAPTURE_TIMEOUT: Duration = Duration::from_secs(30);

pub(super) struct ToolContinuation {
    pub(super) responses: Vec<ToolResponse>,
    pub(super) image: Option<ImageAttachment>,
    pub(super) screen_capture_requested: bool,
    pub(super) stateless_results: Vec<String>,
    pub(super) trace_results: Vec<serde_json::Value>,
}

impl ToolContinuation {
    pub(super) fn revoke_image(&mut self) {
        if self.image.take().is_none() {
            return;
        }
        log::info!("event=screenshot_discarded stage=pre_upload reason=permission_revoked");
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

pub(super) async fn execute_tool_calls(
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
                            log::info!("event=screenshot_capture_started authorized=true");
                            match capture_screen(Arc::clone(capability), total_deadline).await {
                                Ok(captured) if capability.is_authorized() => {
                                    log::info!(
                                        "event=screenshot_capture_completed width={} height={} encoded_bytes={} elapsed_ms={}",
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
                                        "event=screenshot_capture_rejected stage=post_capture reason=permission_revoked elapsed_ms={}",
                                        capture_started.elapsed().as_millis()
                                    );
                                    json!({"status": "error", "code": "permission_revoked"})
                                }
                                Err(_code) if !capability.is_authorized() => {
                                    log::warn!(
                                        "event=screenshot_capture_failed reason=permission_revoked elapsed_ms={}",
                                        capture_started.elapsed().as_millis()
                                    );
                                    json!({"status": "error", "code": "permission_revoked"})
                                }
                                Err(code) => {
                                    log::warn!(
                                        "event=screenshot_capture_failed result={code} elapsed_ms={}",
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

pub(super) fn tool_execution_traces(
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
pub(crate) async fn execute_tool_traces_for_test(calls: &[ToolCall]) -> Vec<ToolExecutionTrace> {
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
pub(crate) async fn execute_screenshot_tool_for_test(
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

pub(crate) fn outfit_argument<'a>(
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

pub(crate) async fn request_outfit_change(
    outfit: String,
    outfit_revision: u64,
    total_deadline: Instant,
    events: &mut mpsc::Sender<ChatStreamEvent>,
) -> Result<(), &'static str> {
    let (request, result) = AgentOutfitRequest::channel(outfit, outfit_revision);
    log::debug!("event=outfit_change_dispatched outfit_revision={outfit_revision}");
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
            log::debug!("event=outfit_change_completed outfit_revision={outfit_revision}");
            Ok(())
        }
        Ok(Ok(AgentOutfitResult::Failed)) => {
            log::warn!("event=outfit_change_failed outfit_revision={outfit_revision}");
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
