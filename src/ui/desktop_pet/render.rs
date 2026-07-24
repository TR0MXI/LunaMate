//! 渲染桌宠根视图，并把 GPUI 输入转发给当前模型 generation。

use gpui::{
    ClickEvent, Context, IntoElement, MouseButton, MouseMoveEvent, ObjectFit, Render,
    StyleRefinement, StyledImage, Window, WindowControlArea, div, img, prelude::*, px, svg,
};
use gpui_component::StyledExt as _;
use rust_i18n::t;

use super::DesktopPetView;
use crate::ui::UiPalette;

const CHAT_PANEL_HEIGHT: f32 = 220.0;

impl Render for DesktopPetView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if !self.apply_pending_model_window_size(window, cx) {
            self.synchronize_render_dimensions(window, cx);
        }
        if let Some(moved) = self.position_controller.apply_pending_reset(window, cx)
            && !moved
        {
            log::debug!("{}", t!("log.pet_move_unsupported"));
        }
        let palette = UiPalette::from_app(cx);
        let control_background = palette.secondary;
        let control_hover = palette.accent;
        let frame = self.frame.clone();
        let rendered_image = frame.as_ref().and_then(|frame| frame.image().cloned());
        self.track_rendered_image(rendered_image.clone(), window);
        let model_message = self.model_state.message();
        let chat = self.chat.clone();
        let chat_open = self.chat_open;
        let model_diagnostics_message = (!chat_open)
            .then(|| self.model_state.diagnostics_message())
            .flatten();
        let model_generation = self.model_generation;
        let show_fps = self.show_fps;
        let actual_fps = self.actual_fps;
        let diagnostics_top = if show_fps { px(42.0) } else { px(12.0) };

        div()
            .relative()
            .size_full()
            .on_mouse_move(cx.listener(|this, event: &MouseMoveEvent, window, _| {
                this.update_look_target(event.position, window);
            }))
            .on_mouse_exit(cx.listener(|this, _, _, _| this.reset_look_target()))
            .when_some(frame, |this, frame| {
                let painted_frame = frame.clone();
                let uses_native_surface = frame.image().is_none();
                let model = div()
                    .id(("live2d-model", model_generation))
                    .size_full()
                    .on_click(cx.listener(move |this, event: &ClickEvent, window, cx| {
                        let ClickEvent::Mouse(mouse) = event else {
                            return;
                        };
                        if mouse.up.button != MouseButton::Left {
                            return;
                        }

                        let hit_frame = if uses_native_surface {
                            this.frame.clone()
                        } else {
                            Some(painted_frame.clone())
                        };
                        if hit_frame.as_ref().is_some_and(|frame| {
                            this.activate_hit_area_at(
                                frame,
                                model_generation,
                                mouse.up.position,
                                window,
                            )
                        }) {
                            cx.stop_propagation();
                        }
                    }));
                if let Some(image) = frame.image().cloned() {
                    this.child(model.child(img(image).size_full().object_fit(ObjectFit::Contain)))
                } else {
                    this.child(model)
                }
            })
            .when_some(model_message, |this, message| {
                this.child(
                    div()
                        .absolute()
                        .top_0()
                        .right_0()
                        .bottom_0()
                        .left_0()
                        .flex()
                        .items_center()
                        .justify_center()
                        .child(
                            div()
                                .id("open-settings-empty-state")
                                .mx_6()
                                .rounded_xl()
                                .border_1()
                                .border_color(palette.border)
                                .bg(palette.popover)
                                .p_4()
                                .text_center()
                                .text_sm()
                                .text_color(palette.foreground)
                                .cursor_pointer()
                                .on_mouse_down(MouseButton::Left, |_, window, cx| {
                                    window.prevent_default();
                                    cx.stop_propagation();
                                })
                                .on_click(cx.listener(|this, _, _, cx| {
                                    cx.stop_propagation();
                                    this.open_config_window(cx);
                                }))
                                .child(message),
                        ),
                )
            })
            .when(show_fps, |this| {
                this.child(
                    div()
                        .id("runtime-fps")
                        .absolute()
                        .top_3()
                        .left_3()
                        .min_w(px(72.0))
                        .h(px(26.0))
                        .flex()
                        .items_center()
                        .justify_center()
                        .rounded_md()
                        .border_1()
                        .border_color(palette.border)
                        .bg(palette.popover.opacity(0.9))
                        .px_2()
                        .text_xs()
                        .font_medium()
                        .text_color(palette.foreground)
                        .child(format!("{actual_fps:.1} FPS")),
                )
            })
            .when_some(model_diagnostics_message, |this, message| {
                this.child(
                    div()
                        .absolute()
                        .top(diagnostics_top)
                        .left_3()
                        .right(px(56.0))
                        .rounded_md()
                        .border_1()
                        .border_color(palette.warning)
                        .bg(palette.muted)
                        .px_3()
                        .py_2()
                        .text_xs()
                        .text_color(palette.warning_foreground)
                        .child(message),
                )
            })
            .when(chat_open, |this| {
                this.child(
                    div()
                        .id("chat-panel")
                        .absolute()
                        .h(px(CHAT_PANEL_HEIGHT))
                        .right(px(12.0))
                        .bottom(px(56.0))
                        .left(px(12.0))
                        .overflow_hidden()
                        .rounded_lg()
                        .border_1()
                        .border_color(palette.border)
                        .bg(palette.popover)
                        .shadow_lg()
                        .on_mouse_down(MouseButton::Left, |_, window, cx| {
                            window.prevent_default();
                            cx.stop_propagation();
                        })
                        .on_click(|_, _, cx| cx.stop_propagation())
                        .child(
                            gpui::AnyView::from(chat)
                                .cached(StyleRefinement::default().size_full()),
                        ),
                )
            })
            .child(
                div()
                    .id("close-window")
                    .absolute()
                    .top_3()
                    .right_3()
                    .flex()
                    .size_9()
                    .items_center()
                    .justify_center()
                    .rounded_full()
                    .bg(control_background)
                    .border_1()
                    .border_color(palette.border)
                    .text_color(palette.foreground)
                    .cursor_pointer()
                    .hover(move |style| {
                        style
                            .bg(palette.danger)
                            .text_color(palette.danger_foreground)
                    })
                    .on_mouse_down(MouseButton::Left, |_, window, cx| {
                        window.prevent_default();
                        cx.stop_propagation();
                    })
                    .on_click(|_, _, cx| {
                        cx.stop_propagation();
                        cx.quit();
                    })
                    .child(
                        svg()
                            .path("icons/x.svg")
                            .size_4()
                            .text_color(palette.foreground),
                    ),
            )
            .child(
                div()
                    .id("toggle-chat")
                    .absolute()
                    .left_3()
                    .bottom_3()
                    .flex()
                    .size_9()
                    .items_center()
                    .justify_center()
                    .rounded_full()
                    .bg(if chat_open {
                        palette.primary
                    } else {
                        control_background
                    })
                    .border_1()
                    .border_color(palette.border)
                    .cursor_pointer()
                    .hover(move |style| style.bg(control_hover))
                    .on_mouse_down(MouseButton::Left, |_, window, cx| {
                        window.prevent_default();
                        cx.stop_propagation();
                    })
                    .on_click(cx.listener(|this, _, window, cx| {
                        cx.stop_propagation();
                        this.toggle_chat(window, cx);
                    }))
                    .child(
                        svg()
                            .path("icons/message-circle.svg")
                            .size_4()
                            .text_color(palette.foreground),
                    ),
            )
            .when(!chat_open, |this| {
                this.child(
                    div()
                        .id("toggle-settings")
                        .absolute()
                        .right_3()
                        .bottom(px(56.0))
                        .flex()
                        .size_9()
                        .items_center()
                        .justify_center()
                        .rounded_full()
                        .bg(control_background)
                        .border_1()
                        .border_color(palette.border)
                        .cursor_pointer()
                        .hover(move |style| style.bg(control_hover))
                        .on_mouse_down(MouseButton::Left, |_, window, cx| {
                            window.prevent_default();
                            cx.stop_propagation();
                        })
                        .on_click(cx.listener(|this, _, _, cx| {
                            cx.stop_propagation();
                            this.open_config_window(cx);
                        }))
                        .child(
                            svg()
                                .path("icons/settings.svg")
                                .size_4()
                                .text_color(palette.foreground),
                        ),
                )
            })
            .child(
                div()
                    .id("move-window")
                    .absolute()
                    .right_3()
                    .bottom_3()
                    .flex()
                    .size_9()
                    .items_center()
                    .justify_center()
                    .rounded_full()
                    .bg(control_background)
                    .border_1()
                    .border_color(palette.border)
                    .cursor_grab()
                    .hover(move |style| style.bg(control_hover))
                    .window_control_area(WindowControlArea::Drag)
                    .when(!cfg!(target_os = "windows"), |this| {
                        this.on_mouse_down(
                            MouseButton::Left,
                            cx.listener(|this, _, _window, cx| {
                                this.window_mover.mouse_down();
                                cx.stop_propagation();
                            }),
                        )
                        .on_mouse_up(
                            MouseButton::Left,
                            cx.listener(|this, _, _, cx| {
                                this.window_mover.mouse_up();
                                cx.stop_propagation();
                            }),
                        )
                        .on_mouse_up_out(
                            MouseButton::Left,
                            cx.listener(|this, _, _, _| this.window_mover.mouse_up()),
                        )
                        .on_mouse_move(cx.listener(
                            |this, _, window, cx| {
                                this.window_mover.mouse_move(window);
                                cx.stop_propagation();
                            },
                        ))
                    })
                    .child(
                        svg()
                            .path("icons/move.svg")
                            .size_4()
                            .text_color(palette.foreground),
                    ),
            )
    }
}
