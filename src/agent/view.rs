//! 渲染桌宠窗口内的单行输入栏与回复浮层，并拥有网络请求生命周期。

use std::{
    sync::Arc,
    time::{Duration, Instant},
};

use futures::{
    StreamExt as _,
    channel::mpsc,
    future::{AbortHandle, Abortable},
};
use gpui::{
    Animation, AnimationExt as _, AnyElement, AppContext, Context, Entity, EventEmitter, Image,
    ImageFormat, IntoElement, MouseButton, ObjectFit, PathPromptOptions, Render, ScrollHandle,
    Subscription, Task, Window, div, img, prelude::*, px, svg,
};
use gpui_component::{
    StyledExt as _,
    input::{Input, InputEvent, InputState},
    tooltip::Tooltip,
};
use gpui_tokio::Tokio;
use rust_i18n::t;

use crate::config::{
    AppLanguage, CONFIG, LlmModelConfig, SharedLlmSettings, SharedPersonaSettings,
};

use super::{
    AgentMemoryAccess, AgentOutfitRequest, AgentShutdown, AgentViewEvent, chat_limits,
    media::{ImageAttachment, load_image},
    palette::AgentPalette,
    service::{ChatBackend, ChatServiceRequest, ChatStreamEvent, GenaiChatBackend},
    session::{ChatLimits, ChatMessage, ChatMessageState, ChatRole, ChatSession, ResponseId},
    store::ChatSessionStore,
};

const STREAM_CHANNEL_CAPACITY: usize = 16;
const PERSIST_INTERVAL: Duration = Duration::from_secs(3);
const REPLY_LINGER_DURATION: Duration = Duration::from_secs(4);
const REPLY_FADE_DURATION: Duration = Duration::from_millis(800);
const REPLY_MAX_HEIGHT: f32 = 180.0;
const REPLY_CONTENT_MIN_HEIGHT: f32 = 60.0;
const REPLY_MIN_HEIGHT: f32 = 78.0;
const REPLY_VERTICAL_INSET: f32 = 12.0;
const OVERLAY_BOTTOM_RESERVED: f32 = 108.0;
const NARROW_OVERLAY_BREAKPOINT: f32 = 180.0;

pub(super) fn model_click_event_prompt(part_name: &str, language: AppLanguage) -> String {
    t!(
        "chat.event.model_part_clicked",
        locale = language.id(),
        part = part_name
    )
    .to_string()
}

/// 桌宠窗口中的单会话 Agent 覆盖层。
pub(crate) struct AgentView {
    session: ChatSession,
    store: Arc<ChatSessionStore>,
    persist_revision: u64,
    shutdown_revision: Option<u64>,
    last_persist: Instant,
    settings: SharedLlmSettings,
    persona: SharedPersonaSettings,
    active_persona: String,
    active_limits: ChatLimits,
    memory: AgentMemoryAccess,
    agent_config_revision: u64,
    persona_swap_revision: u64,
    persona_swap_task: Option<Task<()>>,
    backend: Arc<dyn ChatBackend>,
    available_outfits: Vec<String>,
    outfit_revision: u64,
    input: Entity<InputState>,
    pending_image: Option<PendingImage>,
    image_picker_revision: u64,
    image_picker_task: Option<Task<()>>,
    messages_scroll: ScrollHandle,
    status: Option<String>,
    input_visible: bool,
    voice_indicator_visible: bool,
    reply_message_id: Option<u64>,
    reply_lifecycle: ReplyLifecycle,
    reply_fade_task: Option<Task<()>>,
    request_task: Option<Task<()>>,
    request_abort: Option<ActiveRequestAbort>,
    pending_voice: Option<PendingVoice>,
    _input_subscription: Subscription,
}

struct ActiveRequestAbort {
    response_id: ResponseId,
    handle: AbortHandle,
}

struct PendingVoice {
    utterance_id: u64,
    agent_config_revision: u64,
    persona_swap_revision: u64,
    persona: String,
    settings: SharedLlmSettings,
    persona_settings: SharedPersonaSettings,
    language: AppLanguage,
}

struct PendingImage {
    attachment: ImageAttachment,
    preview: Arc<Image>,
}

struct ReplyDisplay {
    text: String,
    detail: Option<String>,
    waiting: bool,
    error: bool,
}

pub(super) struct AgentOverlayLayout {
    pub(super) horizontal_inset: f32,
    pub(super) control_size: f32,
    pub(super) reply_max_height: f32,
}

impl AgentOverlayLayout {
    pub(super) fn for_viewport(width: f32, height: f32) -> Self {
        let narrow = width < NARROW_OVERLAY_BREAKPOINT;
        Self {
            horizontal_inset: if narrow { 4.0 } else { 12.0 },
            control_size: if narrow { 28.0 } else { 32.0 },
            reply_max_height: (height - OVERLAY_BOTTOM_RESERVED - REPLY_VERTICAL_INSET * 2.0)
                .clamp(REPLY_MIN_HEIGHT, REPLY_MAX_HEIGHT),
        }
    }
}

pub(super) struct ReplyLifecycle {
    visible: bool,
    hovered: bool,
    fading: bool,
    revision: u64,
    display_generation: u64,
}

impl ReplyLifecycle {
    pub(super) fn new(visible: bool) -> Self {
        Self {
            visible,
            hovered: false,
            fading: false,
            revision: 0,
            display_generation: u64::from(visible),
        }
    }

