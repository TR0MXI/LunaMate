//! 管理 Agent 回复浮层的布局、可见性、状态文本与淡出生命周期。

use std::time::Duration;

use gpui::Context;
use lunamate_agent::{ChatMessageState, ChatRole};
use rust_i18n::t;

use super::AgentView;

const REPLY_LINGER_DURATION: Duration = Duration::from_secs(4);
pub(super) const REPLY_FADE_DURATION: Duration = Duration::from_millis(800);
pub(super) const REPLY_MAX_HEIGHT: f32 = 180.0;
pub(super) const REPLY_CONTENT_MIN_HEIGHT: f32 = 60.0;
pub(super) const REPLY_MIN_HEIGHT: f32 = 78.0;
pub(super) const REPLY_VERTICAL_INSET: f32 = 12.0;
pub(super) const OVERLAY_BOTTOM_RESERVED: f32 = 108.0;
const NARROW_OVERLAY_BREAKPOINT: f32 = 180.0;

pub(in crate::ui) struct AgentOverlayLayout {
    pub(in crate::ui) horizontal_inset: f32,
    pub(in crate::ui) control_size: f32,
    pub(in crate::ui) reply_max_height: f32,
}

impl AgentOverlayLayout {
    pub(in crate::ui) fn for_viewport(width: f32, height: f32) -> Self {
        let narrow = width < NARROW_OVERLAY_BREAKPOINT;
        Self {
            horizontal_inset: if narrow { 4.0 } else { 12.0 },
            control_size: if narrow { 28.0 } else { 32.0 },
            reply_max_height: (height - OVERLAY_BOTTOM_RESERVED - REPLY_VERTICAL_INSET * 2.0)
                .clamp(REPLY_MIN_HEIGHT, REPLY_MAX_HEIGHT),
        }
    }
}

pub(in crate::ui) struct ReplyLifecycle {
    visible: bool,
    hovered: bool,
    fading: bool,
    revision: u64,
    display_generation: u64,
}

impl ReplyLifecycle {
    pub(in crate::ui) fn new(visible: bool) -> Self {
        Self {
            visible,
            hovered: false,
            fading: false,
            revision: 0,
            display_generation: u64::from(visible),
        }
    }

    pub(in crate::ui) fn visible(&self) -> bool {
        self.visible
    }

    pub(in crate::ui) fn fading(&self) -> bool {
        self.fading
    }

    pub(in crate::ui) fn revision(&self) -> u64 {
        self.revision
    }

    pub(in crate::ui) fn display_generation(&self) -> u64 {
        self.display_generation
    }

    pub(in crate::ui) fn reveal(&mut self) {
        self.advance();
        self.display_generation = self.display_generation.wrapping_add(1).max(1);
        self.visible = true;
        self.hovered = false;
        self.fading = false;
    }

    pub(in crate::ui) fn plan_fade(&mut self, terminal: bool) -> Option<u64> {
        self.advance();
        self.fading = false;
        (self.visible && !self.hovered && terminal).then_some(self.revision)
    }

    pub(in crate::ui) fn begin_fade(&mut self, revision: u64, terminal: bool) -> bool {
        if self.revision != revision || !self.visible || self.hovered || !terminal {
            return false;
        }
        self.fading = true;
        true
    }

    pub(in crate::ui) fn finish_fade(&mut self, revision: u64, terminal: bool) -> bool {
        if self.revision != revision || !self.visible || self.hovered || !terminal {
            return false;
        }
        self.visible = false;
        self.fading = false;
        true
    }

    pub(in crate::ui) fn set_hovered(&mut self, hovered: bool) -> bool {
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

pub(super) struct ReplyDisplay {
    pub(super) text: String,
    pub(super) detail: Option<String>,
    pub(super) waiting: bool,
    pub(super) error: bool,
}

impl AgentView {
    /// 返回当前回复浮层实际展示的文本，供测试断言状态与会话内容一致。
    #[cfg(test)]
    pub(in crate::ui) fn reply_text_for_test(&self) -> Option<String> {
        self.reply_display().map(|display| display.text)
    }

    /// 返回回复层当前是否占用桌宠状态提示区域。
    pub fn reply_visible(&self) -> bool {
        self.reply_lifecycle.visible()
    }

    /// 为主窗口底部录音提示预留回复区域，避免波形遮住流式文本。
    pub fn set_voice_indicator_visible(&mut self, visible: bool, cx: &mut Context<Self>) {
        if self.voice_indicator_visible != visible {
            self.voice_indicator_visible = visible;
            cx.notify();
        }
    }

    /// 挂载后为启动阶段的持久化告警安排一次可取消淡出。
    pub(crate) fn start_initial_reply_fade(&mut self, cx: &mut Context<Self>) {
        if self.snapshot.status().is_some() && self.snapshot.reply_message_id().is_none() {
            self.schedule_reply_fade(cx);
        }
    }

    pub(super) fn reveal_reply(&mut self, cx: &mut Context<Self>) {
        self.reply_fade_task = None;
        self.reply_lifecycle.reveal();
        self.messages_scroll.scroll_to_bottom();
        cx.notify();
    }

    pub(super) fn schedule_reply_fade(&mut self, cx: &mut Context<Self>) {
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

    pub(super) fn set_reply_hovered(&mut self, hovered: bool, cx: &mut Context<Self>) {
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
        let Some(message_id) = self.snapshot.reply_message_id() else {
            return self.snapshot.status().is_some();
        };
        self.snapshot
            .messages()
            .iter()
            .find(|message| message.id() == message_id && message.role() == ChatRole::Assistant)
            .is_some_and(|message| !matches!(message.state(), ChatMessageState::Streaming))
    }

    pub(super) fn reply_display(&self) -> Option<ReplyDisplay> {
        if !self.reply_lifecycle.visible() {
            return None;
        }
        if let Some(message_id) = self.snapshot.reply_message_id()
            && let Some(message) =
                self.snapshot.messages().iter().find(|message| {
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

        self.snapshot.status().map(|status| ReplyDisplay {
            text: status.to_owned(),
            detail: None,
            waiting: false,
            error: false,
        })
    }
}
