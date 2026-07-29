//! 组装设置主体布局，并根据当前侧栏分区调度页面渲染。

use gpui::{
    AnyElement, AnyView, Context, IntoElement, Render, StyleRefinement, Window, div, prelude::*, px,
};
use rust_i18n::t;

use crate::ui::UiPalette;

use super::{ConfigSection, SettingsView, components::sidebar_button};

impl SettingsView {
    fn render_sidebar(&self, cx: &mut Context<Self>) -> AnyElement {
        // `t!` 的 minify_key 要求字面量键，因此在此展开为具体标签。
        let sections = [
            (
                "section-model",
                t!("settings.model").to_string(),
                ConfigSection::Model,
            ),
            (
                "section-provider",
                t!("settings.provider").to_string(),
                ConfigSection::Provider,
            ),
            (
                "section-persona",
                t!("settings.persona").to_string(),
                ConfigSection::Persona,
            ),
            (
                "section-voice",
                t!("settings.voice").to_string(),
                ConfigSection::Voice,
            ),
            (
                "section-shortcut",
                t!("settings.shortcut").to_string(),
                ConfigSection::Shortcut,
            ),
            (
                "section-tool",
                t!("settings.tool").to_string(),
                ConfigSection::Tool,
            ),
            (
                "section-system",
                t!("settings.system").to_string(),
                ConfigSection::System,
            ),
            (
                "section-debug",
                t!("settings.debug").to_string(),
                ConfigSection::Debug,
            ),
        ];

        let palette = UiPalette::from_app(cx);
        let active_section = self.section;
        div()
            .w(px(160.0))
            .h_full()
            .flex_shrink_0()
            .flex()
            .flex_col()
            .border_r_1()
            .border_color(palette.border)
            .bg(palette.sidebar)
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .px_2()
                    .pt_3()
                    .children(sections.map(|(id, label, section)| {
                        sidebar_button(
                            id,
                            label,
                            active_section == section,
                            palette,
                            cx.listener(move |this: &mut SettingsView, _, window, cx| {
                                this.set_section(section, cx);
                                if section == ConfigSection::Persona {
                                    this.refresh_persona_live2d_models(window, cx);
                                }
                            }),
                        )
                    })),
            )
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
                        ConfigSection::Provider => self
                            .agent_settings_view
                            .clone()
                            .map(embedded_page)
                            .unwrap_or_else(|| uninitialized_page(palette)),
                        ConfigSection::Persona => self
                            .persona_settings_view
                            .clone()
                            .map(embedded_page)
                            .unwrap_or_else(|| uninitialized_page(palette)),
                        ConfigSection::Voice => self.render_voice_page(cx),
                        ConfigSection::Shortcut => self.render_shortcut_page(cx),
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

/// 把一个独立设置编辑器实体嵌入主体区域，并缓存其渲染结果。
fn embedded_page<V: Render>(view: gpui::Entity<V>) -> AnyElement {
    AnyView::from(view)
        .cached(StyleRefinement::default().size_full())
        .into_any_element()
}

/// 设置窗口尚未激活时不持有输入组件，此时给出可诊断的占位提示。
fn uninitialized_page(palette: UiPalette) -> AnyElement {
    div()
        .size_full()
        .flex()
        .items_center()
        .justify_center()
        .text_sm()
        .text_color(palette.muted_foreground)
        .child(t!("settings.not_initialized").to_string())
        .into_any_element()
}
