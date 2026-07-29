//! 提供供应商与人格设置页共享的展示组件，不持有任何配置状态。

use gpui::{AnyElement, ClickEvent, Div, IntoElement, Stateful, Window, div, prelude::*, px, svg};
use gpui_component::StyledExt as _;

use crate::agent::palette::AgentPalette;

pub(super) fn section_label(title: String, palette: AgentPalette) -> Div {
    div()
        .pt_6()
        .pb_2()
        .text_xs()
        .font_semibold()
        .text_color(palette.primary)
        .child(title)
}

pub(super) fn form_field(label: String, control: impl IntoElement, palette: AgentPalette) -> Div {
    div()
        .flex_1()
        .min_w_0()
        .min_h(px(78.0))
        .flex()
        .flex_col()
        .gap_2()
        .pt_3()
        .child(
            div()
                .text_xs()
                .font_medium()
                .text_color(palette.muted_foreground)
                .child(label),
        )
        .child(div().w_full().min_w_0().child(control))
}

/// 带启用开关的高级参数行：关闭时该参数不会随请求发送，输入框只保留建议值。
pub(super) fn optional_field(
    id: &'static str,
    label: String,
    hint: String,
    enabled: bool,
    control: impl IntoElement,
    palette: AgentPalette,
    on_toggle: impl Fn(&ClickEvent, &mut Window, &mut gpui::App) + 'static,
) -> Div {
    div()
        .flex_1()
        .min_w_0()
        .min_h(px(78.0))
        .flex()
        .flex_col()
        .gap_2()
        .pt_3()
        .child(
            div()
                .flex()
                .items_center()
                .justify_between()
                .gap_2()
                .child(
                    div()
                        .min_w_0()
                        .overflow_hidden()
                        .text_ellipsis()
                        .text_xs()
                        .font_medium()
                        .text_color(if enabled {
                            palette.foreground
                        } else {
                            palette.muted_foreground
                        })
                        .child(label),
                )
                .child(
                    toggle_switch(id, enabled, palette)
                        .flex_shrink_0()
                        .on_click(on_toggle),
                ),
        )
        .child(div().w_full().min_w_0().child(control))
        .child(
            div()
                .text_xs()
                .text_color(palette.muted_foreground)
                .child(if enabled { String::new() } else { hint }),
        )
}

pub(super) fn toggle_switch(
    id: &'static str,
    checked: bool,
    palette: AgentPalette,
) -> Stateful<Div> {
    div()
        .id(id)
        .w(px(34.0))
        .h(px(18.0))
        .flex()
        .items_center()
        .rounded_full()
        .p(px(2.0))
        .cursor_pointer()
        .bg(if checked {
            palette.primary
        } else {
            palette.muted
        })
        .child(
            div()
                .size(px(14.0))
                .rounded_full()
                .bg(palette.background)
                .when(checked, |this| this.ml(px(16.0))),
        )
}

/// 可折叠区块的标题行；折叠状态由调用方持有。
pub(super) fn collapsible_header(
    id: &'static str,
    title: String,
    summary: String,
    expanded: bool,
    palette: AgentPalette,
    on_click: impl Fn(&ClickEvent, &mut Window, &mut gpui::App) + 'static,
) -> AnyElement {
    div()
        .id(id)
        .mt_6()
        .min_h(px(44.0))
        .flex()
        .items_center()
        .justify_between()
        .gap_3()
        .rounded_md()
        .border_1()
        .border_color(palette.border)
        .bg(palette.sidebar)
        .px_3()
        .cursor_pointer()
        .hover(move |style| style.bg(palette.secondary))
        .on_click(on_click)
        .child(
            div()
                .min_w_0()
                .flex()
                .flex_col()
                .child(
                    div()
                        .text_xs()
                        .font_semibold()
                        .text_color(palette.primary)
                        .child(title),
                )
                .child(
                    div()
                        .mt(px(2.0))
                        .overflow_hidden()
                        .text_ellipsis()
                        .text_xs()
                        .text_color(palette.muted_foreground)
                        .child(summary),
                ),
        )
        .child(
            svg()
                .path(if expanded {
                    "icons/chevron-down.svg"
                } else {
                    "icons/chevron-right.svg"
                })
                .size_4()
                .flex_shrink_0()
                .text_color(palette.muted_foreground),
        )
        .into_any_element()
}

