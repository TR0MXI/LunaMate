//! 渲染桌宠窗口内的聊天记录和输入框，并拥有网络请求生命周期。

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
    AnyElement, AppContext, Context, Entity, IntoElement, MouseButton, Render, ScrollHandle,
    Subscription, Task, Window, div, prelude::*, px, svg,
};
use gpui_component::{
    StyledExt as _,
    input::{Input, InputEvent, InputState},
};
use gpui_tokio::Tokio;
use rust_i18n::t;

use crate::{
    config::{CONFIG, SharedLlmSettings},
    theme::UiPalette,
};

use super::{
    ChatMessage, ChatMessageState, ChatRole, ChatSession,
    service::{ChatBackend, ChatServiceRequest, ChatStreamEvent, GenaiChatBackend},
    store::ChatSessionStore,
};

const STREAM_CHANNEL_CAPACITY: usize = 16;
const PERSIST_INTERVAL: Duration = Duration::from_secs(3);

/// 桌宠窗口中的单会话聊天视图。
pub(crate) struct ChatView {
    session: ChatSession,
    store: Arc<ChatSessionStore>,
    persist_revision: u64,
    last_persist: Instant,
    settings: SharedLlmSettings,
    backend: Arc<dyn ChatBackend>,
    input: Entity<InputState>,
    messages_scroll: ScrollHandle,
    status: Option<String>,
    request_task: Option<Task<()>>,
    request_abort: Option<ActiveRequestAbort>,
    _input_subscription: Subscription,
}

struct ActiveRequestAbort {
    response_id: super::ResponseId,
    handle: AbortHandle,
}

impl ChatView {
    /// 使用当前 LLM 配置创建聊天视图和多行输入框。
    pub(crate) fn new(
        settings: SharedLlmSettings,
        session: ChatSession,
        store: Arc<ChatSessionStore>,
        initial_status: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let input = cx.new(|cx| {
            InputState::new(window, cx)
                .auto_grow(1, 4)
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
            let (store, snapshot) = this.shutdown_snapshot();
            cx.background_executor()
                .spawn(async move {
                    if let Err(error) = store.save(snapshot) {
                        log::error!("聊天视图关闭时保存会话失败：{error}");
                    }
                })
                .detach();
        })
        .detach();

        Self {
            session,
            persist_revision: store.latest_revision(),
            store,
            last_persist: Instant::now(),
            settings,
            backend: Arc::new(GenaiChatBackend::new()),
            input,
            messages_scroll: ScrollHandle::new(),
            status: initial_status,
            request_task: None,
            request_abort: None,
            _input_subscription: input_subscription,
        }
    }

    /// 更新模型和系统提示词；活动请求继续使用启动时的旧快照。
    pub(crate) fn update_settings(&mut self, settings: SharedLlmSettings, cx: &mut Context<Self>) {
        self.settings = settings;
        cx.notify();
    }

    /// 面板打开后把键盘焦点交给输入框。
    pub(crate) fn focus_input(&self, window: &mut Window, cx: &mut Context<Self>) {
        self.input.update(cx, |input, cx| input.focus(window, cx));
    }

    /// 返回当前是否仍有可接收流式增量的请求。
    fn is_streaming(&self) -> bool {
        self.session.active_response_id().is_some()
    }

    /// 终止活动请求并返回退出流程必须等待写入的最后快照。
    pub(crate) fn shutdown_snapshot(
        &mut self,
    ) -> (Arc<ChatSessionStore>, super::session::ChatSessionSnapshot) {
        self.cancel_network_request();
        self.session.interrupt_active_response();
        self.persist_revision = self.persist_revision.saturating_add(1);
        (
            self.store.clone(),
            self.session.snapshot(self.persist_revision),
        )
    }

    fn submit_from_input(
        &mut self,
        input: &Entity<InputState>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let text = input.read(cx).value().to_string();
        if self.submit_message(text, cx) {
            input.update(cx, |input, cx| input.set_value("", window, cx));
        }
    }

    fn submit_current_input(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let input = self.input.clone();
        self.submit_from_input(&input, window, cx);
    }

