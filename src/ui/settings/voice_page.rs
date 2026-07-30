//! 渲染语音输入模式；Transcription 模型及推理选项在模型设置中维护。

use gpui::{AnyElement, Context, IntoElement, div, prelude::*, px};
use gpui_component::StyledExt as _;
use rust_i18n::t;

use crate::{config::VoiceMode, ui::UiPalette};

use super::{
    SettingsView,
    components::{frame_rate_button, page_header, setting_row, system_section_label},
};

impl SettingsView {
    pub(super) fn render_voice_page(&self, cx: &mut Context<Self>) -> AnyElement {
        let palette = UiPalette::from_app(cx);
        let modes = [
            ("voice-mode-off", VoiceMode::Off, t!("voice.mode_off")),
            ("voice-mode-auto", VoiceMode::Auto, t!("voice.mode_auto")),
            (
                "voice-mode-push-to-talk",
                VoiceMode::PushToTalk,
                t!("voice.mode_push_to_talk"),
            ),
        ]
        .into_iter()
        .map(|(id, mode, label)| {
            frame_rate_button(
                id,
                label.to_string(),
                self.voice.mode == mode,
                palette,
                cx.listener(move |this, _, _, cx| this.set_voice_mode_draft(mode, cx)),
            )
        })
        .collect::<Vec<_>>();

        div()
            .size_full()
            .min_w_0()
            .flex()
            .flex_col()
            .child(page_header(t!("voice.title").to_string(), palette))
            .child(
                div()
                    .id("voice-settings-scroll")
                    .flex_1()
                    .overflow_y_scroll()
                    .px_8()
                    .child(
                        div()
                            .max_w(px(720.0))
                            .child(system_section_label(t!("voice.input").to_string(), palette))
                            .child(
                                setting_row(t!("voice.mode").to_string(), palette).child(
                                    div()
                                        .flex()
                                        .flex_wrap()
                                        .justify_end()
                                        .gap_1()
                                        .rounded_md()
                                        .bg(palette.muted)
                                        .children(modes),
                                ),
                            )
                            .child(
                                div().w_full().flex().justify_end().py_4().child(
                                    div()
                                        .id("save-voice-settings")
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
                                        .hover(move |style| style.bg(palette.primary.opacity(0.86)))
                                        .on_click(cx.listener(|this, _, _, cx| {
                                            this.save_voice_settings(cx);
                                        }))
                                        .child(t!("common.save").to_string()),
                                ),
                            ),
                    ),
            )
            .into_any_element()
    }
}
