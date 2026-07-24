//! 渲染 Agent Provider 列表、连接表单、系统提示词与保存反馈。

use gpui::{AnyElement, Context, IntoElement, Render, Window, div, prelude::*, px, svg};
use gpui_component::{
    StyledExt as _,
    input::{Input, InputContentType},
    select::Select,
};
use rust_i18n::t;

use crate::agent::palette::AgentPalette;

use super::{AgentSettingsView, provider_display_name};

impl AgentSettingsView {
    fn render_model_list(&self, cx: &mut Context<Self>) -> AnyElement {
        let palette = AgentPalette::from_app(cx);
        let editing_index = self.editing_index;
        let active_id = self.draft.selected_model.clone();
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
                                    .child(self.draft.models.len().to_string()),
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
                    .when(self.draft.models.is_empty(), |this| {
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
                    .children(self.draft.models.iter().enumerate().map(|(index, model)| {
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
                                                svg().path("icons/bot.svg").size_4().text_color(
                                                    if editing {
                                                        palette.primary_foreground
                                                    } else {
                                                        palette.muted_foreground
                                                    },
                                                ),
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
        let has_model = self.editing_index.is_some();
        let model_fields_disabled = !has_model || self.is_saving;
        let editor_title = self.label_input.read(cx).value().trim().to_owned();
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
                            Input::new(&self.label_input).disabled(model_fields_disabled),
                            palette,
                        ),
                        form_field(
                            t!("llm.provider").to_string(),
                            Select::new(&self.provider_select)
                                .search_placeholder(t!("llm.search_provider").to_string())
                                .disabled(model_fields_disabled),
                            palette,
                        ),
                    ]),
                )
                .child(div().w_full().flex().gap_4().children([
                    form_field(
                        t!("llm.model_id").to_string(),
                        Input::new(&self.model_input).disabled(model_fields_disabled),
                        palette,
                    ),
                    form_field(
                        t!("llm.endpoint").to_string(),
                        Input::new(&self.endpoint_input).disabled(model_fields_disabled),
                        palette,
                    ),
                ]))
                .child(form_field(
                    t!("llm.api_key").to_string(),
                    Input::new(&self.api_key_input)
                        .mask_toggle()
                        .content_type(InputContentType::Password)
                        .disabled(model_fields_disabled),
                    palette,
                ))
            })
            .child(section_label(t!("llm.system_prompt").to_string(), palette))
            .child(
                div().w_full().h(px(180.0)).child(
                    Input::new(&self.system_prompt_input)
                        .h_full()
                        .disabled(self.is_saving),
                ),
            )
            .into_any_element()
    }
}

impl Render for AgentSettingsView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let palette = AgentPalette::from_app(cx);
        let status = self.status.clone();
        div()
            .relative()
            .size_full()
            .min_w_0()
            .flex()
            .flex_col()
            .text_color(palette.foreground)
            .child(
                div()
                    .h(px(54.0))
                    .flex_shrink_0()
                    .flex()
                    .items_center()
                    .justify_between()
                    .border_b_1()
                    .border_color(palette.border)
                    .px_5()
                    .child(
                        div()
                            .text_base()
                            .font_semibold()
                            .child(t!("settings.conversation_title").to_string()),
                    )
                    .child(
                        div()
                            .id("save-llm-settings")
                            .h(px(34.0))
                            .flex()
                            .items_center()
                            .justify_center()
                            .gap_2()
                            .rounded_md()
                            .px_4()
                            .text_sm()
                            .font_medium()
                            .bg(if self.is_saving {
                                palette.muted
                            } else {
                                palette.primary
                            })
                            .text_color(if self.is_saving {
                                palette.muted_foreground
                            } else {
                                palette.primary_foreground
                            })
                            .cursor_pointer()
                            .hover(move |style| style.bg(palette.accent))
                            .on_click(cx.listener(|this, _, _, cx| this.save(cx)))
                            .child(svg().path("icons/check.svg").size_4().text_color(
                                if self.is_saving {
                                    palette.muted_foreground
                                } else {
                                    palette.primary_foreground
                                },
                            ))
                            .child(if self.is_saving {
                                t!("common.saving").to_string()
                            } else {
                                t!("common.save").to_string()
                            }),
                    ),
            )
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .flex()
                    .child(self.render_model_list(cx))
                    .child(self.render_editor(cx)),
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

fn section_label(title: String, palette: AgentPalette) -> gpui::Div {
    div()
        .pt_6()
        .pb_2()
        .text_xs()
        .font_semibold()
        .text_color(palette.primary)
        .child(title)
}

fn form_field(label: String, control: impl IntoElement, palette: AgentPalette) -> gpui::Div {
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
