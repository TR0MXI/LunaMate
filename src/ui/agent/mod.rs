//! 渲染桌宠窗口内的单行输入栏与回复浮层，并桥接纯逻辑 Agent。

mod render;
mod reply;
mod screenshot;

use std::sync::Arc;

use gpui::{
    AppContext, Context, Entity, EventEmitter, Image, ImageFormat, PathPromptOptions, ScrollHandle,
    Subscription, Task, Window,
};
use gpui_component::input::{InputEvent, InputState};
use gpui_tokio::Tokio;
use lunamate_agent::{
    Agent, AgentEffect, AgentError, AgentInput, AgentSnapshot, Client, chat_limits,
    client_from_model,
    config::{AgentConfigSnapshot, AppLanguage},
    media::ImageAttachment,
    model_and_options_from_config,
    stt::{TranscriptionInput, transcribe},
    tools::{AgentOutfitRequest, OutfitOption},
    tts::synthesize,
};
use rust_i18n::t;

use crate::config::CONFIG;
use crate::voice::VoiceController;

pub(super) use reply::{AgentOverlayLayout, ReplyLifecycle};
use screenshot::host_screenshot_capability;

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
    refresh_revision: u64,
    refresh_task: Option<Task<()>>,
    available_outfits: Vec<OutfitOption>,
    outfit_revision: u64,
    input: Entity<InputState>,
    pending_image: Option<PendingImage>,
    image_picker_revision: u64,
    image_picker_task: Option<Task<()>>,
    voice_transcription_task: Option<Task<()>>,
    voice_transcription_abort: Option<tokio::task::AbortHandle>,
    voice_transcription_request: Option<RemoteTranscriptionRequest>,
    speech_task: Option<Task<()>>,
    speech_abort: Option<tokio::task::AbortHandle>,
    speech_revision: u64,
    suspended: bool,
    messages_scroll: ScrollHandle,
    input_visible: bool,
    voice_indicator_visible: bool,
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
        self.agent.snapshot().pending_voice()
    }

    #[cfg(test)]
    pub(super) fn active_persona_for_test(&self) -> &str {
        self.snapshot.active_persona()
    }

    /// 把宿主配置快照解析为直接 Agent 组件；核心本身不依赖该快照类型。
    pub fn refresh_settings(&mut self, snapshot: AgentConfigSnapshot, cx: &mut Context<Self>) {
        if snapshot.generation() <= self.agent_config_generation {
            return;
        }
        self.agent.cancel_pending_voice();
        self.cancel_speech();
        self.agent_config_generation = snapshot.generation();
        self.refresh_revision = self.refresh_revision.wrapping_add(1).max(1);
        let revision = self.refresh_revision;
        self.refresh_task = None;

        let Some(persona) = snapshot.personas().active().cloned() else {
            return;
        };
        let model = persona
            .model
            .as_deref()
            .and_then(|id| snapshot.settings().model(id))
            .or_else(|| snapshot.settings().selected())
            .cloned();
        let limits = chat_limits(&persona, snapshot.settings());
        let language = snapshot.language();
        let system_prompt = persona.system_prompt;
        let persona_id = persona.id;
        let agent = self.agent.clone();
        let memory = agent.memory();
        let build = Tokio::spawn(cx, async move {
            let client_model = model.clone();
            let client = tokio::task::spawn_blocking(move || {
                client_model
                    .as_ref()
                    .map_or_else(Client::default, client_from_model)
            })
            .await
            .map_err(|error| error.to_string())?;
            let (model_iden, options) = model
                .as_ref()
                .and_then(model_and_options_from_config)
                .map_or((None, None), |(model, options)| (Some(model), options));
            agent
                .apply_configuration(
                    snapshot.generation(),
                    client,
                    model_iden,
                    options,
                    system_prompt,
                    memory,
                    persona_id,
                    limits,
                    language,
                )
                .await
                .map_err(|error| error.to_string())?;
            Ok::<(), String>(())
        });
        self.refresh_task = Some(cx.spawn(async move |this, cx| {
            let result = build.await;
            let _ = this.update(cx, |this, cx| {
                if this.refresh_revision != revision {
                    return;
                }
                this.refresh_task = None;
                if let Err(error) = result.unwrap_or_else(|error| Err(error.to_string())) {
                    this.agent.set_status(Some(
                        t!("chat.persistence_unavailable", error = error).to_string(),
                    ));
                }
                cx.notify();
            });
        }));
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
            cx,
        )
    }

    pub fn voice_speech_started(
        &mut self,
        utterance_id: u64,
        language: AppLanguage,
        cx: &mut Context<Self>,
    ) {
        self.cancel_remote_transcription();
        self.cancel_speech();
        cx.emit(AgentViewEvent::StopSpeech);
        let snapshot = CONFIG.agent_config_snapshot();
        if snapshot.generation() != self.agent_config_generation {
            self.refresh_settings(snapshot, cx);
            return;
        }
        self.agent.voice_started(utterance_id, language);
        self.sync_agent_snapshot(cx);
        cx.notify();
    }

    /// 在按键边沿立即停止本地语音，语义层打断仍等待真实语音确认。
    pub fn voice_input_pressed(&mut self, cx: &mut Context<Self>) {
        self.cancel_speech();
        cx.emit(AgentViewEvent::StopSpeech);
    }

    pub fn send_voice_transcript(
        &mut self,
        utterance_id: u64,
        text: String,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some(language) = self.agent.take_voice_transcript(utterance_id) else {
            return false;
        };
        self.send_message(text, None, language, cx)
    }

    pub fn voice_utterance_cancelled(&mut self, utterance_id: u64) {
        self.cancel_remote_transcription_for(utterance_id);
        self.cancel_speech();
        self.agent.cancel_voice(utterance_id);
    }

    pub fn cancel_pending_voice(&mut self) {
        self.cancel_remote_transcription();
        self.cancel_speech();
        self.agent.cancel_pending_voice();
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

    pub fn transcribe_voice(
        &mut self,
        revision: u64,
        utterance_id: u64,
        model_id: String,
        samples: Vec<i16>,
        voice: VoiceController,
        cx: &mut Context<Self>,
    ) {
        self.cancel_remote_transcription();
        self.cancel_speech();
        cx.emit(AgentViewEvent::StopSpeech);
        let language = self.snapshot.language();
        let Some(model) = CONFIG.llm_settings().model(&model_id).cloned() else {
            voice.complete_remote_transcription(
                revision,
                utterance_id,
                Err(t!("voice.model_missing", locale = language.id()).to_string()),
            );
            return;
        };
        let task = tokio::spawn(async move {
            let input = TranscriptionInput::new(samples)
                .map_err(|error| error.localized_message(language))?;
            transcribe(&model, input, language)
                .await
                .map_err(|error| error.localized_message(language))
        });
        let request = RemoteTranscriptionRequest {
            revision,
            utterance_id,
            voice: voice.clone(),
        };
        self.voice_transcription_abort = Some(task.abort_handle());
        self.voice_transcription_request = Some(request.clone());
        self.voice_transcription_task = Some(cx.spawn(async move |this, cx| {
            let result = task.await.unwrap_or_else(|_| {
                Err(t!("voice.transcription_cancelled", locale = language.id()).to_string())
            });
            let current = this
                .update(cx, |this, _| {
                    this.voice_transcription_request
                        .as_ref()
                        .is_some_and(|request| {
                            request.revision == revision && request.utterance_id == utterance_id
                        })
                })
                .unwrap_or(false);
            if current {
                voice.complete_remote_transcription(revision, utterance_id, result);
            }
            let _ = this.update(cx, |this, _| {
                if this
                    .voice_transcription_request
                    .as_ref()
                    .is_some_and(|request| {
                        request.revision == revision && request.utterance_id == utterance_id
                    })
                {
                    this.voice_transcription_request = None;
                    this.voice_transcription_abort = None;
                    this.voice_transcription_task = None;
                }
            });
        }));
    }

    pub fn voice_failed(&mut self, message: String, cx: &mut Context<Self>) {
        self.cancel_remote_transcription();
        self.cancel_speech();
        cx.emit(AgentViewEvent::StopSpeech);
        self.agent.cancel_pending_voice();
        self.agent.set_status(Some(message));
        self.sync_agent_snapshot(cx);
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
        if self.send_message(text, image, language, cx) {
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
        let agent = self.agent.clone();
        Tokio::spawn(cx, async move {
            if let Err(error) = agent.clone().send(request).await
                && !matches!(error, AgentError::Suspended | AgentError::StaleInput)
            {
                agent.set_status(Some(error.localized_message(language)));
            }
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

    fn cancel_remote_transcription(&mut self) {
        if let Some(request) = self.voice_transcription_request.take() {
            request
                .voice
                .cancel_remote_transcription(request.revision, request.utterance_id);
        }
        if let Some(abort) = self.voice_transcription_abort.take() {
            abort.abort();
        }
        self.voice_transcription_task = None;
    }

    fn cancel_remote_transcription_for(&mut self, utterance_id: u64) {
        if self
            .voice_transcription_request
            .as_ref()
            .is_some_and(|request| request.utterance_id == utterance_id)
        {
            self.cancel_remote_transcription();
        }
    }

    fn start_speech(&mut self, snapshot: AgentSnapshot, cx: &mut Context<Self>) {
        self.cancel_speech();
        let revision = self.speech_revision;
        let personas = CONFIG.persona_settings();
        let Some(persona) = personas.active() else {
            return;
        };
        if persona.id != snapshot.active_persona() {
            return;
        }
        let Some(model_id) = persona.tts_model.as_deref() else {
            return;
        };
        let Some(message) = snapshot.messages().iter().find(|message| {
            Some(message.id()) == snapshot.reply_message_id()
                && message.role() == lunamate_agent::ChatRole::Assistant
        }) else {
            return;
        };
        if !matches!(message.state(), lunamate_agent::ChatMessageState::Complete) {
            return;
        }
        let Some(model) = CONFIG.llm_settings().model(model_id).cloned() else {
            return;
        };
        let text = message.visible_content().to_owned();
        let task = tokio::spawn(async move { synthesize(&model, &text).await });
        self.speech_abort = Some(task.abort_handle());
        self.speech_task = Some(cx.spawn(async move |this, cx| {
            match task.await {
                Ok(Ok(audio)) => {
                    let _ = this.update(cx, |this, cx| {
                        if this.suspended || this.speech_revision != revision {
                            return;
                        }
                        let sample_rate = audio.sample_rate();
                        cx.emit(AgentViewEvent::SpeechAudio {
                            samples: audio.into_samples(),
                            sample_rate,
                        });
                    });
                }
                Ok(Err(error)) => log::warn!("助手回复语音合成失败：{error}"),
                Err(error) if !error.is_cancelled() => {
                    log::warn!("助手回复语音合成任务异常结束：{error}");
                }
                Err(_) => {}
            }
            let _ = this.update(cx, |this, _| {
                if this.speech_revision == revision {
                    this.speech_abort = None;
                    this.speech_task = None;
                }
            });
        }));
    }

    fn cancel_speech(&mut self) {
        self.speech_revision = self.speech_revision.wrapping_add(1).max(1);
        if let Some(abort) = self.speech_abort.take() {
            abort.abort();
        }
        self.speech_task = None;
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
