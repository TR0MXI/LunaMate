//! 承载设置主体的独立窗口壳层，处理标题栏拖动和窗口位置缓存。

use gpui::{
    AnyView, Context, Entity, IntoElement, MouseButton, Render, StyleRefinement, Subscription,
    Window, WindowControlArea, div, prelude::*, px, svg,
};
use gpui_component::StyledExt;

use crate::{
    config::ConfigWindow,
    platform::{WindowMover, WindowPositionController},
    ui::{UiPalette, cache_window_position},
};

use super::{SettingsEvent, SettingsView};

/// 为设置主体提供可拖动标题栏、关闭按钮和窗口位置采样。
pub(crate) struct SettingsWindowView {
    config: Entity<SettingsView>,
    window_mover: WindowMover,
    position_controller: WindowPositionController,
    _config_subscription: Subscription,
    _bounds_subscription: Subscription,
}

impl SettingsWindowView {
    /// 创建设置窗口根视图，并初始化窗口绑定的输入组件。
    pub(crate) fn new(
        config: Entity<SettingsView>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        config.update(cx, |config, cx| config.activate_window(window, cx));
        cache_window_position(window, ConfigWindow::Settings);
        let bounds_subscription = cx.observe_window_bounds(window, |this, window, _| {
            if !this.position_controller.observe_bounds() {
                return;
            }
            cache_window_position(window, ConfigWindow::Settings);
        });
        let config_subscription = cx.subscribe_in(
            &config,
            window,
            |this, config, event: &SettingsEvent, window, cx| match event {
                SettingsEvent::AgentChanged => {
                    config.update(cx, |config, cx| {
                        config.refresh_persona_providers(window, cx);
                    });
                }
                SettingsEvent::ModelCatalogChanged => {
                    config.update(cx, |config, cx| {
                        config.refresh_persona_live2d_models(window, cx);
                    });
                }
                SettingsEvent::WindowPositionsReset => {
                    this.position_controller.request_reset();
                    cx.notify();
                }
                _ => {}
            },
        );
        cx.on_release(|this, cx| {
            this.config
                .update(cx, |config, cx| config.deactivate_window(cx));
        })
        .detach();
        Self {
            config,
            window_mover: WindowMover::new(),
            position_controller: WindowPositionController::default(),
            _config_subscription: config_subscription,
            _bounds_subscription: bounds_subscription,
        }
    }
}

impl Render for SettingsWindowView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let palette = UiPalette::from_app(cx);
        if let Some(moved) = self.position_controller.apply_pending_reset(window, cx)
            && !moved
        {
            log::debug!("event=window_move_unsupported window=settings");
        }
        let config = self.config.clone();
        div().size_full().bg(gpui::transparent_black()).child(
            div()
                .size_full()
                .overflow_hidden()
                .rounded_lg()
                .flex()
                .flex_col()
                .bg(palette.background)
                .child(
                    div()
                        .id("settings-titlebar")
                        .h(px(38.0))
                        .flex_shrink_0()
                        .flex()
                        .items_center()
                        .justify_between()
                        .pl_4()
                        .pr_2()
                        .border_b_1()
                        .border_color(palette.border)
                        .bg(palette.sidebar)
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
                                |this, _, window, _| {
                                    this.window_mover.mouse_move(window);
                                },
                            ))
                        })
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .gap_2()
                                .text_sm()
                                .font_semibold()
                                .child(
                                    svg()
                                        .path("icons/settings.svg")
                                        .size_4()
                                        .text_color(palette.primary),
                                )
                                .child("LunaMate"),
                        )
                        .child(
                            div()
                                .id("close-settings-window")
                                .size(px(28.0))
                                .flex()
                                .items_center()
                                .justify_center()
                                .rounded_md()
                                .text_color(palette.foreground)
                                .cursor_pointer()
                                .window_control_area(WindowControlArea::Close)
                                .hover(move |style| {
                                    style
                                        .bg(palette.danger)
                                        .text_color(palette.danger_foreground)
                                })
                                .on_mouse_down(MouseButton::Left, |_, window, cx| {
                                    window.prevent_default();
                                    cx.stop_propagation();
                                })
                                .on_click(|_, window, cx| {
                                    cx.stop_propagation();
                                    window.remove_window();
                                })
                                .child(
                                    svg()
                                        .path("icons/x.svg")
                                        .size_4()
                                        .text_color(palette.foreground),
                                ),
                        ),
                )
                .child(
                    div().flex_1().min_h_0().child(
                        AnyView::from(config).cached(StyleRefinement::default().size_full()),
                    ),
                ),
        )
    }
}
