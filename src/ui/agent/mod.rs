//! 渲染桌宠窗口内的单行输入栏与回复浮层，并桥接纯逻辑 Agent。

mod configuration;
mod render;
mod reply;
mod screenshot;
mod voice;

use std::sync::Arc;

use futures::future::AbortHandle;
use gpui::{
    AppContext, Context, Entity, EventEmitter, Image, ImageFormat, PathPromptOptions, ScrollHandle,
    Subscription, Task, Window,
};
use gpui_component::input::{InputEvent, InputState};
use gpui_tokio::Tokio;
use lunamate_agent::{
    Agent, AgentEffect, AgentError, AgentInput, AgentSnapshot,
    config::AppLanguage,
    media::ImageAttachment,
    tools::{AgentOutfitRequest, OutfitOption},
};
use rust_i18n::t;

use crate::config::CONFIG;
use crate::voice::VoiceController;

pub(super) use reply::{AgentOverlayLayout, ReplyLifecycle};
use screenshot::host_screenshot_capability;

/// 当前请求在收到首个可见回复前使用的输入反馈样式。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(in crate::ui) enum ThinkingFeedback {
    Text,
    Voice,
}

/// Agent 视图向桌宠根视图发布的本地能力请求。
#[derive(Clone)]
pub(in crate::ui) enum AgentViewEvent {
    ChangeOutfit(AgentOutfitRequest),
    StopSpeech,
    SpeechAudio { samples: Vec<i16>, sample_rate: u32 },
}

pub(super) fn model_click_event_prompt(part_name: &str, language: AppLanguage) -> String {
    t!(
        "chat.event.model_part_clicked",
        locale = language.id(),
        part = part_name
    )
    .to_string()
}

/// 桌宠窗口中的 Agent 输入与回复覆盖层。
pub struct AgentView {
    agent: Arc<Agent>,
    snapshot: AgentSnapshot,
    agent_config_generation: u64,
    agent_config_pending_generation: Option<u64>,
    refresh_revision: u64,
    refresh_task: Option<Task<()>>,
    available_outfits: Vec<OutfitOption>,
    outfit_revision: u64,
    input: Entity<InputState>,
    pending_image: Option<PendingImage>,
    image_picker_revision: u64,
    image_picker_task: Option<Task<()>>,
    voice_transcription_task: Option<Task<()>>,
    voice_transcription_abort: Option<AbortHandle>,
    voice_transcription_request: Option<RemoteTranscriptionRequest>,
    speech_task: Option<Task<()>>,
    speech_abort: Option<AbortHandle>,
    speech_revision: u64,
    suspended: bool,
    messages_scroll: ScrollHandle,
    input_visible: bool,
    voice_indicator_visible: bool,
    thinking_feedback: Option<ThinkingFeedback>,
    thinking_feedback_revision: u64,
    reply_lifecycle: ReplyLifecycle,
    reply_fade_task: Option<Task<()>>,
    _state_task: Task<()>,
    _effect_task: Task<()>,
    _input_subscription: Subscription,
}

struct PendingImage {
    attachment: ImageAttachment,
    preview: Arc<Image>,
}

#[derive(Clone)]
struct RemoteTranscriptionRequest {
    revision: u64,
    utterance_id: u64,
    voice: VoiceController,
}

impl AgentView {
    /// 将框架无关核心挂载为 GPUI 输入和回复视图。
    pub(crate) fn new(
        agent: Arc<Agent>,
        generation: u64,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let snapshot = agent.snapshot();
        let input = cx.new(|cx| {
            InputState::new(window, cx)
                .submit_on_enter(true)
                .placeholder(t!("chat.input_placeholder").to_string())
        });
        let input_subscription = cx.subscribe_in(
            &input,
            window,
            |this, input, event: &InputEvent, window, cx| {
                if matches!(event, InputEvent::PressEnter { shift: false, .. }) {
                    this.submit_from_input(input, window, cx);
                }
            },
        );

        let mut state_updates = agent.subscribe();
        let state_task = cx.spawn(async move |this, cx| {
            while state_updates.changed().await.is_ok() {
                if this
                    .update(cx, |this, cx| this.sync_agent_snapshot(cx))
                    .is_err()
                {
                    break;
                }
            }
        });
        let effects = agent.effects();
        let effect_task = cx.spawn(async move |this, cx| {
            while let Ok(effect) = effects.recv().await {
                if this
                    .update(cx, |_this, cx| match effect {
                        AgentEffect::ChangeOutfit(request) => {
                            cx.emit(AgentViewEvent::ChangeOutfit(request));
                        }
                    })
                    .is_err()
                {
                    break;
                }
            }
        });

        let reply_visible = snapshot.status().is_some() || snapshot.reply_message_id().is_some();
        Self {
            agent,
            snapshot,
            agent_config_generation: generation,
            agent_config_pending_generation: None,
            refresh_revision: 0,
            refresh_task: None,
            available_outfits: Vec::new(),
            outfit_revision: 0,
            input,
            pending_image: None,
            image_picker_revision: 0,
            image_picker_task: None,
            voice_transcription_task: None,
            voice_transcription_abort: None,
            voice_transcription_request: None,
            speech_task: None,
            speech_abort: None,
            speech_revision: 0,
            suspended: false,
            messages_scroll: ScrollHandle::new(),
            input_visible: false,
            voice_indicator_visible: false,
            thinking_feedback: None,
            thinking_feedback_revision: 0,
            reply_lifecycle: ReplyLifecycle::new(reply_visible),
            reply_fade_task: None,
            _state_task: state_task,
            _effect_task: effect_task,
            _input_subscription: input_subscription,
        }
    }

