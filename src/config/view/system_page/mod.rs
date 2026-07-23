//! 渲染运行时、窗口、外观、语言与调试设置页面。

use gpui::{AnyElement, Context, IntoElement, div, prelude::*, px};
use gpui_component::StyledExt;
use rust_i18n::t;

use crate::theme::{AppLanguage, ThemePreset, UiPalette};

use super::{
    ConfigView,
    components::{
        color_input, frame_rate_button, page_header, setting_row, system_section_label,
        toggle_switch,
    },
};
use crate::config::{FrameRate, ModelWindowSize};

impl ConfigView {
    pub(super) fn render_system_page(&self, cx: &mut Context<Self>) -> AnyElement {
        let palette = UiPalette::from_app(cx);
        let frame_rate_buttons = vec![
            frame_rate_button(
                "frame-rate-30",
                "30",
                self.frame_rate == FrameRate::Fps30,
                palette,
                cx.listener(|this, _, _, cx| this.set_frame_rate(FrameRate::Fps30, cx)),
            ),
            frame_rate_button(
                "frame-rate-60",
                "60",
                self.frame_rate == FrameRate::Fps60,
                palette,
                cx.listener(|this, _, _, cx| this.set_frame_rate(FrameRate::Fps60, cx)),
            ),
            frame_rate_button(
                "frame-rate-120",
                "120",
                self.frame_rate == FrameRate::Fps120,
                palette,
                cx.listener(|this, _, _, cx| this.set_frame_rate(FrameRate::Fps120, cx)),
            ),
            frame_rate_button(
                "frame-rate-unlimited",
                t!("system.unlimited").to_string(),
                self.frame_rate == FrameRate::Unlimited,
                palette,
                cx.listener(|this, _, _, cx| this.set_frame_rate(FrameRate::Unlimited, cx)),
            ),
        ];
        let window_size_buttons = [
            (
                "model-window-auto",
                ModelWindowSize::Auto,
                t!("system.size_auto"),
            ),
            (
                "model-window-compact",
                ModelWindowSize::Compact,
                t!("system.size_compact"),
            ),
            (
                "model-window-standard",
                ModelWindowSize::Standard,
                t!("system.size_standard"),
            ),
            (
                "model-window-large",
                ModelWindowSize::Large,
                t!("system.size_large"),
            ),
            (
                "model-window-extra-large",
                ModelWindowSize::ExtraLarge,
                t!("system.size_extra_large"),
            ),
        ]
        .into_iter()
        .map(|(id, size, label)| {
            frame_rate_button(
                id,
                label.to_string(),
                self.model_window_size == size,
                palette,
                cx.listener(move |this, _, _, cx| this.set_model_window_size(size, cx)),
            )
        })
        .collect::<Vec<_>>();

        let theme_buttons = [
            (
                "theme-system",
                ThemePreset::System,
                t!("system.theme_system"),
            ),
            ("theme-light", ThemePreset::Light, t!("system.theme_light")),
            ("theme-dark", ThemePreset::Dark, t!("system.theme_dark")),
            (
                "theme-graphite",
                ThemePreset::Graphite,
                t!("system.theme_graphite"),
            ),
            (
                "theme-sakura",
                ThemePreset::Sakura,
                t!("system.theme_sakura"),
            ),
            ("theme-ocean", ThemePreset::Ocean, t!("system.theme_ocean")),
            (
                "theme-high-contrast",
                ThemePreset::HighContrast,
                t!("system.theme_high_contrast"),
            ),
            (
                "theme-custom",
                ThemePreset::Custom,
                t!("system.theme_custom"),
            ),
        ]
        .into_iter()
        .map(|(id, theme, label)| {
            frame_rate_button(
                id,
                label.to_string(),
                self.appearance.theme == theme,
                palette,
                cx.listener(move |this, _, window, cx| this.set_theme(theme, window, cx)),
            )
        })
        .collect::<Vec<_>>();

        let language_buttons = [
            (
                "language-zh-cn",
                AppLanguage::SimplifiedChinese,
                t!("language.zh_cn"),
            ),
            (
                "language-zh-tw",
                AppLanguage::TraditionalChinese,
                t!("language.zh_tw"),
            ),
            ("language-en", AppLanguage::English, t!("language.en")),
            ("language-ja", AppLanguage::Japanese, t!("language.ja")),
        ]
        .into_iter()
        .map(|(id, language, label)| {
            frame_rate_button(
                id,
                label.to_string(),
                self.appearance.language == language,
                palette,
                cx.listener(move |this, _, window, cx| this.set_language(language, window, cx)),
            )
        })
        .collect::<Vec<_>>();
        let accent_input = self.custom_accent_input.clone();
        let background_input = self.custom_background_input.clone();

        div()
            .size_full()
            .min_w_0()
            .flex()
            .flex_col()
            .child(page_header(
                t!("settings.system_title").to_string(),
                palette,
            ))
            .child(
                div()
                    .id("system-settings-scroll")
                    .flex_1()
                    .overflow_y_scroll()
                    .px_8()
                    .child(
                        div()
                            .max_w(px(720.0))
                            .child(system_section_label(
                                t!("system.render").to_string(),
                                palette,
                            ))
                            .child(
                                setting_row(t!("system.frame_rate").to_string(), palette).child(
                                    div()
                                        .flex()
                                        .flex_wrap()
                                        .justify_end()
                                        .gap_1()
                                        .rounded_md()
                                        .bg(palette.muted)
                                        .children(frame_rate_buttons),
                                ),
                            )
                            .child(
                                setting_row(t!("system.model_window_size").to_string(), palette)
                                    .child(
                                        div()
                                            .flex()
                                            .flex_wrap()
                                            .justify_end()
                                            .gap_1()
                                            .rounded_md()
                                            .bg(palette.muted)
                                            .children(window_size_buttons),
                                    ),
                            )
                            .child(system_section_label(
                                t!("system.interaction").to_string(),
                                palette,
                            ))
                            .child(
                                setting_row(t!("system.eye_tracking").to_string(), palette).child(
                                    toggle_switch("eye-tracking", self.eye_tracking, palette)
                                        .on_click(cx.listener(|this, _, _, cx| {
                                            this.set_eye_tracking(!this.eye_tracking, cx);
                                        })),
                                ),
                            )
                            .child(system_section_label(
                                t!("system.window").to_string(),
                                palette,
                            ))
                            .child(
                                setting_row(t!("system.remember_positions").to_string(), palette)
                                    .child(
                                        toggle_switch(
                                            "remember-window-position",
                                            self.remember_window_positions,
                                            palette,
                                        )
                                        .on_click(
                                            cx.listener(|this, _, _, cx| {
                                                this.set_remember_window_positions(
                                                    !this.remember_window_positions,
                                                    cx,
                                                );
                                            }),
                                        ),
                                    ),
                            )
                            .child(
                                setting_row(t!("system.saved_positions").to_string(), palette)
                                    .child(
                                        div()
                                            .id("reset-window-positions")
                                            .rounded_md()
                                            .border_1()
                                            .border_color(palette.danger)
                                            .px_3()
                                            .py_1()
                                            .text_xs()
                                            .text_color(palette.danger)
                                            .cursor_pointer()
                                            .hover(move |style| {
                                                style.bg(palette.danger.opacity(0.16))
                                            })
                                            .on_click(cx.listener(|this, _, _, cx| {
                                                this.reset_window_positions(cx);
                                            }))
                                            .child(t!("system.reset_positions").to_string()),
                                    ),
                            )
                            .child(system_section_label(
                                t!("system.appearance").to_string(),
                                palette,
                            ))
                            .child(
                                setting_row(t!("system.theme").to_string(), palette).child(
                                    div()
                                        .flex()
                                        .flex_wrap()
                                        .justify_end()
                                        .gap_1()
                                        .rounded_md()
                                        .bg(palette.muted)
                                        .children(theme_buttons),
                                ),
                            )
                            .when(self.appearance.theme == ThemePreset::Custom, |this| {
                                this.child(
                                    setting_row(t!("system.custom_mode").to_string(), palette)
                                        .child(div().flex().gap_1().children([
                                            frame_rate_button(
                                                "custom-mode-light",
                                                t!("system.theme_light").to_string(),
                                                !self.appearance.custom.mode.is_dark(),
                                                palette,
                                                cx.listener(|this, _, window, cx| {
                                                    let mut appearance = this.appearance.clone();
                                                    appearance.custom.mode =
                                                        gpui_component::ThemeMode::Light;
                                                    this.set_appearance(
                                                        appearance, false, window, cx,
                                                    );
                                                }),
                                            ),
                                            frame_rate_button(
                                                "custom-mode-dark",
                                                t!("system.theme_dark").to_string(),
                                                self.appearance.custom.mode.is_dark(),
                                                palette,
                                                cx.listener(|this, _, window, cx| {
                                                    let mut appearance = this.appearance.clone();
                                                    appearance.custom.mode =
                                                        gpui_component::ThemeMode::Dark;
                                                    this.set_appearance(
                                                        appearance, false, window, cx,
                                                    );
                                                }),
                                            ),
                                        ])),
                                )
                                .when_some(accent_input.clone(), |this, input| {
                                    this.child(
                                        setting_row(
                                            t!("system.custom_accent").to_string(),
                                            palette,
                                        )
                                        .child(
                                            color_input(
                                                input,
                                                &self.appearance.custom.accent,
                                                palette,
                                            ),
                                        ),
                                    )
                                })
                                .when_some(background_input.clone(), |this, input| {
                                    this.child(
                                        setting_row(
                                            t!("system.custom_background").to_string(),
                                            palette,
                                        )
                                        .child(
                                            color_input(
                                                input,
                                                &self.appearance.custom.background,
                                                palette,
                                            ),
                                        ),
                                    )
                                })
                                .child(
                                    div().w_full().flex().justify_end().py_3().child(
                                        div()
                                            .id("apply-custom-theme")
                                            .h(px(32.0))
                                            .flex()
                                            .items_center()
                                            .justify_center()
                                            .rounded_md()
                                            .bg(palette.primary)
                                            .px_4()
                                            .text_xs()
                                            .font_medium()
                                            .text_color(palette.primary_foreground)
                                            .cursor_pointer()
                                            .hover(move |style| {
                                                style.bg(palette.primary.opacity(0.86))
                                            })
                                            .on_click(cx.listener(|this, _, window, cx| {
                                                this.apply_custom_theme(window, cx);
                                            }))
                                            .child(t!("common.apply").to_string()),
                                    ),
                                )
                            })
                            .child(system_section_label(
                                t!("system.language").to_string(),
                                palette,
                            ))
                            .child(
                                setting_row(t!("system.language").to_string(), palette).child(
                                    div()
                                        .flex()
                                        .flex_wrap()
                                        .justify_end()
                                        .gap_1()
                                        .rounded_md()
                                        .bg(palette.muted)
                                        .children(language_buttons),
                                ),
                            ),
                    ),
            )
            .into_any_element()
    }

    pub(super) fn render_debug_page(&self, cx: &mut Context<Self>) -> AnyElement {
        let palette = UiPalette::from_app(cx);
        div()
            .size_full()
            .min_w_0()
            .flex()
            .flex_col()
            .child(page_header(t!("settings.debug_title").to_string(), palette))
            .child(
                div()
                    .id("debug-settings-scroll")
                    .flex_1()
                    .overflow_y_scroll()
                    .px_8()
                    .child(
                        div()
                            .max_w(px(720.0))
                            .child(system_section_label(
                                t!("debug.runtime").to_string(),
                                palette,
                            ))
                            .child(
                                setting_row(t!("debug.show_fps").to_string(), palette).child(
                                    toggle_switch("show-fps", self.show_fps, palette).on_click(
                                        cx.listener(|this, _, _, cx| {
                                            this.set_show_fps(!this.show_fps, cx);
                                        }),
                                    ),
                                ),
                            ),
                    ),
            )
            .into_any_element()
    }
}
