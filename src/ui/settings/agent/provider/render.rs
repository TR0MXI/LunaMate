//! 渲染供应商列表、连接表单、高级参数折叠项与保存反馈。

use gpui::{
    AnyElement, Context, IntoElement, KeyDownEvent, MouseButton, Render, Window, div, prelude::*,
    px, svg,
};
use gpui_component::{
    StyledExt as _,
    input::{Input, InputContentType},
    select::Select,
    tooltip::Tooltip,
};
use rust_i18n::t;

use lunamate_agent::config::{
    MAX_OUTPUT_TOKENS_MAX, MODEL_CONTEXT_TOKENS_MAX, ModelKind, ModelProvider, TEMPERATURE_MAX,
    TOP_P_MAX,
};

use crate::ui::UiPalette;

use super::{
    super::{
        components::{
            collapsible_header, form_field, optional_field, section_label, status_toast,
            toggle_switch,
        },
        provider_display_name, provider_icon,
    },
    ProviderSettingsView,
    options::{
        REASONING_BUDGET_INDEX, default_provider, model_provider_from_display_name,
        selected_reasoning_index,
    },
};

impl ProviderSettingsView {
    fn render_provider_list(&self, cx: &mut Context<Self>) -> AnyElement {
        let palette = UiPalette::from_app(cx);
        let editing_index = self.editing_index;
        let draft = &self.draft;
        let active_kind = self.active_kind;
        let active_id = draft.selected_model_id(active_kind).map(str::to_owned);
        let visible_count = draft
            .models
            .iter()
            .filter(|model| model.kind == active_kind)
            .count();
        div()
            .w(px(224.0))
            .h_full()
            .flex_shrink_0()
            .flex()
            .flex_col()
            .border_r_1()
            .border_color(palette.border)
            .bg(palette.sidebar)
            .child(
                div()
                    .h(px(46.0))
                    .flex_shrink_0()
                    .flex()
                    .items_center()
                    .gap_1()
                    .border_b_1()
                    .border_color(palette.border)
                    .px_3()
                    .children(
                        [
                            ("llm-kind-chat", ModelKind::ChatCompletions),
                            ("llm-kind-tts", ModelKind::SpeechSynthesis),
                            ("llm-kind-stt", ModelKind::Transcription),
                        ]
                        .into_iter()
                        .map(|(id, kind)| {
                            let selected = active_kind == kind;
                            let tooltip = model_kind_label(kind);
                            div()
                                .id(id)
                                .h(px(28.0))
                                .flex_1()
                                .flex()
                                .items_center()
                                .justify_center()
                                .rounded_md()
                                .text_xs()
                                .cursor_pointer()
                                .bg(if selected {
                                    palette.primary
                                } else {
                                    palette.muted
                                })
                                .text_color(if selected {
                                    palette.primary_foreground
                                } else {
                                    palette.muted_foreground
                                })
                                .tooltip(move |window, cx| {
                                    Tooltip::new(tooltip.clone()).build(window, cx)
                                })
                                .on_click(cx.listener(move |this, _, window, cx| {
                                    this.select_kind(kind, window, cx);
                                }))
                                .child(model_kind_short_label(kind))
                        }),
                    ),
            )
            .child(
                div()
                    .id("llm-model-list")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .p_3()
                    .flex()
                    .flex_col()
                    .gap_2()
                    .when(visible_count == 0, |this| {
                        this.child(
                            div()
                                .flex_1()
                                .min_h(px(180.0))
                                .flex()
                                .flex_col()
                                .items_center()
                                .justify_center()
                                .gap_3()
                                .px_4()
                                .text_sm()
                                .text_color(palette.muted_foreground)
                                .child(
                                    div()
                                        .size(px(40.0))
                                        .flex()
                                        .items_center()
                                        .justify_center()
                                        .rounded_md()
                                        .bg(palette.muted)
                                        .child(
                                            svg()
                                                .path("icons/bot.svg")
                                                .size_5()
                                                .text_color(palette.muted_foreground),
                                        ),
                                )
                                .child(t!("llm.none").to_string()),
                        )
                    })
                    .children(
                        draft
                            .models
                            .iter()
                            .enumerate()
                            .filter_map(|(index, model)| {
                                if model.kind != active_kind {
                                    return None;
                                }
                                let editing = editing_index == Some(index);
                                let active = active_id.as_deref() == Some(model.id.as_str());
                                let model_name = if model.model.trim().is_empty() {
                                    t!("llm.model_unset").to_string()
                                } else {
                                    model.model.clone()
                                };
                                Some(
                                    div()
                                        .id(("llm-model", index))
                                        .min_h(px(66.0))
                                        .rounded_md()
                                        .border_1()
                                        .border_color(if editing {
                                            palette.primary
                                        } else {
                                            palette.border
                                        })
                                        .px_3()
                                        .py_2()
                                        .cursor_pointer()
                                        .bg(if editing {
                                            palette.accent
                                        } else {
                                            palette.sidebar
                                        })
                                        .hover(move |style| style.bg(palette.secondary))
                                        .on_click(cx.listener(move |this, _, window, cx| {
                                            this.select_model(index, window, cx);
                                        }))
                                        .child(
                                            div()
                                                .flex()
                                                .items_center()
                                                .gap_3()
                                                .child(
                                                    div()
                                                        .size_8()
                                                        .flex_shrink_0()
                                                        .flex()
                                                        .items_center()
                                                        .justify_center()
                                                        .rounded_md()
                                                        .bg(if editing {
                                                            palette.primary
                                                        } else {
                                                            palette.muted
                                                        })
                                                        .child(
                                                            svg()
                                                                .path(provider_icon(model.provider))
                                                                .size_4()
                                                                .text_color(if editing {
                                                                    palette.primary_foreground
                                                                } else {
                                                                    palette.foreground
                                                                }),
                                                        ),
                                                )
                                                .child(
                                                    div()
                                                        .flex_1()
                                                        .min_w_0()
                                                        .child(
                                                            div()
                                                                .overflow_hidden()
                                                                .text_ellipsis()
                                                                .text_sm()
                                                                .font_medium()
                                                                .child(model.label.clone()),
                                                        )
                                                        .child(
                                                            div()
                                                                .mt_1()
                                                                .overflow_hidden()
                                                                .text_ellipsis()
                                                                .text_xs()
                                                                .text_color(
                                                                    palette.muted_foreground,
                                                                )
                                                                .child(format!(
                                                                    "{} · {model_name}",
                                                                    provider_display_name(
                                                                        model.provider
                                                                    ),
                                                                )),
                                                        ),
                                                )
                                                .when(active, |this| {
                                                    this.child(
                                                        div()
                                                            .size_5()
                                                            .flex_shrink_0()
                                                            .flex()
                                                            .items_center()
                                                            .justify_center()
                                                            .rounded_full()
                                                            .bg(palette.primary)
                                                            .child(
                                                                svg()
                                                                    .path("icons/check.svg")
                                                                    .size_3()
                                                                    .text_color(
                                                                        palette.primary_foreground,
                                                                    ),
                                                            ),
                                                    )
                                                }),
                                        ),
                                )
                            }),
                    ),
            )
            .child(
                div()
                    .h(px(46.0))
                    .flex_shrink_0()
                    .flex()
                    .items_center()
                    .justify_between()
                    .border_t_1()
                    .border_color(palette.border)
                    .px_3()
                    .child(
                        div()
                            .min_w(px(24.0))
                            .h(px(20.0))
                            .flex()
                            .items_center()
                            .justify_center()
                            .rounded_md()
                            .bg(palette.muted)
                            .px_2()
                            .text_xs()
                            .text_color(palette.muted_foreground)
                            .child(visible_count.to_string()),
                    )
                    .child({
                        let label = t!("llm.add_model").to_string();
                        div()
                            .id("add-llm-model")
                            .size(px(30.0))
                            .flex()
                            .items_center()
                            .justify_center()
                            .rounded_md()
                            .bg(palette.primary)
                            .cursor_pointer()
                            .hover(move |style| style.bg(palette.primary.opacity(0.86)))
                            .tooltip(move |window, cx| {
                                Tooltip::new(label.clone()).build(window, cx)
                            })
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.add_model(window, cx);
                            }))
                            .child(
                                svg()
                                    .path("icons/plus.svg")
                                    .size_4()
                                    .text_color(palette.primary_foreground),
                            )
                    }),
            )
            .into_any_element()
    }

    fn render_editor(&self, cx: &mut Context<Self>) -> AnyElement {
        let palette = UiPalette::from_app(cx);
        let has_model = self.editing_index.is_some();
        let disabled = !has_model;
        let editor_title = self.label_input.read(cx).value().trim().to_owned();
        let provider = has_model.then(|| {
            self.provider_select
                .read(cx)
                .selected_value()
                .and_then(|value| {
                    model_provider_from_display_name(self.active_kind, value.as_ref())
                })
                .unwrap_or_else(|| default_provider(self.active_kind))
        });
        let kind = self.active_kind;
        let local = provider == Some(ModelProvider::LocalWhisper);
        div()
            .id("llm-editor-scroll")
            .flex_1()
            .min_w_0()
            .h_full()
            .overflow_y_scroll()
            .px_7()
            .pb_7()
            .when(!has_model, |this| {
                this.child(
                    div()
                        .min_h(px(240.0))
                        .flex()
                        .flex_col()
                        .items_center()
                        .justify_center()
                        .gap_3()
                        .border_b_1()
                        .border_color(palette.border)
                        .text_center()
                        .text_sm()
                        .text_color(palette.muted_foreground)
                        .child(
                            div()
                                .size(px(48.0))
                                .flex()
                                .items_center()
                                .justify_center()
                                .rounded_md()
                                .bg(palette.muted)
                                .child(
                                    svg()
                                        .path("icons/bot.svg")
                                        .size_6()
                                        .text_color(palette.muted_foreground),
                                ),
                        )
                        .child(t!("llm.no_model_selected").to_string()),
                )
            })
            .when(has_model, |this| {
                this.child(
                    div()
                        .min_h(px(64.0))
                        .flex()
                        .items_center()
                        .justify_between()
                        .gap_4()
                        .border_b_1()
                        .border_color(palette.border)
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .gap_3()
                                .min_w_0()
                                .when_some(provider, |this, provider| {
                                    this.child(
                                        div()
                                            .size_8()
                                            .flex_shrink_0()
                                            .flex()
                                            .items_center()
                                            .justify_center()
                                            .rounded_md()
                                            .bg(palette.muted)
                                            .child(
                                                svg()
                                                    .path(provider_icon(provider))
                                                    .size_5()
                                                    .text_color(palette.foreground),
                                            ),
                                    )
                                })
                                .child(
                                    div()
                                        .min_w_0()
                                        .child(
                                            div()
                                                .overflow_hidden()
                                                .text_ellipsis()
                                                .text_base()
                                                .font_semibold()
                                                .child(if editor_title.is_empty() {
                                                    t!("llm.new_model").to_string()
                                                } else {
                                                    editor_title.clone()
                                                }),
                                        )
                                        .child(
                                            div()
                                                .mt_1()
                                                .text_xs()
                                                .text_color(palette.muted_foreground)
                                                .child(t!("llm.connection").to_string()),
                                        ),
                                ),
                        )
                        .child(
                            div()
                                .id("delete-llm-model")
                                .h(px(32.0))
                                .flex_shrink_0()
                                .flex()
                                .items_center()
                                .justify_center()
                                .gap_2()
                                .rounded_md()
                                .border_1()
                                .border_color(palette.danger)
                                .px_3()
                                .text_xs()
                                .font_medium()
                                .text_color(palette.danger)
                                .cursor_pointer()
                                .hover(move |style| style.bg(palette.danger.opacity(0.12)))
                                .on_click(cx.listener(|this, _, window, cx| {
                                    this.delete_model(window, cx);
                                }))
                                .child(
                                    svg()
                                        .path("icons/trash-2.svg")
                                        .size_3()
                                        .text_color(palette.danger),
                                )
                                .child(t!("llm.delete_model").to_string()),
                        ),
                )
                .child(section_label(t!("llm.connection").to_string(), palette))
                .child(
                    div().w_full().flex().gap_4().children([
                        form_field(
                            t!("llm.name").to_string(),
                            Input::new(&self.label_input).disabled(disabled),
                            palette,
                        ),
                        form_field(
                            t!("llm.provider").to_string(),
                            Select::new(&self.provider_select)
                                .search_placeholder(t!("llm.search_provider").to_string())
                                .disabled(disabled),
                            palette,
                        ),
                    ]),
                )
                .when(!local, |this| {
                    this.child(div().w_full().flex().gap_4().children([
                        form_field(
                            t!("llm.model_id").to_string(),
                            Input::new(&self.model_input).disabled(disabled),
                            palette,
                        ),
                        form_field(
                            t!("llm.endpoint").to_string(),
                            Input::new(&self.endpoint_input).disabled(disabled),
                            palette,
                        ),
                    ]))
                    .child(form_field(
                        t!("llm.api_key").to_string(),
                        Input::new(&self.api_key_input)
                            .mask_toggle()
                            .content_type(InputContentType::Password)
                            .disabled(disabled),
                        palette,
                    ))
                })
                .when(kind == ModelKind::SpeechSynthesis, |this| {
                    this.child(form_field(
                        if provider == Some(ModelProvider::Doubao) {
                            t!("llm.voice_type").to_string()
                        } else {
                            t!("llm.voice").to_string()
                        },
                        Input::new(&self.voice_input).disabled(disabled),
                        palette,
                    ))
                })
                .when(local, |this| {
                    this.child(form_field(
                        t!("llm.local_model_path").to_string(),
                        div()
                            .flex()
                            .items_center()
                            .gap_1()
                            .child(
                                div()
                                    .min_w_0()
                                    .flex_1()
                                    .child(Input::new(&self.local_path_input).disabled(disabled)),
                            )
                            .child(
                                div()
                                    .id("choose-local-whisper-model")
                                    .size_8()
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .rounded_md()
                                    .border_1()
                                    .border_color(palette.border)
                                    .cursor_pointer()
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.choose_local_model(cx);
                                    }))
                                    .child(
                                        svg()
                                            .path("icons/folder-open.svg")
                                            .size_4()
                                            .text_color(palette.foreground),
                                    ),
                            ),
                        palette,
                    ))
                    .child(form_field(
                        t!("llm.whisper_language").to_string(),
                        Select::new(&self.whisper_language_select)
                            .search_placeholder(t!("llm.whisper_language_search").to_string())
                            .disabled(disabled),
                        palette,
                    ))
                    .child(
                        div()
                            .pt_1()
                            .text_xs()
                            .text_color(palette.muted_foreground)
                            .child(t!("llm.whisper_language_hint").to_string()),
                    )
                    .child(form_field(
                        t!("llm.gpu_acceleration").to_string(),
                        toggle_switch("toggle-local-whisper-gpu", self.use_gpu, palette).on_click(
                            cx.listener(|this, _, _, cx| {
                                this.toggle_use_gpu(cx);
                            }),
                        ),
                        palette,
                    ))
                })
                .when(kind == ModelKind::ChatCompletions, |this| {
                    this.child(self.render_advanced(disabled, cx))
                })
            })
            .into_any_element()
    }

    fn render_advanced(&self, disabled: bool, cx: &mut Context<Self>) -> AnyElement {
        let palette = UiPalette::from_app(cx);
        let expanded = self.advanced_expanded;
        let context_window_tokens = self.context_window_tokens_enabled;
        let max_output_tokens = self.max_output_tokens_enabled;
        let temperature = self.temperature_enabled;
        let top_p = self.top_p_enabled;
        let hint = t!("llm.advanced_default_hint").to_string();
        div()
            .w_full()
            .child(collapsible_header(
                "toggle-llm-advanced",
                t!("llm.advanced").to_string(),
                t!("llm.advanced_summary").to_string(),
                expanded,
                palette,
                cx.listener(|this, _, _, cx| this.toggle_advanced(cx)),
            ))
            .when(expanded, |this| {
                this.child(
                    div()
                        .w_full()
                        .rounded_md()
                        .border_1()
                        .border_color(palette.border)
                        .px_4()
                        .pb_4()
                        .mt_2()
                        .child(div().w_full().flex().gap_4().children([
                            form_field(
                                t!("llm.reasoning_effort").to_string(),
                                Select::new(&self.reasoning_select).disabled(disabled),
                                palette,
                            ),
                            if selected_reasoning_index(&self.reasoning_select, cx)
                                == REASONING_BUDGET_INDEX
                            {
                                form_field(
                                    t!("llm.reasoning_budget").to_string(),
                                    Input::new(&self.reasoning_budget_input).disabled(disabled),
                                    palette,
                                )
                            } else {
                                form_field(
                                    t!("llm.reasoning_budget").to_string(),
                                    div()
                                        .h(px(32.0))
                                        .flex()
                                        .items_center()
                                        .text_xs()
                                        .text_color(palette.muted_foreground)
                                        .child(t!("llm.reasoning_budget_unused").to_string()),
                                    palette,
                                )
                            },
                        ]))
                        .child(
                            div().w_full().flex().gap_4().children([
                                optional_field(
                                    "toggle-context-window-tokens",
                                    t!("llm.context_window_tokens").to_string(),
                                    t!("llm.context_window_default_hint").to_string(),
                                    context_window_tokens,
                                    Input::new(&self.context_window_tokens_input)
                                        .disabled(disabled || !context_window_tokens),
                                    palette,
                                    cx.listener(|this, _, _, cx| {
                                        this.toggle_context_window_tokens(cx);
                                    }),
                                ),
                                optional_field(
                                    "toggle-max-output-tokens",
                                    t!("llm.max_output_tokens").to_string(),
                                    hint.clone(),
                                    max_output_tokens,
                                    Input::new(&self.max_output_tokens_input)
                                        .disabled(disabled || !max_output_tokens),
                                    palette,
                                    cx.listener(|this, _, _, cx| {
                                        this.toggle_max_output_tokens(cx);
                                    }),
                                ),
                            ]),
                        )
                        .child(
                            div().w_full().flex().gap_4().children([
                                optional_field(
                                    "toggle-temperature",
                                    t!("llm.temperature").to_string(),
                                    hint.clone(),
                                    temperature,
                                    Input::new(&self.temperature_input)
                                        .disabled(disabled || !temperature),
                                    palette,
                                    cx.listener(|this, _, _, cx| this.toggle_temperature(cx)),
                                ),
                                optional_field(
                                    "toggle-top-p",
                                    t!("llm.top_p").to_string(),
                                    hint,
                                    top_p,
                                    Input::new(&self.top_p_input).disabled(disabled || !top_p),
                                    palette,
                                    cx.listener(|this, _, _, cx| this.toggle_top_p(cx)),
                                ),
                            ]),
                        )
                        .child(
                            div()
                                .pt_3()
                                .text_xs()
                                .text_color(palette.muted_foreground)
                                .child(
                                    t!(
                                        "llm.advanced_range_hint",
                                        context = MODEL_CONTEXT_TOKENS_MAX,
                                        tokens = MAX_OUTPUT_TOKENS_MAX,
                                        temperature = format!("{TEMPERATURE_MAX}"),
                                        top_p = format!("{TOP_P_MAX}")
                                    )
                                    .to_string(),
                                ),
                        ),
                )
            })
            .into_any_element()
    }
}

