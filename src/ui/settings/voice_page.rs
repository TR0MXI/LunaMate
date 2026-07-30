//! 渲染语音模式、Whisper 模型路径、下载入口和推理设备草稿。

use gpui::{AnyElement, Context, Entity, IntoElement, div, prelude::*, px, svg};
use gpui_component::{
    Icon, IconName, Sizable as _, StyledExt as _,
    input::{Input, InputState},
    link::Link,
    tooltip::Tooltip,
};
use rust_i18n::t;

use crate::{config::VoiceMode, ui::UiPalette};

use super::{
    SettingsView,
    components::{
        frame_rate_button, page_header, setting_row, system_section_label, toggle_switch,
    },
};

const WHISPER_MODELS_URL: &str = "https://huggingface.co/ggerganov/whisper.cpp/tree/main";

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
                            .child(system_section_label(
                                t!("voice.models").to_string(),
                                palette,
                            ))
                            .child(model_download_info(palette))
                            .child(
                                setting_row(t!("voice.whisper_model").to_string(), palette)
                                    .when_some(
                                        self.voice_whisper_model_input.clone(),
                                        |row, input| {
                                            row.child(model_path_control(
                                                input,
                                                "choose-whisper-model",
                                                palette,
                                                cx,
                                            ))
                                        },
                                    ),
                            )
                            .child(system_section_label(
                                t!("voice.inference").to_string(),
                                palette,
                            ))
                            .child(
                                setting_row(t!("voice.gpu_acceleration").to_string(), palette)
                                    .child(
                                        toggle_switch("voice-gpu", self.voice.use_gpu, palette)
                                            .on_click(cx.listener(|this, _, _, cx| {
                                                this.toggle_voice_gpu_draft(cx);
                                            })),
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

fn model_download_info(palette: UiPalette) -> gpui::Div {
    div()
        .mt_2()
        .mb_1()
        .w_full()
        .flex()
        .items_start()
        .gap_2()
        .rounded_md()
        .border_1()
        .border_color(palette.border)
        .bg(palette.muted.opacity(0.55))
        .p_3()
        .text_xs()
        .child(
            div().flex_shrink_0().pt(px(1.0)).child(
                Icon::new(IconName::Info)
                    .small()
                    .text_color(palette.primary),
            ),
        )
        .child(
            div()
                .min_w_0()
                .flex_1()
                .flex()
                .flex_col()
                .gap_1()
                .child(
                    div()
                        .font_medium()
                        .text_color(palette.foreground)
                        .child(t!("voice.model_downloads").to_string()),
                )
                .child(
                    div()
                        .text_color(palette.muted_foreground)
                        .child(t!("voice.model_download_notice").to_string()),
                )
                .child(
                    div().flex().flex_wrap().items_center().gap_3().child(
                        Link::new("whisper-model-list")
                            .href(WHISPER_MODELS_URL)
                            .flex()
                            .items_center()
                            .gap_1()
                            .child(t!("voice.whisper_model_list").to_string())
                            .child(Icon::new(IconName::ExternalLink).xsmall()),
                    ),
                ),
        )
}

fn model_path_control(
    input: Entity<InputState>,
    button_id: &'static str,
    palette: UiPalette,
    cx: &mut Context<SettingsView>,
) -> gpui::Div {
    let tooltip = t!("voice.select_model").to_string();
    div()
        .w(px(390.0))
        .max_w_full()
        .flex()
        .items_center()
        .gap_1()
        .child(div().min_w_0().flex_1().child(Input::new(&input)))
        .child(
            div()
                .id(button_id)
                .size_8()
                .flex_shrink_0()
                .flex()
                .items_center()
                .justify_center()
                .rounded_md()
                .border_1()
                .border_color(palette.border)
                .bg(palette.secondary)
                .cursor_pointer()
                .hover(move |style| style.bg(palette.accent))
                .tooltip(move |window, cx| Tooltip::new(tooltip.clone()).build(window, cx))
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.choose_voice_model(cx);
                }))
                .child(
                    svg()
                        .path("icons/folder-open.svg")
                        .size_4()
                        .text_color(palette.foreground),
                ),
        )
}