    fn submit_message(&mut self, text: String, cx: &mut Context<Self>) -> bool {
        self.settings = CONFIG.llm_settings();
        let Some(model) = self.settings.selected().cloned() else {
            self.status = Some(t!("chat.configure_model").to_string());
            cx.notify();
            return false;
        };
        let started = match self.session.start_turn(text) {
            Ok(started) => started,
            Err(error) => {
                self.status = Some(error.to_string());
                cx.notify();
                return false;
            }
        };

        let response_id = started.response_id;
        let request = ChatServiceRequest {
            model,
            system_prompt: self.settings.system_prompt.clone(),
            messages: started.context,
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
        self.persist(true, cx);
        self.messages_scroll.scroll_to_bottom();
        cx.notify();

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
                    cx.notify();
                }
            });
        }));
        true
    }

    fn apply_stream_event(
        &mut self,
        response_id: super::ResponseId,
        event: ChatStreamEvent,
    ) -> Option<(bool, bool)> {
        if self.session.active_response_id() != Some(response_id) {
            return None;
        }
        Some(match event {
            ChatStreamEvent::Delta(chunk) => {
                if let Err(error) = self.session.append_response(response_id, &chunk) {
                    self.session
                        .fail_response(response_id, t!("chat.reply_too_large").to_string());
                    self.status = Some(error.to_string());
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
        self.cancel_network_request();
        if let Some(response_id) = self.session.active_response_id() {
            self.session.cancel_response(response_id);
        }
        self.status = Some(t!("chat.generation_stopped").to_string());
        self.persist(true, cx);
        cx.notify();
    }

    fn clear(&mut self, cx: &mut Context<Self>) {
        self.cancel_network_request();
        self.session.clear();
        self.status = None;
        self.persist(true, cx);
        cx.notify();
    }

    fn cancel_network_request(&mut self) {
        if let Some(request) = self.request_abort.take() {
            request.handle.abort();
        }
        self.request_task = None;
    }

    fn clear_request_abort(&mut self, response_id: super::ResponseId) {
        if self
            .request_abort
            .as_ref()
            .is_some_and(|request| request.response_id == response_id)
        {
            self.request_abort = None;
        }
    }

    fn persist(&mut self, force: bool, cx: &Context<Self>) {
        if !force && self.last_persist.elapsed() < PERSIST_INTERVAL {
            return;
        }
        self.persist_revision = self.persist_revision.saturating_add(1).max(1);
        self.last_persist = Instant::now();
        let snapshot = self.session.snapshot(self.persist_revision);
        let store = self.store.clone();
        cx.background_executor()
            .spawn(async move {
                if let Err(error) = store.save(snapshot) {
                    log::error!("保存聊天会话失败：{error}");
                }
            })
            .detach();
    }

    fn render_message(message: &ChatMessage, palette: UiPalette) -> AnyElement {
        let user = message.role() == ChatRole::User;
        let content = if message.content().is_empty() {
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
        let state_message = match message.state() {
            ChatMessageState::Failed(error) if !message.content().is_empty() => Some(error.clone()),
            ChatMessageState::Cancelled if !message.content().is_empty() => {
                Some(t!("chat.stopped").to_string())
            }
            ChatMessageState::Interrupted if !message.content().is_empty() => {
                Some(t!("chat.interrupted").to_string())
            }
            _ => None,
        };

        div()
            .id(("chat-message", message.id()))
            .w_full()
            .flex()
            .when(user, |this| this.justify_end())
            .child(
                div()
                    .max_w(px(250.0))
                    .min_w_0()
                    .rounded_md()
                    .border_1()
                    .border_color(if user {
                        palette.primary
                    } else {
                        palette.border
                    })
                    .bg(if user {
                        palette.accent
                    } else {
                        palette.secondary
                    })
                    .px_3()
                    .py_2()
                    .text_sm()
                    .whitespace_normal()
                    .text_color(palette.foreground)
                    .child(content)
                    .when_some(state_message, |this, state| {
                        this.child(
                            div()
                                .mt_1()
                                .text_xs()
                                .text_color(palette.danger)
                                .child(state),
                        )
                    }),
            )
            .into_any_element()
    }
}

impl Render for ChatView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let palette = UiPalette::from_app(cx);
        let streaming = self.is_streaming();
        let messages = self.session.messages();
        let selected_model = self
            .settings
            .selected()
            .map(|model| model.label.clone())
            .unwrap_or_else(|| t!("chat.unconfigured").to_string());
        let status = self.status.clone();

        div()
            .size_full()
            .min_h_0()
            .flex()
            .flex_col()
            .text_color(palette.foreground)
            .child(
                div()
                    .h(px(42.0))
                    .flex_shrink_0()
                    .flex()
                    .items_center()
                    .justify_between()
                    .border_b_1()
                    .border_color(palette.border)
                    .px_3()
                    .child(
                        div()
                            .min_w_0()
                            .overflow_hidden()
                            .text_ellipsis()
                            .text_sm()
                            .font_medium()
                            .child(selected_model),
                    )
                    .child(
                        div()
                            .id("clear-chat")
                            .rounded_md()
                            .px_2()
                            .py_1()
                            .text_xs()
                            .text_color(palette.muted_foreground)
                            .cursor_pointer()
                            .hover(move |style| style.bg(palette.accent))
                            .on_click(cx.listener(|this, _, _, cx| this.clear(cx)))
                            .child(t!("common.clear").to_string()),
                    ),
            )
            .child(
                div()
                    .id("chat-output")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .track_scroll(&self.messages_scroll)
                    .p_3()
                    .flex()
                    .flex_col()
                    .gap_2()
                    .when(messages.is_empty(), |this| {
                        this.child(
                            div()
                                .flex_1()
                                .flex()
                                .items_center()
                                .justify_center()
                                .text_sm()
                                .text_color(palette.muted_foreground)
                                .child(t!("chat.start").to_string()),
                        )
                    })
                    .children(
                        messages
                            .iter()
                            .map(|message| Self::render_message(message, palette)),
                    ),
            )
            .when_some(status, |this, status| {
                this.child(
                    div()
                        .flex_shrink_0()
                        .border_t_1()
                        .border_color(palette.border)
                        .px_3()
                        .py_1()
                        .text_xs()
                        .text_color(palette.info)
                        .child(status),
                )
            })
            .child(
                div()
                    .flex_shrink_0()
                    .flex()
                    .items_end()
                    .gap_2()
                    .border_t_1()
                    .border_color(palette.border)
                    .p_2()
                    .child(
                        div()
                            .min_w_0()
                            .flex_1()
                            .child(Input::new(&self.input).disabled(streaming)),
                    )
                    .child(
                        div()
                            .id(if streaming { "stop-chat" } else { "send-chat" })
                            .size_9()
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
                            .hover(move |style| style.bg(palette.accent))
                            .on_mouse_down(MouseButton::Left, |_, window, cx| {
                                window.prevent_default();
                                cx.stop_propagation();
                            })
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
