//! 渲染 Agent 回复浮层、图片附件控件与底部输入栏。

use std::time::Duration;

use gpui::{
    Animation, AnimationExt as _, AnyElement, BoxShadow, Context, IntoElement, MouseButton,
    ObjectFit, Render, Window, bounce, div, ease_in_out, img, prelude::*, px, svg,
};
use gpui_component::{input::Input, tooltip::Tooltip};
use rust_i18n::t;

use crate::ui::UiPalette;

use super::{
    AgentOverlayLayout, AgentView, ThinkingFeedback,
    reply::{
        OVERLAY_BOTTOM_RESERVED, REPLY_CONTENT_MIN_HEIGHT, REPLY_FADE_DURATION, REPLY_MIN_HEIGHT,
        REPLY_VERTICAL_INSET,
    },
};

impl Render for AgentView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let palette = UiPalette::from_app(cx);
        let viewport = window.viewport_size();
        let layout =
            AgentOverlayLayout::for_viewport(f32::from(viewport.width), f32::from(viewport.height));
        let streaming = self.snapshot.is_streaming();
        let input_visible = self.input_visible;
        let voice_indicator_visible = self.voice_indicator_visible;
        let reply_fading = self.reply_lifecycle.fading();
        let reply_fade_revision = self.reply_lifecycle.revision();
        let reply_element_id = self.reply_lifecycle.display_generation();
        let reply = self.reply_display().map(|reply| {
            let primary_error = reply.error && reply.detail.is_none();
            let text = reply.text;
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
                                } else {
                                    palette.foreground
                                })
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
        let input_bar = if input_visible {
            let pending_image = self
                .pending_image
                .as_ref()
                .map(|pending| pending.preview.clone());
            let attach_tooltip = t!("chat.attach_image").to_string();
            let remove_tooltip = t!("chat.remove_image").to_string();
            let thinking_feedback = self.thinking_feedback();
            let image_control: AnyElement =
                if let Some(preview) = pending_image {
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
                        .tooltip(move |window, cx| {
                            Tooltip::new(remove_tooltip.clone()).build(window, cx)
                        })
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
                                .hover(move |style| {
                                    style.bg(palette.accent).border_color(palette.primary)
                                })
                                .on_click(cx.listener(|this, _, _, cx| this.choose_image(cx)))
                        })
                        .tooltip(move |window, cx| {
                            Tooltip::new(attach_tooltip.clone()).build(window, cx)
                        })
                        .child(svg().path("icons/image-plus.svg").size_4().text_color(
                            if streaming {
                                palette.muted_foreground
                            } else {
                                palette.primary
                            },
                        ))
                        .into_any_element()
                };

            let input_bar = div()
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
                );
            let input_bar: AnyElement = if thinking_feedback == Some(ThinkingFeedback::Text) {
                input_bar
                    .with_animation(
                        "chat-input-breathing",
                        Animation::new(Duration::from_millis(2_400))
                            .repeat()
                            .with_easing(bounce(ease_in_out)),
                        move |this, delta| {
                            this.border_color(palette.primary.opacity(0.58 + delta * 0.36))
                                .shadow(vec![
                                    BoxShadow::new(
                                        px(0.0),
                                        px(0.0),
                                        palette.primary.opacity(0.1 + delta * 0.18),
                                    )
                                    .blur_radius(px(8.0 + delta * 8.0))
                                    .spread_radius(px(delta * 1.5)),
                                ])
                        },
                    )
                    .into_any_element()
            } else {
                input_bar.into_any_element()
            };
            Some(input_bar)
        } else {
            None
        };

        div()
            .relative()
            .size_full()
            .text_color(palette.foreground)
            .when_some(reply, |this, reply| this.child(reply))
            .when_some(input_bar, |this, input_bar| this.child(input_bar))
    }
}
