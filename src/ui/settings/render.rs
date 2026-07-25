//! 组装设置主体布局，并根据当前侧栏分区调度页面渲染。

use gpui::{
    AnyElement, AnyView, Context, IntoElement, Render, StyleRefinement, Window, div, prelude::*, px,
};
use rust_i18n::t;

use crate::ui::UiPalette;

use super::{ConfigSection, SettingsView, components::sidebar_button};

impl SettingsView {
    fn render_sidebar(&self, cx: &mut Context<Self>) -> AnyElement {
        let palette = UiPalette::from_app(cx);
        let model_active = self.section == ConfigSection::Model;
        let conversation_active = self.section == ConfigSection::Conversation;
        let tool_active = self.section == ConfigSection::Tool;
        let system_active = self.section == ConfigSection::System;
        let debug_active = self.section == ConfigSection::Debug;
        div()
            .w(px(160.0))
            .h_full()
            .flex_shrink_0()
            .flex()
            .flex_col()
            .border_r_1()
            .border_color(palette.border)
            .bg(palette.sidebar)
            .child(div().flex().flex_col().gap_1().px_2().pt_3().children([
                sidebar_button(
                    "section-model",
                    t!("settings.model").to_string(),
                    model_active,
                    palette,
                    cx.listener(
                        |this: &mut SettingsView,
                         _: &gpui::ClickEvent,
                         _: &mut Window,
                         cx: &mut Context<SettingsView>| {
                            this.section = ConfigSection::Model;
                            cx.notify();
                        },
                    ),
                ),
                sidebar_button(
                    "section-conversation",
                    t!("settings.conversation").to_string(),
                    conversation_active,
                    palette,
                    cx.listener(
                        |this: &mut SettingsView,
                         _: &gpui::ClickEvent,
                         _: &mut Window,
                         cx: &mut Context<SettingsView>| {
                            this.section = ConfigSection::Conversation;
                            cx.notify();
                        },
                    ),
                ),
                sidebar_button(
                    "section-tool",
                    t!("settings.tool").to_string(),
                    tool_active,
                    palette,
                    cx.listener(
                        |this: &mut SettingsView,
                         _: &gpui::ClickEvent,
                         _: &mut Window,
                         cx: &mut Context<SettingsView>| {
                            this.section = ConfigSection::Tool;
                            cx.notify();
                        },
                    ),
                ),
                sidebar_button(
                    "section-system",
                    t!("settings.system").to_string(),
                    system_active,
                    palette,
                    cx.listener(
                        |this: &mut SettingsView,
                         _: &gpui::ClickEvent,
                         _: &mut Window,
                         cx: &mut Context<SettingsView>| {
                            this.section = ConfigSection::System;
                            cx.notify();
                        },
                    ),
                ),
                sidebar_button(
                    "section-debug",
                    t!("settings.debug").to_string(),
                    debug_active,
                    palette,
                    cx.listener(
                        |this: &mut SettingsView,
                         _: &gpui::ClickEvent,
                         _: &mut Window,
                         cx: &mut Context<SettingsView>| {
                            this.section = ConfigSection::Debug;
                            cx.notify();
                        },
                    ),
                ),
            ]))
            .into_any_element()
    }
}

impl Render for SettingsView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let palette = UiPalette::from_app(cx);
        let status = self.status.clone();
        div()
            .relative()
            .size_full()
            .min_w_0()
            .flex()
            .text_color(palette.foreground)
            .bg(palette.background)
            .child(self.render_sidebar(cx))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .child(match self.section {
                        ConfigSection::Model => self.render_model_page(cx),
                        ConfigSection::Conversation => self
                            .agent_settings_view
                            .clone()
                            .map(|view| {
                                AnyView::from(view)
                                    .cached(StyleRefinement::default().size_full())
                                    .into_any_element()
                            })
                            .unwrap_or_else(|| {
                                div()
                                    .size_full()
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .text_sm()
                                    .text_color(palette.muted_foreground)
                                    .child(t!("settings.not_initialized").to_string())
                                    .into_any_element()
                            }),
                        ConfigSection::Tool => self.render_tool_page(cx),
                        ConfigSection::System => self.render_system_page(cx),
                        ConfigSection::Debug => self.render_debug_page(cx),
                    }),
            )
            .when_some(status, |this, status| {
                this.child(
                    div()
                        .absolute()
                        .top_3()
                        .left_0()
                        .right_0()
                        .flex()
                        .justify_center()
                        .child(
                            div()
                                .max_w(px(460.0))
                                .rounded_lg()
                                .border_1()
                                .border_color(palette.border)
                                .bg(palette.popover)
                                .px_4()
                                .py_2()
                                .text_sm()
                                .text_color(palette.foreground)
                                .shadow_lg()
                                .child(status),
                        ),
                )
            })
    }
}
