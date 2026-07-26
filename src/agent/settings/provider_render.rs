//! 渲染供应商列表、连接表单、高级参数折叠项与保存反馈。

use gpui::{AnyElement, Context, IntoElement, Render, Window, div, prelude::*, px, svg};
use gpui_component::{
    StyledExt as _,
    input::{Input, InputContentType},
    select::Select,
};
use rust_i18n::t;

use crate::{
    agent::palette::AgentPalette,
    config::{MAX_OUTPUT_TOKENS_MAX, TEMPERATURE_MAX, TOP_P_MAX},
};

use super::{
    components::{
        collapsible_header, form_field, optional_field, page_header, section_label, status_toast,
    },
    provider::AgentSettingsView,
    provider_display_name, provider_icon,
};

impl AgentSettingsView {
    fn render_provider_list(&self, cx: &mut Context<Self>) -> AnyElement {
        let palette = AgentPalette::from_app(cx);
        let editing_index = self.editing_index();
        let draft = self.draft();
        let active_id = draft.selected_model.clone();
        div()
            .w(px(260.0))
            .h_full()
            .flex_shrink_0()
            .flex()
            .flex_col()
            .border_r_1()
            .border_color(palette.border)
            .bg(palette.sidebar)
            .child(
                div()
                    .flex_shrink_0()
                    .flex()
                    .flex_col()
                    .gap_3()
                    .border_b_1()
                    .border_color(palette.border)
                    .p_4()
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .justify_between()
                            .child(
                                div()
                                    .text_sm()
                                    .font_semibold()
                                    .child(t!("llm.models").to_string()),
                            )
                            .child(
                                div()
                                    .rounded_md()
                                    .bg(palette.muted)
                                    .px_2()
                                    .py(px(2.0))
                                    .text_xs()
                                    .text_color(palette.muted_foreground)
                                    .child(draft.models.len().to_string()),
                            ),
                    )
                    .child(
                        div()
                            .id("add-llm-model")
                            .h(px(34.0))
                            .w_full()
                            .flex()
                            .items_center()
                            .justify_center()
                            .gap_2()
                            .rounded_md()
                            .bg(palette.primary)
                            .text_color(palette.primary_foreground)
                            .text_sm()
                            .font_medium()
                            .cursor_pointer()
                            .hover(move |style| style.bg(palette.primary.opacity(0.86)))
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.add_model(window, cx);
                            }))
                            .child(
                                svg()
                                    .path("icons/plus.svg")
                                    .size_4()
                                    .text_color(palette.primary_foreground),
                            )
                            .child(t!("llm.add_model").to_string()),
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
                    .when(draft.models.is_empty(), |this| {
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
                    .children(draft.models.iter().enumerate().map(|(index, model)| {
                        let editing = editing_index == Some(index);
                        let active = active_id.as_deref() == Some(model.id.as_str());
                        let model_name = if model.model.trim().is_empty() {
                            t!("llm.model_unset").to_string()
                        } else {
                            model.model.clone()
                        };
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
                                                    .text_color(palette.muted_foreground)
                                                    .child(format!(
                                                        "{} · {model_name}",
                                                        provider_display_name(model.provider),
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
                                                        .text_color(palette.primary_foreground),
                                                ),
                                        )
                                    }),
                            )
                    })),
            )
            .into_any_element()
    }

    fn render_editor(&self, cx: &mut Context<Self>) -> AnyElement {
        let palette = AgentPalette::from_app(cx);
        let has_model = self.editing_index().is_some();
        let disabled = !has_model || self.is_saving();
        let inputs = self.inputs();
        let editor_title = inputs.label.read(cx).value().trim().to_owned();
        let provider = self
            .editing_index()
            .and_then(|index| self.draft().models.get(index))
            .map(|model| model.provider);
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
                            Input::new(inputs.label).disabled(disabled),
                            palette,
                        ),
                        form_field(
                            t!("llm.provider").to_string(),
                            Select::new(inputs.provider)
                                .search_placeholder(t!("llm.search_provider").to_string())
                                .disabled(disabled),
                            palette,
                        ),
                    ]),
                )
                .child(div().w_full().flex().gap_4().children([
                    form_field(
                        t!("llm.model_id").to_string(),
                        Input::new(inputs.model).disabled(disabled),
                        palette,
                    ),
                    form_field(
                        t!("llm.endpoint").to_string(),
                        Input::new(inputs.endpoint).disabled(disabled),
                        palette,
                    ),
                ]))
                .child(form_field(
                    t!("llm.api_key").to_string(),
                    Input::new(inputs.api_key)
                        .mask_toggle()
                        .content_type(InputContentType::Password)
                        .disabled(disabled),
                    palette,
                ))
                .child(self.render_advanced(disabled, cx))
            })
            .into_any_element()
    }

    fn render_advanced(&self, disabled: bool, cx: &mut Context<Self>) -> AnyElement {
        let palette = AgentPalette::from_app(cx);
        let expanded = self.advanced_expanded;
        let inputs = self.inputs();
        let [max_output_tokens, temperature, top_p] = self.advanced_toggles();
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
                                Select::new(inputs.reasoning).disabled(disabled),
                                palette,
                            ),
                            if self.reasoning_is_budget(cx) {
                                form_field(
                                    t!("llm.reasoning_budget").to_string(),
                                    Input::new(inputs.reasoning_budget).disabled(disabled),
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
                                    "toggle-max-output-tokens",
                                    t!("llm.max_output_tokens").to_string(),
                                    hint.clone(),
                                    max_output_tokens,
                                    Input::new(inputs.max_output_tokens)
                                        .disabled(disabled || !max_output_tokens),
                                    palette,
                                    cx.listener(|this, _, _, cx| {
                                        this.toggle_max_output_tokens(cx);
                                    }),
                                ),
                                optional_field(
                                    "toggle-temperature",
                                    t!("llm.temperature").to_string(),
                                    hint.clone(),
                                    temperature,
                                    Input::new(inputs.temperature)
                                        .disabled(disabled || !temperature),
                                    palette,
                                    cx.listener(|this, _, _, cx| this.toggle_temperature(cx)),
                                ),
                                optional_field(
                                    "toggle-top-p",
                                    t!("llm.top_p").to_string(),
                                    hint,
                                    top_p,
                                    Input::new(inputs.top_p).disabled(disabled || !top_p),
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

impl Render for AgentSettingsView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let palette = AgentPalette::from_app(cx);
        let status = self.status().map(str::to_owned);
        let saving = self.is_saving();
        div()
            .relative()
            .size_full()
            .min_w_0()
            .flex()
            .flex_col()
            .text_color(palette.foreground)
            .child(page_header(
                "save-llm-settings",
                t!("settings.provider_title").to_string(),
                if saving {
                    t!("common.saving").to_string()
                } else {
                    t!("common.save").to_string()
                },
                saving,
                palette,
                cx.listener(|this, _, _, cx| this.save(cx)),
            ))
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .flex()
                    .child(self.render_provider_list(cx))
                    .child(self.render_editor(cx)),
            )
            .when_some(status, |this, status| {
                this.child(status_toast(status, palette))
            })
    }
}
