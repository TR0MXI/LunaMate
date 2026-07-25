//! 渲染 Agent 工具授权，并将隐私敏感能力保持为显式选择。

use gpui::{AnyElement, Context, IntoElement, div, prelude::*, px};
use gpui_component::StyledExt as _;
use rust_i18n::t;

use crate::ui::UiPalette;

use super::{
    SettingsView,
    components::{page_header, setting_row, system_section_label, toggle_switch},
};

impl SettingsView {
    pub(super) fn render_tool_page(&self, cx: &mut Context<Self>) -> AnyElement {
        let palette = UiPalette::from_app(cx);
        let retry_required = self.screenshot_permission_retry_required;
        div()
            .size_full()
            .min_w_0()
            .flex()
            .flex_col()
            .child(page_header(t!("settings.tool_title").to_string(), palette))
            .child(
                div()
                    .id("tool-settings-scroll")
                    .flex_1()
                    .overflow_y_scroll()
                    .px_8()
                    .child(
                        div()
                            .max_w(px(720.0))
                            .child(system_section_label(
                                t!("tools.permissions").to_string(),
                                palette,
                            ))
                            .child(
                                setting_row(
                                    t!("tools.allow_agent_screenshot").to_string(),
                                    palette,
                                )
                                .child(
                                    toggle_switch(
                                        "allow-agent-screenshot",
                                        self.allow_agent_screenshot,
                                        palette,
                                    )
                                    .on_click(cx.listener(
                                        |this, _, _, cx| {
                                            let allowed =
                                                if this.screenshot_permission_retry_required {
                                                    false
                                                } else {
                                                    !this.allow_agent_screenshot
                                                };
                                            this.set_allow_agent_screenshot(allowed, cx);
                                        },
                                    )),
                                ),
                            )
                            .child(
                                div()
                                    .pt_3()
                                    .text_xs()
                                    .text_color(palette.muted_foreground)
                                    .child(t!("tools.screenshot_notice").to_string()),
                            )
                            .when(retry_required, |this| {
                                this.child(
                                    div()
                                        .mt_3()
                                        .flex()
                                        .items_center()
                                        .justify_between()
                                        .gap_3()
                                        .rounded_md()
                                        .border_1()
                                        .border_color(palette.warning)
                                        .bg(palette.warning.opacity(0.08))
                                        .p_3()
                                        .text_xs()
                                        .text_color(palette.foreground)
                                        .child(div().min_w_0().flex_1().child(
                                            t!("tools.screenshot_disable_retry_notice").to_string(),
                                        ))
                                        .child(
                                            div()
                                                .id("retry-disable-agent-screenshot")
                                                .flex_shrink_0()
                                                .rounded_md()
                                                .bg(palette.warning)
                                                .px_3()
                                                .py_1()
                                                .font_medium()
                                                .text_color(palette.warning_foreground)
                                                .cursor_pointer()
                                                .hover(move |style| {
                                                    style.bg(palette.warning.opacity(0.86))
                                                })
                                                .on_click(cx.listener(|this, _, _, cx| {
                                                    this.set_allow_agent_screenshot(false, cx);
                                                }))
                                                .child(
                                                    t!("tools.retry_screenshot_disable")
                                                        .to_string(),
                                                ),
                                        ),
                                )
                            }),
                    ),
            )
            .into_any_element()
    }
}