/// 危险操作的二次确认层；覆盖整个页面并吃掉底层交互。
pub(super) fn confirm_overlay(
    title: String,
    message: String,
    confirm_label: String,
    cancel_label: String,
    palette: AgentPalette,
    on_confirm: impl Fn(&ClickEvent, &mut Window, &mut gpui::App) + 'static,
    on_cancel: impl Fn(&ClickEvent, &mut Window, &mut gpui::App) + 'static,
) -> AnyElement {
    div()
        .id("confirm-overlay")
        .absolute()
        .inset_0()
        .flex()
        .items_center()
        .justify_center()
        .bg(gpui::black().opacity(0.45))
        .occlude()
        .child(
            div()
                .w(px(420.0))
                .max_w_full()
                .flex()
                .flex_col()
                .gap_3()
                .rounded_lg()
                .border_1()
                .border_color(palette.border)
                .bg(palette.popover)
                .p_5()
                .shadow_lg()
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap_2()
                        .child(
                            svg()
                                .path("icons/triangle-alert.svg")
                                .size_4()
                                .flex_shrink_0()
                                .text_color(palette.danger),
                        )
                        .child(div().text_sm().font_semibold().child(title)),
                )
                .child(
                    div()
                        .text_xs()
                        .text_color(palette.muted_foreground)
                        .child(message),
                )
                .child(
                    div()
                        .pt_2()
                        .flex()
                        .items_center()
                        .justify_end()
                        .gap_2()
                        .child(
                            div()
                                .id("confirm-cancel")
                                .h(px(30.0))
                                .flex()
                                .items_center()
                                .justify_center()
                                .rounded_md()
                                .border_1()
                                .border_color(palette.border)
                                .px_4()
                                .text_xs()
                                .font_medium()
                                .cursor_pointer()
                                .hover(move |style| style.bg(palette.secondary))
                                .on_click(on_cancel)
                                .child(cancel_label),
                        )
                        .child(
                            div()
                                .id("confirm-accept")
                                .h(px(30.0))
                                .flex()
                                .items_center()
                                .justify_center()
                                .rounded_md()
                                .bg(palette.danger)
                                .px_4()
                                .text_xs()
                                .font_medium()
                                .text_color(palette.danger_foreground)
                                .cursor_pointer()
                                .hover(move |style| style.bg(palette.danger.opacity(0.86)))
                                .on_click(on_confirm)
                                .child(confirm_label),
                        ),
                ),
        )
        .into_any_element()
}

/// 页面顶部的标题栏与保存按钮。
pub(super) fn page_header(
    save_id: &'static str,
    title: String,
    save_label: String,
    saving: bool,
    palette: AgentPalette,
    on_save: impl Fn(&ClickEvent, &mut Window, &mut gpui::App) + 'static,
) -> Div {
    div()
        .h(px(54.0))
        .flex_shrink_0()
        .flex()
        .items_center()
        .justify_between()
        .border_b_1()
        .border_color(palette.border)
        .px_5()
        .child(div().text_base().font_semibold().child(title))
        .child(
            div()
                .id(save_id)
                .h(px(34.0))
                .flex()
                .items_center()
                .justify_center()
                .gap_2()
                .rounded_md()
                .px_4()
                .text_sm()
                .font_medium()
                .bg(if saving {
                    palette.muted
                } else {
                    palette.primary
                })
                .text_color(if saving {
                    palette.muted_foreground
                } else {
                    palette.primary_foreground
                })
                .cursor_pointer()
                .hover(move |style| style.bg(palette.accent))
                .on_click(on_save)
                .child(
                    svg()
                        .path("icons/check.svg")
                        .size_4()
                        .text_color(if saving {
                            palette.muted_foreground
                        } else {
                            palette.primary_foreground
                        }),
                )
                .child(save_label),
        )
}

/// 页面底部的短时状态提示。
pub(super) fn status_toast(status: String, palette: AgentPalette) -> AnyElement {
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
        )
        .into_any_element()
}
