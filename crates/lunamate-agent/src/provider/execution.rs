//! 协调整轮请求、工具续轮和最终详情投递。

use futures::channel::mpsc;
use genai::{
    Client,
    chat::{ChatMessage as GenaiMessage, ContentPart, MessageContent},
};
use rust_i18n::t;
use tokio::time::Instant;

use crate::{config::llm_provider_id, media::ImageAttachment, memory::AssistantTrace};

use super::{
    ChatServiceRequest, ChatStreamEvent, TOTAL_RESPONSE_TIMEOUT,
    request::{
        append_stateless_capture_result, append_stateless_tool_results, bounded_outfits,
        build_request, image_content_part, outfit_tool_options, provider_supports_binary_and_tools,
        screen_capture_handoff_prompt,
    },
    stream::{
        StreamAttempt, StreamOutcome, send_completion_events, send_terminal_event,
        stream_with_retry,
    },
    tool_execution::{execute_tool_calls, tool_execution_traces},
};

/// 执行流式聊天，并且只在首段正文前进行有限退避重试。
pub(super) async fn stream_chat(
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
            "event=provider_capability_rejected provider={} capability=image",
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
                    log::warn!("event=agent_tool_loop_rejected reason=round_limit");
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

fn append_trace_reasoning(target: &mut String, reasoning: Option<String>) {
    let Some(reasoning) = reasoning.filter(|reasoning| !reasoning.trim().is_empty()) else {
        return;
    };
    if !target.is_empty() {
        target.push_str("\n\n");
    }
    target.push_str(&reasoning);
}