fn model_kind_label(kind: ModelKind) -> String {
    match kind {
        ModelKind::ChatCompletions => t!("llm.chat_completions").to_string(),
        ModelKind::SpeechSynthesis => t!("llm.speech_synthesis").to_string(),
        ModelKind::Transcription => t!("llm.transcription").to_string(),
    }
}

fn model_kind_short_label(kind: ModelKind) -> String {
    match kind {
        ModelKind::ChatCompletions => "Chat".to_owned(),
        ModelKind::SpeechSynthesis => "TTS".to_owned(),
        ModelKind::Transcription => "STT".to_owned(),
    }
}

impl Render for ProviderSettingsView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let palette = UiPalette::from_app(cx);
        let status = self.status.clone();
        div()
            .relative()
            .size_full()
            .min_w_0()
            .flex()
            .text_color(palette.foreground)
            .on_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
                if event.keystroke.key.eq_ignore_ascii_case("escape")
                    && this.cancel_input_edit(window, cx)
                {
                    window.prevent_default();
                    cx.stop_propagation();
                }
            }))
            .on_mouse_down(MouseButton::Left, |_, window, _| window.blur())
            .child(self.render_provider_list(cx))
            .child(self.render_editor(cx))
            .when_some(status, |this, status| {
                this.child(status_toast(status, palette))
            })
    }
}
