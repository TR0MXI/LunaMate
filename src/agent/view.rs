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
    Animation, AnimationExt as _, AnyElement, AppContext, Context, Entity, Image, ImageFormat,
    IntoElement, MouseButton, ObjectFit, PathPromptOptions, Render, ScrollHandle, Subscription,
    Task, Window, div, img, prelude::*, px, svg,
};
use gpui_component::{
    StyledExt as _,
    input::{Input, InputEvent, InputState},
    tooltip::Tooltip,
};
use gpui_tokio::Tokio;
use rust_i18n::t;

use crate::config::{CONFIG, SharedLlmSettings};

use super::{
    AgentShutdown,
    media::{ImageAttachment, load_image},
    palette::AgentPalette,
    service::{ChatBackend, ChatServiceRequest, ChatStreamEvent, GenaiChatBackend},
    session::{ChatMessage, ChatMessageState, ChatRole, ChatSession, ResponseId},
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

/// 桌宠窗口中的单会话 Agent 覆盖层。
pub(crate) struct AgentView {
    session: ChatSession,
    store: Arc<ChatSessionStore>,
    persist_revision: u64,
    shutdown_revision: Option<u64>,
    last_persist: Instant,
    settings: SharedLlmSettings,
    backend: Arc<dyn ChatBackend>,
    input: Entity<InputState>,
    pending_image: Option<PendingImage>,
    image_picker_revision: u64,
    image_picker_task: Option<Task<()>>,
    messages_scroll: ScrollHandle,
    status: Option<String>,
    input_visible: bool,
    reply_message_id: Option<u64>,
    reply_lifecycle: ReplyLifecycle,
    reply_fade_task: Option<Task<()>>,
    request_task: Option<Task<()>>,
    request_abort: Option<ActiveRequestAbort>,
    _input_subscription: Subscription,
}

struct ActiveRequestAbort {
    response_id: ResponseId,
    handle: AbortHandle,
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
    pub(super) fn new(
        settings: SharedLlmSettings,
        session: ChatSession,
        store: Arc<ChatSessionStore>,
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
        Self {
            session,
            persist_revision: store.latest_revision(),
            shutdown_revision: None,
            store,
            last_persist: Instant::now(),
            settings,
            backend: Arc::new(GenaiChatBackend::new()),
            input,
            pending_image: None,
            image_picker_revision: 0,
            image_picker_task: None,
            messages_scroll: ScrollHandle::new(),
            status: initial_status,
            input_visible: false,
            reply_message_id: None,
            reply_lifecycle: ReplyLifecycle::new(reply_visible),
            reply_fade_task: None,
            request_task: None,
            request_abort: None,
            _input_subscription: input_subscription,
        }
    }

    /// 从全局配置刷新模型和系统提示词；活动请求继续使用启动时的旧快照。
    pub(crate) fn refresh_settings(&mut self, cx: &mut Context<Self>) {
        self.settings = CONFIG.llm_settings();
        cx.notify();
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
        let text = input.read(cx).value().to_string();
        let image = self
            .pending_image
            .as_ref()
            .map(|pending| pending.attachment.clone());
        if self.send_message_with_image(text, image, cx) {
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
        cx: &mut Context<Self>,
    ) -> bool {
        if self.shutdown_revision.is_some() {
            return false;
        }
        self.settings = CONFIG.llm_settings();
        let Some(model) = self.settings.selected().cloned() else {
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
            system_prompt: self.settings.system_prompt.clone(),
            messages: started.context,
            screenshot_permission_revision: CONFIG.agent_screenshot_permission_revision(),
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
                            this.apply_stream_event(response_id, event)
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
            let waiting = message.content().is_empty()
                && matches!(message.state(), ChatMessageState::Streaming);
            let text = if message.content().is_empty() {
                match message.state() {
                    ChatMessageState::Streaming => t!("chat.thinking").to_string(),
                    ChatMessageState::Failed(error) => error.clone(),
                    ChatMessageState::Cancelled => t!("chat.stopped").to_string(),
                    ChatMessageState::Interrupted => t!("chat.interrupted").to_string(),
                    ChatMessageState::Complete => String::new(),
                }
            } else {
                message.content().to_owned()
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
                .bottom(px(if input_visible {
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

fn join_error_kind(error: &gpui_tokio::JoinError) -> String {
    if error.is_cancelled() {
        t!("chat.task_cancelled").to_string()
    } else if error.is_panic() {
        t!("chat.task_panicked").to_string()
    } else {
        t!("chat.task_unknown").to_string()
    }
}
