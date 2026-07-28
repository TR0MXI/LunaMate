//! 提供设置页面共享的轻量展示组件，不持有配置状态。

use gpui::{AnyElement, Entity, IntoElement, Window, div, prelude::*, px};
use gpui_component::{
    StyledExt,
    input::{Input, InputState},
    try_parse_color,
};

use crate::ui::UiPalette;

pub(super) fn sidebar_button(
    id: &'static str,
    label: String,
    active: bool,
    palette: UiPalette,
    on_click: impl Fn(&gpui::ClickEvent, &mut Window, &mut gpui::App) + 'static,
) -> AnyElement {
    div()
        .id(id)
        .h(px(36.0))
        .flex()
        .items_center()
        .rounded_md()
        .px_3()
        .text_sm()
        .font_medium()
        .cursor_pointer()
        .bg(if active {
            palette.accent
        } else {
            palette.sidebar
        })
        .text_color(if active {
            palette.accent_foreground
        } else {
            palette.muted_foreground
        })
        .hover(move |style| style.bg(palette.secondary))
        .on_click(on_click)
        .child(label)
        .into_any_element()
}

pub(super) fn page_header(title: String, palette: UiPalette) -> gpui::Div {
    div()
        .h(px(54.0))
        .flex_shrink_0()
        .flex()
        .items_center()
        .justify_between()
        .border_b_1()
        .border_color(palette.border)
        .px_5()
        .text_base()
        .font_semibold()
        .child(title)
}

pub(super) fn control_section(title: String, palette: UiPalette) -> gpui::Div {
    div().border_b_1().border_color(palette.border).p_4().child(
        div()
            .mb_3()
            .text_xs()
            .font_semibold()
            .text_color(palette.muted_foreground)
            .child(title),
    )
}

pub(super) fn empty_control_text(text: String, palette: UiPalette) -> gpui::Div {
    div()
        .py_1()
        .text_xs()
        .text_color(palette.muted_foreground)
        .child(text)
}

pub(super) fn system_section_label(title: String, palette: UiPalette) -> gpui::Div {
    div()
        .pt_7()
        .pb_2()
        .text_xs()
        .font_semibold()
        .text_color(palette.primary)
        .child(title)
}

pub(super) fn setting_row(title: String, palette: UiPalette) -> gpui::Div {
    div()
        .min_h(px(58.0))
        .flex()
        .items_center()
        .justify_between()
        .gap_5()
        .border_b_1()
        .border_color(palette.border)
        .text_sm()
        .child(title)
}

pub(super) fn color_input(input: Entity<InputState>, color: &str, palette: UiPalette) -> gpui::Div {
    let color = try_parse_color(color).unwrap_or(palette.muted);
    div()
        .flex()
        .items_center()
        .gap_2()
        .child(
            div()
                .size(px(24.0))
                .flex_shrink_0()
                .rounded_md()
                .border_1()
                .border_color(palette.border)
                .bg(color),
        )
        .child(div().w(px(180.0)).child(Input::new(&input)))
}

pub(super) fn frame_rate_button(
    id: &'static str,
    label: impl Into<String>,
    active: bool,
    palette: UiPalette,
    on_click: impl Fn(&gpui::ClickEvent, &mut Window, &mut gpui::App) + 'static,
) -> AnyElement {
    div()
        .id(id)
        .min_w(px(64.0))
        .h(px(32.0))
        .flex()
        .items_center()
        .justify_center()
        .rounded_md()
        .px_3()
        .text_xs()
        .cursor_pointer()
        .bg(if active {
            palette.primary
        } else {
            palette.muted
        })
        .text_color(if active {
            palette.primary_foreground
        } else {
            palette.foreground
        })
        .hover(move |style| {
            style.bg(if active {
                palette.primary.opacity(0.86)
            } else {
                palette.accent
            })
        })
        .on_click(on_click)
        .child(label.into())
        .into_any_element()
}

pub(super) fn toggle_switch(
    id: &'static str,
    checked: bool,
    palette: UiPalette,
) -> gpui::Stateful<gpui::Div> {
    div()
        .id(id)
        .w(px(36.0))
        .h(px(20.0))
        .flex()
        .items_center()
        .rounded_full()
        .p(px(2.0))
        .cursor_pointer()
        .bg(if checked {
            palette.primary
        } else {
            palette.input
        })
        .child(
            div()
                .size(px(16.0))
                .rounded_full()
                .bg(palette.background)
                .when(checked, |this| this.ml(px(16.0))),
        )
}
