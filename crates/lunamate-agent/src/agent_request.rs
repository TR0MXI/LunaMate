//! 协调单轮 Provider 请求、取消、挂起、语音打断和流事件落盘。

use std::{sync::Arc, time::Instant};

use futures::{
    StreamExt as _,
    channel::mpsc,
    future::{AbortHandle, Abortable},
};
use rust_i18n::t;

use crate::{
    ActiveRequest, Agent, AgentEffect, AgentError, AgentInput, AgentState, ChatMessage,
    PendingVoice,
    config::AppLanguage,
    provider::{ChatServiceRequest, ChatStreamEvent, stream_with_client},
    session::ResponseId,
};

use super::agent_coordination::{abort_active_request, next_revision};

const STREAM_CHANNEL_CAPACITY: usize = 16;

impl Agent {
    /// 创建并执行一轮完整请求，直到收到终态、取消或网络任务结束。
    pub async fn send(self: Arc<Self>, input: AgentInput) -> Result<(), AgentError> {
        let runtime = self.runtime.read().clone();
        let (response_id, request, abort_registration) = {
            let mut state = self.state.lock();
            if state.shutting_down {
                return Err(AgentError::ShuttingDown);
            }
            if state.suspended {
                return Err(AgentError::Suspended);
            }
            if input.request_revision != state.request_revision {
                return Err(AgentError::StaleInput);
            }
            if state.switching_memory {
                return Err(AgentError::MemorySwitching);
            }
            let model = runtime.model.clone().ok_or(AgentError::ModelUnavailable)?;
            if runtime.limits.max_request_tokens < 8 {
                return Err(AgentError::ContextWindowExhausted);
            }
            let started = state
                .session
                .start_turn_with_image(input.text, input.image, input.language)
                .map_err(|error| AgentError::Session(error.localized_message(input.language)))?;
            state.pending_voice = None;
            let response_id = started.response_id;
            let request = ChatServiceRequest {
                model,
                options: runtime.options.clone(),
                system_prompt: runtime.system_prompt.to_string(),
                messages: started.context,
                screenshot_capability: input.screenshot_capability,
                outfits: input.outfits,
                outfit_revision: input.outfit_revision,
                language: input.language,
            };
            let (abort, abort_registration) = AbortHandle::new_pair();
            state.active_request = Some(ActiveRequest {
                response_id,
                runtime_revision: runtime.revision,
                abort,
                started_at: Instant::now(),
            });
            state.status = None;
            state.reply_message_id = state.session.messages().back().map(ChatMessage::id);
            (response_id, request, abort_registration)
        };
        self.publish_live_context_for(&runtime);
        self.persist(true);
        self.notify_state();

        let (sender, mut receiver) = mpsc::channel(STREAM_CHANNEL_CAPACITY);
        let provider_task = tokio::spawn(Abortable::new(
            stream_with_client(runtime.client.clone(), request, sender),
            abort_registration,
        ));
        while let Some(event) = receiver.next().await {
            if !self.apply_stream_event(response_id, runtime.revision, event, input.language) {
                break;
            }
        }
        drop(receiver);

        let provider_result = provider_task.await;
        let mut terminal_failure = None;
        {
            let mut state = self.state.lock();
            if request_is_current(&state, response_id, runtime.revision)
                && state.session.active_response_id() == Some(response_id)
            {
                let failure = match provider_result {
                    Ok(Ok(())) => t!("chat.stream_ended", locale = input.language.id()).to_string(),
                    Ok(Err(_)) => return Ok(()),
                    Err(error) => t!(
                        "chat.task_ended",
                        locale = input.language.id(),
                        kind = if error.is_cancelled() {
                            "cancelled"
                        } else {
                            "panic"
                        }
                    )
                    .to_string(),
                };
                state.session.fail_response(response_id, failure.clone());
                state.status = Some(failure.clone());
                state.active_request = None;
                terminal_failure = Some(failure);
            }
        }
        if terminal_failure.is_some() {
            self.persist(true);
            self.notify_state();
        }
        Ok(())
    }

    /// 取消当前 Provider 请求并把助手消息转换为明确终态。
    pub fn cancel(&self) -> bool {
        let cancelled = {
            let mut state = self.state.lock();
            let Some(response_id) = state.session.active_response_id() else {
                return false;
            };
            state.request_revision = next_revision(state.request_revision);
            abort_active_request(&mut state, "user_stop");
            let cancelled = state.session.cancel_response(response_id);
            if cancelled {
                state.status = None;
            }
            cancelled
        };
        if cancelled {
            self.persist(true);
            self.notify_state();
        }
        cancelled
    }

    /// 挂起 Agent，取消当前请求并移除触发它的整轮消息。
    pub fn suspend_and_discard_active_turn(&self) -> bool {
        let discarded = {
            let mut state = self.state.lock();
            state.suspended = true;
            state.request_revision = next_revision(state.request_revision);
            state.pending_voice = None;
            let discarded = state
                .session
                .active_response_id()
                .map(|response_id| {
                    abort_active_request(&mut state, "hidden");
                    state.session.discard_response_turn(response_id)
                })
                .unwrap_or(false);
            if discarded {
                state.reply_message_id = None;
                state.status = None;
            }
            discarded
        };
        if discarded {
            self.publish_live_context();
            self.persist(true);
        }
        self.notify_state();
        discarded
    }