    pub(super) fn visible(&self) -> bool {
        self.visible
    }

    pub(super) fn fading(&self) -> bool {
        self.fading
    }

    pub(super) fn revision(&self) -> u64 {
        self.revision
    }

    pub(super) fn display_generation(&self) -> u64 {
        self.display_generation
    }

    pub(super) fn reveal(&mut self) {
        self.advance();
        self.display_generation = self.display_generation.wrapping_add(1).max(1);
        self.visible = true;
        self.hovered = false;
        self.fading = false;
    }

    fn hide(&mut self) {
        self.advance();
        self.visible = false;
        self.hovered = false;
        self.fading = false;
    }

    pub(super) fn plan_fade(&mut self, terminal: bool) -> Option<u64> {
        self.advance();
        self.fading = false;
        (self.visible && !self.hovered && terminal).then_some(self.revision)
    }

    pub(super) fn begin_fade(&mut self, revision: u64, terminal: bool) -> bool {
        if self.revision != revision || !self.visible || self.hovered || !terminal {
            return false;
        }
        self.fading = true;
        true
    }

    pub(super) fn finish_fade(&mut self, revision: u64, terminal: bool) -> bool {
        if self.revision != revision || !self.visible || self.hovered || !terminal {
            return false;
        }
        self.visible = false;
        self.fading = false;
        true
    }

    pub(super) fn set_hovered(&mut self, hovered: bool) -> bool {
        if self.hovered == hovered {
            return false;
        }
        self.hovered = hovered;
        if hovered {
            self.advance();
            self.fading = false;
        }
        true
    }

    fn advance(&mut self) {
        self.revision = self.revision.wrapping_add(1).max(1);
    }
}

