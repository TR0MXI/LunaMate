//! 渲染四个全局快捷键动作及其键盘录入控件。

use gpui::{AnyElement, Context, IntoElement, KeyDownEvent, MouseButton, div, prelude::*, px, svg};
use gpui_component::{StyledExt as _, tooltip::Tooltip};
use rust_i18n::t;

use crate::{
    config::{KeyboardShortcut, ShortcutAction},
    shortcut::shortcut_keycaps,
    ui::UiPalette,
};

use super::{
    SettingsView,
    components::{page_header, setting_row, system_section_label},
};

impl SettingsView {
    pub(super) fn render_shortcut_page(&self, cx: &mut Context<Self>) -> AnyElement {
        let palette = UiPalette::from_app(cx);
        let actions = [
            (ShortcutAction::VoiceInput, t!("shortcut.voice_input")),
            (
                ShortcutAction::ToggleDesktopPet,
                t!("shortcut.toggle_desktop_pet"),
            ),
            (
                ShortcutAction::ToggleSettings,
                t!("shortcut.toggle_settings"),
            ),
            (
                ShortcutAction::ToggleChatInput,
                t!("shortcut.toggle_chat_input"),
            ),
        ];
        let focus = self.shortcut_focus.clone();
        let runtime_error = (!self.shortcut_runtime_errors.is_empty()).then(|| {
            t!(
                "shortcut.registration_failed",
                error = self
                    .shortcut_runtime_errors
                    .join(t!("common.status_separator").as_ref())
            )
            .to_string()
        });

        div()
            .size_full()
            .min_w_0()
            .flex()
            .flex_col()
            .when_some(focus, |this, focus| {
                this.track_focus(&focus).on_key_down(cx.listener(
                    |this, event: &KeyDownEvent, _, cx| {
                        if this.shortcut_recording.is_some() {
                            cx.stop_propagation();
                            this.handle_shortcut_key_down(event, cx);
                        }
                    },
                ))
            })
            .child(page_header(t!("shortcut.title").to_string(), palette))
            .child(
                div()
                    .id("shortcut-settings-scroll")
                    .flex_1()
                    .overflow_y_scroll()
                    .px_8()
                    .child(
                        div()
                            .max_w(px(720.0))
                            .child(system_section_label(
                                t!("shortcut.actions").to_string(),
                                palette,
                            ))
                            .when_some(runtime_error, |this, error| {
                                this.child(
                                    div()
                                        .mb_2()
                                        .w_full()
                                        .flex()
                                        .items_start()
                                        .gap_2()
                                        .rounded_md()
                                        .border_1()
                                        .border_color(palette.warning)
                                        .bg(palette.muted)
                                        .p_3()
                                        .text_xs()
                                        .text_color(palette.warning_foreground)
                                        .child(
                                            svg()
                                                .path("icons/triangle-alert.svg")
                                                .size_4()
                                                .flex_shrink_0()
                                                .text_color(palette.warning),
                                        )
                                        .child(div().min_w_0().flex_1().child(error)),
                                )
                            })
                            .children(actions.map(|(action, label)| {
                                setting_row(label.to_string(), palette)
                                    .flex_wrap()
                                    .py_2()
                                    .child(shortcut_recorder(
                                        action,
                                        self.shortcuts.shortcut(action),
                                        self.shortcut_runtime_bindings
                                            .get(&action)
                                            .map(String::as_str),
                                        self.shortcut_recording == Some(action),
                                        palette,
                                        cx,
                                    ))
                            })),
                    ),
            )
            .into_any_element()
    }
}

fn shortcut_recorder(
    action: ShortcutAction,
    shortcut: Option<KeyboardShortcut>,
    runtime_trigger: Option<&str>,
    recording: bool,
    palette: UiPalette,
    cx: &mut Context<SettingsView>,
) -> AnyElement {
    let shows_runtime_trigger = runtime_trigger.is_some();
    let keycaps = runtime_trigger
        .map(|trigger| vec![trigger.to_owned()])
        .or_else(|| shortcut.map(shortcut_keycaps))
        .unwrap_or_default();
    let tooltip = t!("shortcut.record_tooltip").to_string();
    let content = div()
        .min_w_0()
        .max_w_full()
        .flex_1()
        .flex()
        .flex_wrap()
        .items_center()
        .justify_center()
        .gap_1()
        .child(
            svg()
                .path("icons/keyboard.svg")
                .size_4()
                .flex_shrink_0()
                .text_color(if recording {
                    palette.primary
                } else {
                    palette.muted_foreground
                }),
        )
        .when(recording, |this| {
            this.child(
                div()
                    .text_xs()
                    .font_medium()
                    .text_color(palette.primary)
                    .child(t!("shortcut.recording").to_string()),
            )
        })
        .when(!recording && keycaps.is_empty(), |this| {
            this.child(
                div()
                    .text_xs()
                    .text_color(palette.muted_foreground)
                    .child(t!("shortcut.unassigned").to_string()),
            )
        })
        .when(!recording && !keycaps.is_empty(), |this| {
            this.children(keycaps.into_iter().map(move |label| {
                div()
                    .min_w(px(24.0))
                    .max_w_full()
                    .h(px(24.0))
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded_sm()
                    .border_1()
                    .border_b_2()
                    .border_color(palette.border)
                    .bg(palette.background)
                    .px_2()
                    .text_xs()
                    .font_medium()
                    .text_color(palette.foreground)
                    .when(!shows_runtime_trigger, |this| this.flex_shrink_0())
                    .when(shows_runtime_trigger, |this| {
                        this.min_w_0().overflow_hidden().text_ellipsis()
                    })
                    .child(label)
            }))
        });
    div()
        .id(action.id())
        .w(px(360.0))
        .min_w(px(240.0))
        .max_w_full()
        .min_h(px(38.0))
        .flex()
        .items_center()
        .justify_center()
        .rounded_md()
        .border_1()
        .border_color(if recording {
            palette.primary
        } else {
            palette.border
        })
        .bg(if recording {
            palette.accent
        } else {
            palette.secondary
        })
        .px_2()
        .py_1()
        .cursor_pointer()
        .hover(move |style| style.bg(palette.accent))
        .tooltip(move |window, cx| Tooltip::new(tooltip.clone()).build(window, cx))
        .on_mouse_down(MouseButton::Left, |_, window, cx| {
            window.prevent_default();
            cx.stop_propagation();
        })
        .on_click(cx.listener(move |this, _, window, cx| {
            cx.stop_propagation();
            this.begin_shortcut_recording(action, window, cx);
        }))
        .child(content)
        .into_any_element()
}