    fn sync_agent_snapshot(&mut self, cx: &mut Context<Self>) {
        let previous = self.snapshot.clone();
        let snapshot = self.agent.snapshot();
        if snapshot.revision() == previous.revision() {
            return;
        }
        let has_reply = snapshot.status().is_some() || snapshot.reply_message_id().is_some();
        let became_terminal = previous.is_streaming() && !snapshot.is_streaming();
        let reply_changed = snapshot.reply_message_id() != previous.reply_message_id()
            || snapshot.status() != previous.status();
        self.snapshot = snapshot;
        if self.snapshot.is_streaming() && !self.waiting_for_visible_reply() {
            self.thinking_feedback = None;
        }
        if has_reply && (reply_changed || previous.is_streaming()) {
            self.reveal_reply(cx);
        }
        if became_terminal {
            self.schedule_reply_fade(cx);
            if !self.suspended {
                self.start_speech(self.snapshot.clone(), cx);
            }
        }
        self.messages_scroll.scroll_to_bottom();
        cx.notify();
    }

    pub fn set_available_outfits(&mut self, outfits: Vec<OutfitOption>) {
        self.available_outfits = outfits;
        self.outfit_revision = self.outfit_revision.wrapping_add(1).max(1);
    }

    pub fn outfit_request_is_current(&self, request: &AgentOutfitRequest) -> bool {
        request.revision() == self.outfit_revision && !request.is_cancelled()
    }

    #[cfg(test)]
    pub(super) fn is_streaming_for_test(&self) -> bool {
        self.snapshot.is_streaming()
    }

    #[cfg(test)]
    pub(super) fn message_count_for_test(&self) -> usize {
        self.snapshot.messages().len()
    }

    #[cfg(test)]
    pub(super) fn pending_voice_for_test(&self) -> Option<u64> {
        self.pending_voice_utterance()
    }

    pub(in crate::ui) fn pending_voice_utterance(&self) -> Option<u64> {
        self.agent.snapshot().pending_voice()
    }

    #[cfg(test)]
    pub(super) fn active_persona_for_test(&self) -> &str {
        self.snapshot.active_persona()
    }

    pub fn set_input_visible(
        &mut self,
        visible: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.input_visible = visible;
        if visible {
            self.refresh_settings(CONFIG.agent_config_snapshot(), cx);
            self.input.update(cx, |input, cx| input.focus(window, cx));
        } else {
            cx.notify();
        }
    }

    pub fn send_model_click_event(
        &mut self,
        part_name: &str,
        language: AppLanguage,
        cx: &mut Context<Self>,
    ) -> bool {
        if self.snapshot.is_streaming() {
            return false;
        }
        self.cancel_speech();
        cx.emit(AgentViewEvent::StopSpeech);
        self.agent.cancel_pending_voice();
        self.refresh_settings(CONFIG.agent_config_snapshot(), cx);
        self.send_message(
            model_click_event_prompt(part_name, language),
            None,
            language,
            None,
            cx,
        )
    }

    /// 隐藏桌宠时挂起 Agent，并丢弃触发中的整轮对话。
    pub fn suspend_for_hidden(&mut self, cx: &mut Context<Self>) {
        if self.suspended {
            return;
        }
        self.suspended = true;
        self.cancel_remote_transcription();
        self.cancel_speech();
        self.agent.suspend_and_discard_active_turn();
        self.sync_agent_snapshot(cx);
        cx.emit(AgentViewEvent::StopSpeech);
    }

    /// 显示桌宠后允许新的 Agent 请求进入 Provider。
    pub fn resume_after_hidden(&mut self) {
        if !self.suspended {
            return;
        }
        self.suspended = false;
        self.agent.resume_after_hidden();
    }