impl AgentView {
    /// 使用当前 LLM 配置创建回复浮层和单行输入框。
    #[expect(
        clippy::too_many_arguments,
        reason = "挂载时一次性交接已恢复的会话、存储、人格与记忆句柄，拆分反而会引入半初始化状态"
    )]
    pub(super) fn new(
        settings: SharedLlmSettings,
        persona: SharedPersonaSettings,
        active_persona: String,
        session: ChatSession,
        store: Arc<ChatSessionStore>,
        memory: AgentMemoryAccess,
        initial_status: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
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
        cx.on_release(|this, cx| {
            let shutdown = this.shutdown_snapshot();
            Tokio::spawn(cx, async move {
                if let Err(error) = shutdown.persist().await {
                    log::error!("{}", t!("log.chat_close_save_failed", error = error));
                }
            })
            .detach();
        })
        .detach();

        let reply_visible = initial_status.is_some();
        let active_limits = persona
            .personas
            .iter()
            .find(|candidate| candidate.id == active_persona)
            .map_or_else(ChatLimits::default, chat_limits);
        Self {
            session,
            persist_revision: store.latest_revision(),
            shutdown_revision: None,
            store,
            last_persist: Instant::now(),
            settings,
            persona,
            active_persona,
            active_limits,
            memory,
            agent_config_revision: 1,
            persona_swap_revision: 0,
            persona_swap_task: None,
            backend: Arc::new(GenaiChatBackend::new()),
            available_outfits: Vec::new(),
            outfit_revision: 0,
            input,
            pending_image: None,
            image_picker_revision: 0,
            image_picker_task: None,
            messages_scroll: ScrollHandle::new(),
            status: initial_status,
            input_visible: false,
            voice_indicator_visible: false,
            reply_message_id: None,
            reply_lifecycle: ReplyLifecycle::new(reply_visible),
            reply_fade_task: None,
            request_task: None,
            request_abort: None,
            pending_voice: None,
            _input_subscription: input_subscription,
        }
    }

    /// 用 fake backend 替换真实 Provider，使流式生命周期可在测试中确定性验证。
    #[cfg(test)]
    pub(super) fn set_backend_for_test(&mut self, backend: Arc<dyn ChatBackend>) {
        self.backend = backend;
    }

    /// 用当前已加载模型的服装名称替换 Agent 工具快照，并使迟到请求失效。
    pub(crate) fn set_available_outfits(&mut self, outfits: Vec<String>) {
        self.available_outfits = outfits;
        self.outfit_revision = self.outfit_revision.wrapping_add(1).max(1);
    }

    /// 检查已投递到 GPUI 队列的换装请求是否仍属于当前服装清单和活动工具调用。
    pub(crate) fn outfit_request_is_current(&self, request: &AgentOutfitRequest) -> bool {
        request.revision() == self.outfit_revision && !request.is_cancelled()
    }

    /// 返回当前回复浮层实际展示的文本，供测试断言状态与会话内容一致。
    #[cfg(test)]
    pub(super) fn reply_text_for_test(&self) -> Option<String> {
        self.reply_display().map(|display| display.text)
    }

    /// 返回当前是否仍有可接收流式增量的请求。
    #[cfg(test)]
    pub(super) fn is_streaming_for_test(&self) -> bool {
        self.is_streaming()
    }

    /// 返回会话中已记录的消息数量。
    #[cfg(test)]
    pub(super) fn message_count_for_test(&self) -> usize {
        self.session.messages().len()
    }

    /// 返回测试可观察的待提交语音 ID。
    #[cfg(test)]
    pub(super) fn pending_voice_for_test(&self) -> Option<u64> {
        self.pending_voice
            .as_ref()
            .map(|pending| pending.utterance_id)
    }

    /// 直接投递一条用户消息，跳过输入框与焦点管理。
    #[cfg(test)]
    pub(super) fn send_message_for_test(&mut self, text: &str, cx: &mut Context<Self>) -> bool {
        self.send_message_with_image(text.to_owned(), None, CONFIG.appearance().language, cx)
    }

    /// 从全局配置刷新供应商与人格；活动请求继续使用启动时的旧快照。
    ///
    /// 当前人格或其上下文限制发生变化时，先落盘旧人格的上下文，再异步换入新人格的
    /// 上下文；换入完成前拒绝新消息，避免两个人格的记忆互相污染。
    pub(crate) fn refresh_settings(&mut self, cx: &mut Context<Self>) {
        let settings = CONFIG.llm_settings();
        let persona = CONFIG.persona_settings();
        if !Arc::ptr_eq(&self.settings, &settings) || !Arc::ptr_eq(&self.persona, &persona) {
            self.pending_voice = None;
            self.agent_config_revision = self.agent_config_revision.wrapping_add(1).max(1);
        }
        self.settings = settings;
        self.persona = persona;
        let Some(active) = self.persona.active() else {
            cx.notify();
            return;
        };
        let next_persona = active.id.clone();
        let next_limits = chat_limits(active);
        if next_persona == self.active_persona && next_limits == self.active_limits {
            cx.notify();
            return;
        }
        self.swap_persona(next_persona, next_limits, cx);
    }

    /// 返回当前人格的系统提示词；人格缺失时退化为空提示词。
    fn active_system_prompt(&self) -> String {
        self.persona
            .personas
            .iter()
            .find(|persona| persona.id == self.active_persona)
            .map(|persona| persona.system_prompt.clone())
            .unwrap_or_default()
    }

    /// 返回当前人格实际使用的供应商；未绑定时回退到全局默认选择。
    fn active_model(&self) -> Option<LlmModelConfig> {
        let bound = self
            .persona
            .personas
            .iter()
            .find(|persona| persona.id == self.active_persona)
            .and_then(|persona| persona.model.as_deref());
        match bound {
            Some(id) => self
                .settings
                .model(id)
                .or_else(|| self.settings.selected())
                .cloned(),
            None => self.settings.selected().cloned(),
        }
    }

    fn swap_persona(
        &mut self,
        next_persona: String,
        next_limits: ChatLimits,
        cx: &mut Context<Self>,
    ) {
        self.pending_voice = None;
        self.cancel_network_request();
        self.session.interrupt_active_response();
        self.persist(true, cx);

        self.persona_swap_revision = self.persona_swap_revision.wrapping_add(1).max(1);
        let revision = self.persona_swap_revision;
        self.active_persona = next_persona.clone();
        self.active_limits = next_limits;
        let Some(database) = self.memory.database() else {
            // 数据库不可用时无法恢复上下文；换入一个空会话仍然优于继续沿用旧人格的记忆。
            self.session = ChatSession::new(next_limits).unwrap_or_default();
            self.persona_swap_task = None;
            cx.notify();
            return;
        };

        let load = Tokio::spawn(cx, async move {
            ChatSessionStore::load(database, &next_persona, next_limits)
                .await
                .map_err(|error| error.to_string())
        });
        self.persona_swap_task = Some(cx.spawn(async move |this, cx| {
            let loaded = load.await;
            let _ = this.update(cx, |this, cx| {
                // 换入期间可能又切换了一次人格；只有最新一次请求可以覆盖会话。
                if this.persona_swap_revision != revision {
                    return;
                }
                this.persona_swap_task = None;
                match loaded {
                    Ok(Ok((session, store))) => {
                        this.session = session;
                        this.persist_revision = store.latest_revision();
                        this.shutdown_revision = None;
                        this.store = store;
                        this.reply_message_id = None;
                        this.status = None;
                    }
                    Ok(Err(error)) => this.show_status(
                        t!("chat.persistence_unavailable", error = error).to_string(),
                        cx,
                    ),
                    Err(error) => this.show_status(
                        t!("chat.task_ended", kind = join_error_kind(&error)).to_string(),
                        cx,
                    ),
                }
                cx.notify();
            });
        }));
        cx.notify();
    }

    /// 清除指定人格的短期上下文。
    ///
    /// 会话文档只有本视图会写入，因此清除也统一在这里执行：当前人格直接清空内存并
    /// 落盘空快照，其他人格删除对应文档，两条路径都不会与后台写任务竞争。
    pub(crate) fn clear_persona_context(&mut self, persona_id: &str, cx: &mut Context<Self>) {
        if persona_id == self.active_persona {
            self.pending_voice = None;
            self.cancel_network_request();
            self.session.clear();
            self.reply_message_id = None;
            self.status = None;
            self.persist(true, cx);
            cx.notify();
            return;
        }
        let Some(database) = self.memory.database() else {
            return;
        };
        let persona_id = persona_id.to_owned();
        Tokio::spawn(cx, async move {
            if let Err(error) = super::store::delete_persona_session(&database, &persona_id).await {
                log::error!(
                    "{}",
                    t!("log.chat_close_save_failed", error = error.to_string())
                );
            }
        })
        .detach();
    }

    /// 显示或隐藏底部输入栏；显示时把键盘焦点交给单行输入框。
    pub(crate) fn set_input_visible(
        &mut self,
        visible: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.input_visible = visible;
        if visible {
            self.refresh_settings(cx);
            self.input.update(cx, |input, cx| input.focus(window, cx));
        } else {
            cx.notify();
        }
    }

    /// 返回回复层当前是否占用桌宠状态提示区域。
    pub(crate) fn reply_visible(&self) -> bool {
        self.reply_lifecycle.visible()
    }

    /// 为主窗口底部录音提示预留回复区域，避免波形遮住流式文本。
    pub(crate) fn set_voice_indicator_visible(&mut self, visible: bool, cx: &mut Context<Self>) {
        if self.voice_indicator_visible != visible {
            self.voice_indicator_visible = visible;
            cx.notify();
        }
    }

    /// 将模型部位点击作为当前人格会话中的一轮本地化事件发送给 Provider。
    pub(crate) fn send_model_click_event(
        &mut self,
        part_name: &str,
        language: AppLanguage,
        cx: &mut Context<Self>,
    ) -> bool {
        // 连续点击不能用 Busy 状态覆盖仍在流式更新的上一条回复。
        if self.is_streaming() {
            return false;
        }
        self.pending_voice = None;
        let prompt = model_click_event_prompt(part_name, language);
        self.send_message_with_image(prompt, None, language, cx)
    }

    /// 在 VAD 确认句首时立即打断当前回复，并登记唯一可接受的转写结果。
    pub(crate) fn voice_speech_started(
        &mut self,
        utterance_id: u64,
        language: AppLanguage,
        cx: &mut Context<Self>,
    ) {
        if self.shutdown_revision.is_some()
            || self
                .pending_voice
                .as_ref()
                .is_some_and(|pending| pending.utterance_id >= utterance_id)
        {
            return;
        }
        let settings = CONFIG.llm_settings();
        let persona_settings = CONFIG.persona_settings();
        if !Arc::ptr_eq(&self.settings, &settings) || !Arc::ptr_eq(&self.persona, &persona_settings)
        {
            self.refresh_settings(cx);
            return;
        }
        self.pending_voice = Some(PendingVoice {
            utterance_id,
            agent_config_revision: self.agent_config_revision,
            persona_swap_revision: self.persona_swap_revision,
            persona: self.active_persona.clone(),
            settings,
            persona_settings,
            language,
        });
        self.cancel_network_request();
        if let Some(response_id) = self.session.active_response_id()
            && self.session.interrupt_response_by_voice(response_id)
        {
            self.status = None;
            self.persist(true, cx);
            self.messages_scroll.scroll_to_bottom();
            self.reveal_reply(cx);
            self.schedule_reply_fade(cx);
        }
        cx.notify();
    }

    /// 只提交仍属于当前人格和最近一次录音的转写文本。
    pub(crate) fn send_voice_transcript(
        &mut self,
        utterance_id: u64,
        text: String,
        cx: &mut Context<Self>,
    ) -> bool {
        if !self
            .pending_voice
            .as_ref()
            .is_some_and(|pending| pending.utterance_id == utterance_id)
        {
            return false;
        }
        let Some(pending) = self.pending_voice.take() else {
            return false;
        };
        let settings = CONFIG.llm_settings();
        let persona_settings = CONFIG.persona_settings();
        if pending.agent_config_revision != self.agent_config_revision
            || pending.persona_swap_revision != self.persona_swap_revision
            || pending.persona != self.active_persona
            || !Arc::ptr_eq(&pending.settings, &settings)
            || !Arc::ptr_eq(&pending.persona_settings, &persona_settings)
            || self.persona_swap_task.is_some()
            || self.is_streaming()
        {
            return false;
        }
        self.settings = pending.settings;
        self.persona = pending.persona_settings;
        self.send_message_with_current_configuration(text, None, pending.language, cx)
    }

    /// 只撤销匹配的短录音或 VAD 误触，不影响随后开始的新 utterance。
    pub(crate) fn voice_utterance_cancelled(&mut self, utterance_id: u64) {
        if self
            .pending_voice
            .as_ref()
            .is_some_and(|pending| pending.utterance_id == utterance_id)
        {
            self.pending_voice = None;
        }
    }

    /// 配置切换、隐藏或关闭窗口时使尚未提交的转写失效。
    pub(crate) fn cancel_pending_voice(&mut self) {
        self.pending_voice = None;
    }

    /// 清除等待中的语音并复用回复浮层展示本地诊断。
    pub(crate) fn voice_failed(&mut self, message: String, cx: &mut Context<Self>) {
        self.pending_voice = None;
        self.show_status(message, cx);
    }

    /// 挂载后为启动阶段的持久化告警安排一次可取消淡出。
    pub(super) fn start_initial_reply_fade(&mut self, cx: &mut Context<Self>) {
        if self.status.is_some() && self.reply_message_id.is_none() {
            self.schedule_reply_fade(cx);
        }
    }

    /// 返回当前是否仍有可接收流式增量的请求。
    fn is_streaming(&self) -> bool {
        self.session.active_response_id().is_some()
    }

    /// 终止活动请求并返回退出流程必须等待写入的最后快照。
    pub(crate) fn shutdown_snapshot(&mut self) -> AgentShutdown {
        self.pending_voice = None;
        self.cancel_network_request();
        self.session.interrupt_active_response();
        let revision = match self.shutdown_revision {
            Some(revision) => revision,
            None => {
                self.persist_revision = self.persist_revision.saturating_add(1).max(1);
                self.shutdown_revision = Some(self.persist_revision);
                self.persist_revision
            }
        };
        AgentShutdown::new(self.store.clone(), self.session.snapshot(revision))
    }

    fn submit_from_input(
        &mut self,
        input: &Entity<InputState>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.pending_voice = None;
        let text = input.read(cx).value().to_string();
        let image = self
            .pending_image
            .as_ref()
            .map(|pending| pending.attachment.clone());
        let language = CONFIG.appearance().language;
        if self.send_message_with_image(text, image, language, cx) {
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

    fn choose_image(&mut self, cx: &mut Context<Self>) {
        if self.shutdown_revision.is_some() || self.is_streaming() {
            return;
        }
        self.image_picker_revision = self.image_picker_revision.wrapping_add(1).max(1);
        let revision = self.image_picker_revision;
        self.image_picker_task = None;
        let paths = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: Some(t!("chat.select_image").to_string().into()),
        });
        let background = cx.background_executor().clone();
        self.image_picker_task = Some(cx.spawn(async move |this, cx| {
            let path = match paths.await {
                Ok(Ok(Some(paths))) => paths.into_iter().next(),
                Ok(Ok(None)) => return,
                Ok(Err(_)) | Err(_) => {
                    let _ = this.update(cx, |this, cx| {
                        if this.image_picker_revision == revision {
                            this.show_status(t!("chat.error.image_picker").to_string(), cx);
                        }
                    });
                    return;
                }
            };
            let Some(path) = path else {
                return;
            };
            let loaded = background.spawn(async move { load_image(&path) }).await;
            let _ = this.update(cx, |this, cx| {
                if this.image_picker_revision != revision {
                    return;
                }
                match loaded {
                    Ok(attachment) => {
                        let Some(bytes) = attachment.bytes() else {
                            this.show_status(t!("chat.error.image_prepare").to_string(), cx);
                            return;
                        };
                        let preview =
                            Arc::new(Image::from_bytes(ImageFormat::Jpeg, bytes.to_vec()));
                        this.pending_image = Some(PendingImage {
                            attachment,
                            preview,
                        });
                        this.clear_status_reply(cx);
                    }
                    Err(error) => this.show_status(error.to_string(), cx),
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

    fn send_message_with_image(
        &mut self,
        text: String,
        image: Option<ImageAttachment>,
        language: AppLanguage,
        cx: &mut Context<Self>,
    ) -> bool {
        self.settings = CONFIG.llm_settings();
        self.persona = CONFIG.persona_settings();
        self.send_message_with_current_configuration(text, image, language, cx)
    }

    /// 使用调用方已经校验过的 Agent 快照创建请求，不在语音提交中重新绑定 Provider。
    fn send_message_with_current_configuration(
        &mut self,
        text: String,
        image: Option<ImageAttachment>,
        language: AppLanguage,
        cx: &mut Context<Self>,
    ) -> bool {
        if self.shutdown_revision.is_some() {
            return false;
        }
        // 人格换入期间会话尚未就位，此时发送会把消息写进即将被替换的上下文。
        if self.persona_swap_task.is_some() {
            self.show_status(t!("chat.persona_switching").to_string(), cx);
            return false;
        }
        let Some(model) = self.active_model() else {
            self.show_status(t!("chat.configure_model").to_string(), cx);
            return false;
        };
        let started = match self.session.start_turn_with_image(text, image) {
            Ok(started) => started,
            Err(error) => {
                self.show_status(error.to_string(), cx);
                return false;
            }
        };

        let response_id = started.response_id;
        let request = ChatServiceRequest {
            model,
            system_prompt: self.active_system_prompt(),
            messages: started.context,
            screenshot_permission_revision: CONFIG.agent_screenshot_permission_revision(),
            allow_agent_outfit_change: CONFIG.allow_agent_outfit_change(),
            outfits: self.available_outfits.clone(),
            outfit_revision: self.outfit_revision,
            language,
        };
        self.cancel_network_request();
        let (sender, mut receiver) = mpsc::channel(STREAM_CHANNEL_CAPACITY);
        let (abort_handle, abort_registration) = AbortHandle::new_pair();
        let network_abort = abort_handle.clone();
        let network_task = Tokio::spawn(
            cx,
            Abortable::new(self.backend.stream(request, sender), abort_registration),
        );
        self.request_abort = Some(ActiveRequestAbort {
            response_id,
            handle: abort_handle,
        });
        self.status = None;
        self.reply_message_id = self.session.messages().back().map(ChatMessage::id);
        self.reveal_reply(cx);
        self.persist(true, cx);

        self.request_task = Some(cx.spawn(async move |this, cx| {
            let network_task = network_task;
            while let Some(event) = receiver.next().await {
                let keep_receiving = this
                    .update(cx, |this, cx| {
                        let Some((keep_receiving, terminal)) =
                            this.apply_stream_event(response_id, event, cx)
                        else {
                            return false;
                        };
                        this.persist(terminal, cx);
                        this.messages_scroll.scroll_to_bottom();
                        if terminal {
                            this.schedule_reply_fade(cx);
                        }
                        cx.notify();
                        keep_receiving
                    })
                    .unwrap_or(false);
                if !keep_receiving {
                    network_abort.abort();
                    break;
                }
            }
            drop(receiver);
            network_abort.abort();

            let network_result = network_task.await;
            let _ = this.update(cx, |this, cx| {
                this.clear_request_abort(response_id);
                let failure = match network_result {
                    Ok(Ok(())) => t!("chat.stream_ended").to_string(),
                    Ok(Err(_)) => return,
                    Err(error) => t!("chat.task_ended", kind = join_error_kind(&error)).to_string(),
                };
                if this.session.fail_response(response_id, failure.clone()) {
                    this.status = Some(failure);
                    this.persist(true, cx);
                    this.messages_scroll.scroll_to_bottom();
                    this.schedule_reply_fade(cx);
                    cx.notify();
                }
            });
        }));
        true
    }

    fn apply_stream_event(
        &mut self,
        response_id: ResponseId,
        event: ChatStreamEvent,
        cx: &mut Context<Self>,
    ) -> Option<(bool, bool)> {
        if self.session.active_response_id() != Some(response_id) {
            return None;
        }
        Some(match event {
            ChatStreamEvent::Delta(chunk) => {
                if self.session.append_response(response_id, &chunk).is_err() {
                    // 回退状态与会话状态必须一致；`reply_display` 会优先展示消息本身。
                    let failure = t!("chat.reply_too_large").to_string();
                    self.session.fail_response(response_id, failure.clone());
                    self.status = Some(failure);
                    (false, true)
                } else {
                    (true, false)
                }
            }
            ChatStreamEvent::ChangeOutfit(request) => {
                if self.outfit_request_is_current(&request) {
                    cx.emit(AgentViewEvent::ChangeOutfit(request));
                } else {
                    request.complete(false);
                }
                (true, false)
            }
            ChatStreamEvent::Finished => {
                if !self.session.finish_response(response_id) {
                    return None;
                }
                (false, true)
            }
            ChatStreamEvent::Failed(message) => {
                if !self.session.fail_response(response_id, message.clone()) {
                    return None;
                }
                self.status = Some(message);
                (false, true)
            }
        })
    }

    fn stop(&mut self, cx: &mut Context<Self>) {
        if self.shutdown_revision.is_some() {
            return;
        }
        self.cancel_network_request();
        if let Some(response_id) = self.session.active_response_id() {
            self.session.cancel_response(response_id);
        }
        self.status = Some(t!("chat.generation_stopped").to_string());
        self.persist(true, cx);
        self.messages_scroll.scroll_to_bottom();
        self.schedule_reply_fade(cx);
        cx.notify();
    }

    fn reveal_reply(&mut self, cx: &mut Context<Self>) {
        self.reply_fade_task = None;
        self.reply_lifecycle.reveal();
        self.messages_scroll.scroll_to_bottom();
        cx.notify();
    }

    fn show_status(&mut self, status: String, cx: &mut Context<Self>) {
        self.status = Some(status);
        self.reply_message_id = None;
        self.reveal_reply(cx);
        self.schedule_reply_fade(cx);
    }

    fn clear_status_reply(&mut self, cx: &mut Context<Self>) {
        self.status = None;
        if self.reply_message_id.is_none() {
            self.reply_fade_task = None;
            self.reply_lifecycle.hide();
        }
        cx.notify();
    }

    fn schedule_reply_fade(&mut self, cx: &mut Context<Self>) {
        self.reply_fade_task = None;
        let terminal = self.visible_reply_is_terminal();
        let Some(revision) = self.reply_lifecycle.plan_fade(terminal) else {
            return;
        };

        let background = cx.background_executor().clone();
        self.reply_fade_task = Some(cx.spawn(async move |this, cx| {
            background.timer(REPLY_LINGER_DURATION).await;
            let should_fade = this
                .update(cx, |this, cx| {
                    let terminal = this.visible_reply_is_terminal();
                    if !this.reply_lifecycle.begin_fade(revision, terminal) {
                        return false;
                    }
                    cx.notify();
                    true
                })
                .unwrap_or(false);
            if !should_fade {
                return;
            }

            background.timer(REPLY_FADE_DURATION).await;
            let _ = this.update(cx, |this, cx| {
                let terminal = this.visible_reply_is_terminal();
                if this.reply_lifecycle.finish_fade(revision, terminal) {
                    cx.notify();
                }
            });
        }));
    }

    fn set_reply_hovered(&mut self, hovered: bool, cx: &mut Context<Self>) {
        if !self.reply_lifecycle.set_hovered(hovered) {
            return;
        }
        if hovered {
            self.reply_fade_task = None;
            cx.notify();
        } else {
            self.schedule_reply_fade(cx);
        }
    }

    fn visible_reply_is_terminal(&self) -> bool {
        let Some(message_id) = self.reply_message_id else {
            return self.status.is_some();
        };
        self.session
            .messages()
            .iter()
            .find(|message| message.id() == message_id && message.role() == ChatRole::Assistant)
            .is_some_and(|message| !matches!(message.state(), ChatMessageState::Streaming))
    }

    fn reply_display(&self) -> Option<ReplyDisplay> {
        if !self.reply_lifecycle.visible() {
            return None;
        }
        if let Some(message_id) = self.reply_message_id
            && let Some(message) =
                self.session.messages().iter().find(|message| {
                    message.id() == message_id && message.role() == ChatRole::Assistant
                })
        {
            let visible_content = message.visible_content();
            let waiting = visible_content.is_empty()
                && matches!(message.state(), ChatMessageState::Streaming);
            let text = if visible_content.is_empty() {
                match message.state() {
                    ChatMessageState::Streaming => t!("chat.thinking").to_string(),
                    ChatMessageState::Failed(error) => error.clone(),
                    ChatMessageState::Cancelled => t!("chat.stopped").to_string(),
                    ChatMessageState::Interrupted => t!("chat.interrupted").to_string(),
                    ChatMessageState::InterruptedByVoice => {
                        t!("chat.interrupted_by_voice").to_string()
                    }
                    ChatMessageState::Complete => String::new(),
                }
            } else {
                visible_content.to_owned()
            };
            let detail = match message.state() {
                ChatMessageState::Failed(error) if !message.content().is_empty() => {
                    Some(error.clone())
                }
                ChatMessageState::Cancelled if !message.content().is_empty() => {
                    Some(t!("chat.stopped").to_string())
                }
                ChatMessageState::Interrupted if !message.content().is_empty() => {
                    Some(t!("chat.interrupted").to_string())
                }
                ChatMessageState::InterruptedByVoice if !visible_content.is_empty() => {
                    Some(t!("chat.interrupted_by_voice").to_string())
                }
                _ => None,
            };
            return Some(ReplyDisplay {
                text,
                detail,
                waiting,
                error: matches!(message.state(), ChatMessageState::Failed(_)),
            });
        }

        self.status.as_ref().map(|status| ReplyDisplay {
            text: status.clone(),
            detail: None,
            waiting: false,
            error: false,
        })
    }

    fn cancel_network_request(&mut self) {
        if let Some(request) = self.request_abort.take() {
            request.handle.abort();
        }
        self.request_task = None;
    }

    fn clear_request_abort(&mut self, response_id: ResponseId) {
        if self
            .request_abort
            .as_ref()
            .is_some_and(|request| request.response_id == response_id)
        {
            self.request_abort = None;
        }
    }

    fn persist(&mut self, force: bool, cx: &Context<Self>) {
        // 设置窗口只能看到已发布的占用；无论是否真的写盘都要刷新，否则统计会停在旧值。
        self.memory
            .live_context_usage()
            .publish(&self.active_persona, self.session.usage());
        // 数据库不可用时启动状态已提示过一次，无需为每轮对话重复克隆快照并记录同一条错误。
        if !self.store.is_available() {
            return;
        }
        if !force && self.last_persist.elapsed() < PERSIST_INTERVAL {
            return;
        }
        self.persist_revision = self.persist_revision.saturating_add(1).max(1);
        self.last_persist = Instant::now();
        let snapshot = self.session.snapshot(self.persist_revision);
        let store = self.store.clone();
        Tokio::spawn(cx, async move {
            if let Err(error) = store.save(snapshot).await {
                log::error!("{}", t!("log.chat_save_failed", error = error));
            }
        })
        .detach();
    }
}

impl Render for AgentView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let palette = AgentPalette::from_app(cx);
        let viewport = window.viewport_size();
        let layout =
            AgentOverlayLayout::for_viewport(f32::from(viewport.width), f32::from(viewport.height));
        let streaming = self.is_streaming();
        let input_visible = self.input_visible;
        let voice_indicator_visible = self.voice_indicator_visible;
        let reply_fading = self.reply_lifecycle.fading();
        let reply_fade_revision = self.reply_lifecycle.revision();
        let reply_element_id = self.reply_lifecycle.display_generation();
        let reply = self.reply_display().map(|reply| {
            let primary_error = reply.error && reply.detail.is_none();
            let text = if reply.waiting {
                format!("{}...", reply.text)
            } else {
                reply.text
            };
            let bubble = div()
                .id(("agent-reply", reply_element_id))
                .w_full()
                .min_h(px(REPLY_MIN_HEIGHT))
                .max_h(px(layout.reply_max_height))
                .flex()
                .overflow_hidden()
                .rounded_lg()
                .border_1()
                .border_color(palette.primary.opacity(0.58))
                .bg(palette.popover.opacity(0.82))
                .shadow_md()
                .occlude()
                .on_hover(cx.listener(|this, hovered: &bool, _, cx| {
                    this.set_reply_hovered(*hovered, cx);
                }))
                .on_mouse_move(|_, _, cx| cx.stop_propagation())
                .on_mouse_down(MouseButton::Left, |_, window, cx| {
                    window.prevent_default();
                    cx.stop_propagation();
                })
                .on_click(|_, _, cx| cx.stop_propagation())
                .child(
                    div()
                        .id("agent-reply-output")
                        .w_full()
                        .max_h(px(layout.reply_max_height))
                        .overflow_y_scroll()
                        .track_scroll(&self.messages_scroll)
                        .px_3()
                        .py_2()
                        .child(
                            div()
                                .min_h(px(REPLY_CONTENT_MIN_HEIGHT))
                                .flex()
                                .flex_col()
                                .justify_center()
                                .text_center()
                                .text_sm()
                                .line_height(px(20.0))
                                .whitespace_normal()
                                .text_color(if primary_error {
                                    palette.danger
                                } else if reply.waiting {
                                    palette.muted_foreground
                                } else {
                                    palette.foreground
                                })
                                .when(reply.waiting, |this| this.font_medium())
                                .child(text)
                                .when_some(reply.detail, |this, detail| {
                                    this.child(
                                        div()
                                            .mt_1()
                                            .text_xs()
                                            .line_height(px(16.0))
                                            .text_color(if reply.error {
                                                palette.danger
                                            } else {
                                                palette.muted_foreground
                                            })
                                            .child(detail),
                                    )
                                }),
                        ),
                );

            let bubble = if reply_fading {
                bubble
                    .with_animation(
                        ("agent-reply-fade", reply_fade_revision),
                        Animation::new(REPLY_FADE_DURATION),
                        |this, delta| this.opacity(1.0 - delta),
                    )
                    .into_any_element()
            } else {
                bubble.into_any_element()
            };

            div()
                .absolute()
                .top(px(REPLY_VERTICAL_INSET))
                .right(px(layout.horizontal_inset))
                .bottom(px(if input_visible || voice_indicator_visible {
                    OVERLAY_BOTTOM_RESERVED
                } else {
                    REPLY_VERTICAL_INSET
                }))
                .left(px(layout.horizontal_inset))
                .flex()
                .items_center()
                .child(bubble)
                .into_any_element()
        });
        let pending_image = self
            .pending_image
            .as_ref()
            .map(|pending| pending.preview.clone());
        let attach_tooltip = t!("chat.attach_image").to_string();
        let remove_tooltip = t!("chat.remove_image").to_string();
        let image_control: AnyElement = if let Some(preview) = pending_image {
            div()
                .id("remove-chat-image")
                .size(px(layout.control_size))
                .flex_shrink_0()
                .overflow_hidden()
                .rounded_md()
                .border_1()
                .border_color(palette.primary)
                .when(!streaming, |this| {
                    this.cursor_pointer()
                        .hover(move |style| style.opacity(0.82))
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.remove_pending_image(cx);
                        }))
                })
                .tooltip(move |window, cx| Tooltip::new(remove_tooltip.clone()).build(window, cx))
                .child(img(preview).size_full().object_fit(ObjectFit::Cover))
                .into_any_element()
        } else {
            div()
                .id("attach-chat-image")
                .size(px(layout.control_size))
                .flex_shrink_0()
                .flex()
                .items_center()
                .justify_center()
                .rounded_md()
                .border_1()
                .border_color(if streaming {
                    palette.border
                } else {
                    palette.primary.opacity(0.82)
                })
                .bg(palette.secondary.opacity(0.92))
                .when(!streaming, |this| {
                    this.cursor_pointer()
                        .hover(move |style| style.bg(palette.accent).border_color(palette.primary))
                        .on_click(cx.listener(|this, _, _, cx| this.choose_image(cx)))
                })
                .tooltip(move |window, cx| Tooltip::new(attach_tooltip.clone()).build(window, cx))
                .child(
                    svg()
                        .path("icons/image-plus.svg")
                        .size_4()
                        .text_color(if streaming {
                            palette.muted_foreground
                        } else {
                            palette.primary
                        }),
                )
                .into_any_element()
        };

        div()
            .relative()
            .size_full()
            .text_color(palette.foreground)
            .when_some(reply, |this, reply| this.child(reply))
            .when(input_visible, |this| {
                this.child(
                    div()
                        .id("chat-input-bar")
                        .absolute()
                        .right(px(layout.horizontal_inset))
                        .bottom(px(56.0))
                        .left(px(layout.horizontal_inset))
                        .h(px(40.0))
                        .flex()
                        .items_center()
                        .gap_1()
                        .overflow_hidden()
                        .rounded_lg()
                        .border_1()
                        .border_color(palette.primary.opacity(0.62))
                        .bg(palette.popover.opacity(0.9))
                        .p_1()
                        .shadow_md()
                        .occlude()
                        .on_mouse_move(|_, _, cx| cx.stop_propagation())
                        .on_mouse_down(MouseButton::Left, |_, window, cx| {
                            window.prevent_default();
                            cx.stop_propagation();
                        })
                        .on_click(|_, _, cx| cx.stop_propagation())
                        .child(image_control)
                        .child(
                            div().min_w_0().flex_1().child(
                                Input::new(&self.input)
                                    .appearance(false)
                                    .focus_bordered(false)
                                    .disabled(streaming),
                            ),
                        )
                        .child(
                            div()
                                .id(if streaming { "stop-chat" } else { "send-chat" })
                                .size(px(layout.control_size))
                                .flex_shrink_0()
                                .flex()
                                .items_center()
                                .justify_center()
                                .rounded_md()
                                .bg(if streaming {
                                    palette.danger
                                } else {
                                    palette.primary
                                })
                                .cursor_pointer()
                                .hover(move |style| style.opacity(0.84))
                                .on_click(cx.listener(move |this, _, window, cx| {
                                    if streaming {
                                        this.stop(cx);
                                    } else {
                                        this.submit_current_input(window, cx);
                                    }
                                }))
                                .child(
                                    svg()
                                        .path(if streaming {
                                            "icons/square.svg"
                                        } else {
                                            "icons/send.svg"
                                        })
                                        .size_4()
                                        .text_color(if streaming {
                                            palette.danger_foreground
                                        } else {
                                            palette.primary_foreground
                                        }),
                                ),
                        ),
                )
            })
    }
}

impl EventEmitter<AgentViewEvent> for AgentView {}

fn join_error_kind(error: &gpui_tokio::JoinError) -> String {
    if error.is_cancelled() {
        t!("chat.task_cancelled").to_string()
    } else if error.is_panic() {
        t!("chat.task_panicked").to_string()
    } else {
        t!("chat.task_unknown").to_string()
    }
}