    /// 解除桌宠隐藏期间的 Agent 挂起状态。
    pub fn resume_after_hidden(&self) {
        let changed = {
            let mut state = self.state.lock();
            if state.suspended {
                state.suspended = false;
                true
            } else {
                false
            }
        };
        if changed {
            self.notify_state();
        }
    }

    /// 登记最新语音 utterance，并在存在活动回复时按语音语义打断。
    pub fn voice_started(&self, utterance_id: u64, language: AppLanguage) -> bool {
        let runtime = self.runtime.read().clone();
        let interrupted = {
            let mut state = self.state.lock();
            if state.shutting_down
                || state
                    .pending_voice
                    .as_ref()
                    .is_some_and(|pending| pending.utterance_id >= utterance_id)
            {
                return false;
            }
            state.pending_voice = Some(PendingVoice {
                utterance_id,
                runtime_revision: runtime.revision,
                persona: runtime.active_persona,
                language,
            });
            state.request_revision = next_revision(state.request_revision);
            let Some(response_id) = state.session.active_response_id() else {
                return true;
            };
            abort_active_request(&mut state, "voice_interruption");
            state.session.interrupt_response_by_voice(response_id)
        };
        if interrupted {
            self.persist(true);
        }
        self.notify_state();
        true
    }

    /// 消费仍属于当前 runtime 和人格的转写；失效结果返回 `None`。
    pub fn take_voice_transcript(&self, utterance_id: u64) -> Option<AppLanguage> {
        let runtime = self.runtime.read().clone();
        let mut state = self.state.lock();
        if !state
            .pending_voice
            .as_ref()
            .is_some_and(|pending| pending.utterance_id == utterance_id)
        {
            return None;
        }
        let pending = state.pending_voice.as_ref()?;
        if pending.runtime_revision != runtime.revision
            || pending.persona != runtime.active_persona
            || state.switching_memory
            || state.shutting_down
            || state.session.active_response_id().is_some()
        {
            state.pending_voice = None;
            return None;
        }
        state.pending_voice.take().map(|pending| pending.language)
    }

    pub fn cancel_voice(&self, utterance_id: u64) {
        let mut state = self.state.lock();
        if state
            .pending_voice
            .as_ref()
            .is_some_and(|pending| pending.utterance_id == utterance_id)
        {
            state.pending_voice = None;
        }
    }

    pub fn cancel_pending_voice(&self) {
        self.state.lock().pending_voice = None;
    }

    /// 设置只用于宿主展示的状态文本，不会进入 Provider 上下文。
    pub fn set_status(&self, status: Option<String>) {
        let mut state = self.state.lock();
        state.status = status;
        if state.status.is_some() {
            state.reply_message_id = None;
        }
        drop(state);
        self.notify_state();
    }

    fn apply_stream_event(
        &self,
        response_id: ResponseId,
        runtime_revision: u64,
        event: ChatStreamEvent,
        language: AppLanguage,
    ) -> bool {
        if let ChatStreamEvent::ChangeOutfit(request) = event {
            let current = {
                let state = self.state.lock();
                request_is_current(&state, response_id, runtime_revision)
                    && state.session.active_response_id() == Some(response_id)
            };
            if !current {
                request.complete(false);
                return false;
            }
            if self
                .effects
                .try_send(AgentEffect::ChangeOutfit(request.clone()))
                .is_err()
            {
                request.complete(false);
            }
            return true;
        }
        let (keep_receiving, terminal) = {
            let mut state = self.state.lock();
            if !request_is_current(&state, response_id, runtime_revision)
                || state.session.active_response_id() != Some(response_id)
            {
                return false;
            }
            match event {
                ChatStreamEvent::Delta(chunk) => {
                    if state.session.append_response(response_id, &chunk).is_err() {
                        let failure =
                            t!("chat.reply_too_large", locale = language.id()).to_string();
                        state.session.fail_response(response_id, failure.clone());
                        state.status = Some(failure);
                        state.active_request = None;
                        (false, true)
                    } else {
                        (true, false)
                    }
                }
                ChatStreamEvent::Trace(trace) => {
                    let _ = state.session.attach_response_trace(response_id, trace);
                    (true, false)
                }
                ChatStreamEvent::Finished => {
                    if !state.session.finish_response(response_id) {
                        return false;
                    }
                    state.active_request = None;
                    (false, true)
                }
                ChatStreamEvent::Failed(message) => {
                    if !state.session.fail_response(response_id, message.clone()) {
                        return false;
                    }
                    state.status = Some(message);
                    state.active_request = None;
                    (false, true)
                }
                ChatStreamEvent::ChangeOutfit(_) => unreachable!("换装事件已在加锁前处理"),
            }
        };
        self.publish_live_context();
        self.persist(terminal);
        self.notify_state();
        keep_receiving
    }
}

fn request_is_current(state: &AgentState, response_id: ResponseId, revision: u64) -> bool {
    state.active_request.as_ref().is_some_and(|request| {
        request.response_id == response_id && request.runtime_revision == revision
    })
}