    fn submit_from_input(
        &mut self,
        input: &Entity<InputState>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.agent.cancel_pending_voice();
        self.cancel_remote_transcription();
        self.cancel_speech();
        cx.emit(AgentViewEvent::StopSpeech);
        let text = input.read(cx).value().to_string();
        let image = self
            .pending_image
            .as_ref()
            .map(|pending| pending.attachment.clone());
        let language = CONFIG.agent_config_snapshot().language();
        self.refresh_settings(CONFIG.agent_config_snapshot(), cx);
        if self.send_message(text, image, language, Some(ThinkingFeedback::Text), cx) {
            self.image_picker_revision = self.image_picker_revision.wrapping_add(1).max(1);
            self.image_picker_task = None;
            self.pending_image = None;
            input.update(cx, |input, cx| input.set_value("", window, cx));
        }
    }

    fn submit_current_input(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let input = self.input.clone();
        self.submit_from_input(&input, window, cx);
    }

    fn send_message(
        &mut self,
        text: String,
        image: Option<ImageAttachment>,
        language: AppLanguage,
        thinking_feedback: Option<ThinkingFeedback>,
        cx: &mut Context<Self>,
    ) -> bool {
        if self.suspended
            || self.snapshot.is_streaming()
            || self.snapshot.is_switching_memory()
            || self.snapshot.is_shutting_down()
        {
            return false;
        }
        let outfits = if CONFIG.allow_agent_outfit_change() {
            self.available_outfits.clone()
        } else {
            Vec::new()
        };
        let request = AgentInput {
            text,
            image,
            screenshot_capability: host_screenshot_capability(),
            outfits,
            outfit_revision: self.outfit_revision,
            request_revision: self.agent.request_revision(),
            language,
        };
        self.thinking_feedback = thinking_feedback;
        self.thinking_feedback_revision = self.thinking_feedback_revision.wrapping_add(1).max(1);
        let thinking_feedback_revision = self.thinking_feedback_revision;
        cx.notify();
        let agent = self.agent.clone();
        let request_agent = agent.clone();
        let request_task = Tokio::spawn(cx, async move { request_agent.send(request).await });
        cx.spawn(async move |this, cx| {
            if let Ok(Err(error)) = request_task.await
                && !matches!(error, AgentError::Suspended | AgentError::StaleInput)
            {
                agent.set_status(Some(error.localized_message(language)));
            }
            let _ = this.update(cx, |this, cx| {
                if this.thinking_feedback_revision == thinking_feedback_revision {
                    this.thinking_feedback = None;
                    cx.notify();
                }
            });
        })
        .detach();
        true
    }

    fn stop(&mut self, cx: &mut Context<Self>) {
        self.cancel_speech();
        if self.agent.cancel() {
            self.sync_agent_snapshot(cx);
            self.schedule_reply_fade(cx);
        }
    }

    fn choose_image(&mut self, cx: &mut Context<Self>) {
        if self.snapshot.is_streaming() {
            return;
        }
        self.image_picker_revision = self.image_picker_revision.wrapping_add(1).max(1);
        let revision = self.image_picker_revision;
        let language = self.snapshot.language();
        self.image_picker_task = None;
        let paths = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: Some(
                t!("chat.select_image", locale = language.id())
                    .to_string()
                    .into(),
            ),
        });
        let background = cx.background_executor().clone();
        self.image_picker_task = Some(cx.spawn(async move |this, cx| {
            let path = match paths.await {
                Ok(Ok(Some(paths))) => paths.into_iter().next(),
                Ok(Ok(None)) => return,
                Ok(Err(_)) | Err(_) => {
                    let _ = this.update(cx, |this, cx| {
                        if this.image_picker_revision == revision {
                            this.agent.set_status(Some(
                                t!("chat.error.image_picker", locale = language.id()).to_string(),
                            ));
                            this.sync_agent_snapshot(cx);
                        }
                    });
                    return;
                }
            };
            let Some(path) = path else {
                return;
            };
            let loaded = background
                .spawn(async move { crate::platform::load_agent_image(&path) })
                .await;
            let _ = this.update(cx, |this, cx| {
                if this.image_picker_revision != revision {
                    return;
                }
                match loaded {
                    Ok(attachment) => {
                        let Some(bytes) = attachment.bytes() else {
                            this.agent.set_status(Some(
                                t!("chat.error.image_prepare", locale = language.id()).to_string(),
                            ));
                            this.sync_agent_snapshot(cx);
                            return;
                        };
                        let preview =
                            Arc::new(Image::from_bytes(ImageFormat::Jpeg, bytes.to_vec()));
                        this.pending_image = Some(PendingImage {
                            attachment,
                            preview,
                        });
                        this.agent.set_status(None);
                        this.sync_agent_snapshot(cx);
                    }
                    Err(error) => {
                        this.agent
                            .set_status(Some(error.localized_message(language)));
                        this.sync_agent_snapshot(cx);
                    }
                }
                cx.notify();
            });
        }));
    }

    fn remove_pending_image(&mut self, cx: &mut Context<Self>) {
        self.image_picker_revision = self.image_picker_revision.wrapping_add(1).max(1);
        self.image_picker_task = None;
        self.pending_image = None;
        cx.notify();
    }
}

impl EventEmitter<AgentViewEvent> for AgentView {}
