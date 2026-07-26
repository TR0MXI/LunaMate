//! 渲染人格列表、提示词表单、记忆用量与清除操作的二次确认框。

use gpui::{AnyElement, Context, IntoElement, Render, Window, div, prelude::*, px, svg};
use gpui_component::{StyledExt as _, input::Input, select::Select};
use rust_i18n::t;

use crate::{
    agent::{MemoryScope, palette::AgentPalette},
    config::{CONTEXT_KIB_MAX, CONTEXT_MESSAGES_MAX, MAX_PERSONAS},
};

use super::{
    components::{
        collapsible_header, confirm_overlay, danger_button, form_field, optional_field,
        page_header, section_label, status_toast, usage_card,
    },
    persona::PersonaSettingsView,
    provider_display_name, provider_icon,
};

impl PersonaSettingsView {
    fn render_persona_list(&self, cx: &mut Context<Self>) -> AnyElement {
        let palette = AgentPalette::from_app(cx);
        let editing_index = self.editing_index();
        let draft = self.draft();
        let active_id = draft.selected.clone();
        let providers = self.providers().clone();
        div()
            .w(px(240.0))
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
                                    .child(t!("persona.personas").to_string()),
                            )
                            .child(
                                div()
                                    .rounded_md()
                                    .bg(palette.muted)
                                    .px_2()
                                    .py(px(2.0))
                                    .text_xs()
                                    .text_color(palette.muted_foreground)
                                    .child(format!("{}/{MAX_PERSONAS}", draft.personas.len())),
                            ),
                    )
                    .child(
                        div()
                            .id("add-persona")
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
                                this.add_persona(window, cx);
                            }))
                            .child(
                                svg()
                                    .path("icons/plus.svg")
                                    .size_4()
                                    .text_color(palette.primary_foreground),
                            )
                            .child(t!("persona.add_persona").to_string()),
                    ),
            )
            .child(
                div()
                    .id("persona-list")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .p_3()
                    .flex()
                    .flex_col()
                    .gap_2()
                    .children(draft.personas.iter().enumerate().map(|(index, persona)| {
                        let editing = editing_index == Some(index);
                        let active = active_id.as_deref() == Some(persona.id.as_str());
                        let bound = persona
                            .model
                            .as_deref()
                            .and_then(|id| providers.model(id))
                            .map(|model| model.provider);
                        let subtitle = match (&persona.model, bound) {
                            (Some(_), Some(provider)) => provider_display_name(provider).to_owned(),
                            (Some(_), None) => t!("persona.provider_missing").to_string(),
                            (None, _) => t!("persona.provider_inherit").to_string(),
                        };
                        div()
                            .id(("persona", index))
                            .min_h(px(62.0))
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
                                this.select_persona(index, window, cx);
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
                                                    .path(bound.map_or_else(
                                                        || "icons/user-round.svg".to_owned(),
                                                        provider_icon,
                                                    ))
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
                                                    .child(persona.name.clone()),
                                            )
                                            .child(
                                                div()
                                                    .mt_1()
                                                    .overflow_hidden()
                                                    .text_ellipsis()
                                                    .text_xs()
                                                    .text_color(palette.muted_foreground)
                                                    .child(subtitle),
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

    fn render_memory(&self, cx: &mut Context<Self>) -> AnyElement {
        let palette = AgentPalette::from_app(cx);
        let usage = self.usage();
        let error = self.usage_error().map(str::to_owned);
        let enabled = !self.is_saving();
        let value = |text: Option<String>| text.unwrap_or_else(|| "—".to_owned());
        div()
            .w_full()
            .child(section_label(t!("persona.memory").to_string(), palette))
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap_3()
                    .pb_2()
                    .child(
                        div()
                            .min_w_0()
                            .flex_1()
                            .text_xs()
                            .text_color(palette.muted_foreground)
                            .child(t!("persona.memory_hint").to_string()),
                    )
                    .child(
                        div()
                            .id("reload-persona-usage")
                            .h(px(28.0))
                            .flex_shrink_0()
                            .flex()
                            .items_center()
                            .gap_2()
                            .rounded_md()
                            .border_1()
                            .border_color(palette.border)
                            .px_3()
                            .text_xs()
                            .cursor_pointer()
                            .hover(move |style| style.bg(palette.secondary))
                            .on_click(cx.listener(|this, _, _, cx| this.reload_usage(cx)))
                            .child(
                                svg()
                                    .path("icons/refresh-cw.svg")
                                    .size_3()
                                    .text_color(palette.muted_foreground),
                            )
                            .child(t!("persona.memory_refresh").to_string()),
                    ),
            )
            .when_some(error, |this, error| {
                this.child(
                    div()
                        .mb_2()
                        .rounded_md()
                        .border_1()
                        .border_color(palette.danger)
                        .bg(palette.danger.opacity(0.08))
                        .p_3()
                        .text_xs()
                        .text_color(palette.foreground)
                        .child(error),
                )
            })
            .child(
                div()
                    .w_full()
                    .flex()
                    .gap_3()
                    .child(usage_card(
                        t!("persona.memory_context").to_string(),
                        value(usage.map(|usage| {
                            format!(
                                "{} / {}",
                                usage.context.messages, usage.context.max_messages
                            )
                        })),
                        value(usage.map(|usage| {
                            t!(
                                "persona.memory_context_bytes",
                                used = format_kib(usage.context.bytes),
                                total = format_kib(usage.context.max_bytes)
                            )
                            .to_string()
                        })),
                        palette,
                        danger_button(
                            "clear-context-memory",
                            t!("persona.memory_clear").to_string(),
                            enabled,
                            palette,
                            cx.listener(|this, _, _, cx| {
                                this.request_clear_memory(MemoryScope::Context, cx);
                            }),
                        ),
                    ))
                    .child(usage_card(
                        t!("persona.memory_medium").to_string(),
                        value(usage.map(|usage| usage.medium.to_string())),
                        t!("persona.memory_medium_hint").to_string(),
                        palette,
                        danger_button(
                            "clear-medium-memory",
                            t!("persona.memory_clear").to_string(),
                            enabled,
                            palette,
                            cx.listener(|this, _, _, cx| {
                                this.request_clear_memory(MemoryScope::Medium, cx);
                            }),
                        ),
                    ))
                    .child(usage_card(
                        t!("persona.memory_long").to_string(),
                        value(usage.map(|usage| usage.long.to_string())),
                        t!("persona.memory_long_hint").to_string(),
                        palette,
                        danger_button(
                            "clear-long-memory",
                            t!("persona.memory_clear").to_string(),
                            enabled,
                            palette,
                            cx.listener(|this, _, _, cx| {
                                this.request_clear_memory(MemoryScope::Long, cx);
                            }),
                        ),
                    )),
            )
            .child(
                div()
                    .pt_3()
                    .flex()
                    .items_center()
                    .justify_end()
                    .child(danger_button(
                        "clear-all-memory",
                        t!("persona.memory_clear_all").to_string(),
                        enabled,
                        palette,
                        cx.listener(|this, _, _, cx| {
                            this.request_clear_memory(MemoryScope::All, cx);
                        }),
                    )),
            )
            .into_any_element()
    }

    fn render_editor(&self, cx: &mut Context<Self>) -> AnyElement {
        let palette = AgentPalette::from_app(cx);
        let has_persona = self.editing_index().is_some();
        let disabled = !has_persona || self.is_saving();
        let form = self.form();
        let [messages_enabled, size_enabled] = self.context_toggles();
        let title = form.name.read(cx).value().trim().to_owned();
        let hint = t!("persona.context_default_hint").to_string();
        div()
            .id("persona-editor-scroll")
            .flex_1()
            .min_w_0()
            .h_full()
            .overflow_y_scroll()
            .px_7()
            .pb_7()
            .when(!has_persona, |this| {
                this.child(
                    div()
                        .min_h(px(240.0))
                        .flex()
                        .items_center()
                        .justify_center()
                        .text_sm()
                        .text_color(palette.muted_foreground)
                        .child(t!("persona.none").to_string()),
                )
            })
            .when(has_persona, |this| {
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
                                        .child(if title.is_empty() {
                                            t!("persona.new_persona").to_string()
                                        } else {
                                            title.clone()
                                        }),
                                )
                                .child(
                                    div()
                                        .mt_1()
                                        .text_xs()
                                        .text_color(palette.muted_foreground)
                                        .child(t!("persona.identity").to_string()),
                                ),
                        )
                        .child(danger_button(
                            "delete-persona",
                            t!("persona.delete_persona").to_string(),
                            !self.is_saving(),
                            palette,
                            cx.listener(|this, _, _, cx| this.request_delete_persona(cx)),
                        )),
                )
                .child(section_label(t!("persona.identity").to_string(), palette))
                .child(
                    div().w_full().flex().gap_4().children([
                        form_field(
                            t!("persona.name").to_string(),
                            Input::new(form.name).disabled(disabled),
                            palette,
                        ),
                        form_field(
                            t!("persona.provider").to_string(),
                            Select::new(form.provider)
                                .search_placeholder(t!("llm.search_provider").to_string())
                                .disabled(disabled),
                            palette,
                        ),
                    ]),
                )
                .child(section_label(t!("llm.system_prompt").to_string(), palette))
                .child(
                    div()
                        .w_full()
                        .h(px(200.0))
                        .child(Input::new(form.system_prompt).h_full().disabled(disabled)),
                )
                .child(self.render_memory(cx))
                .child(collapsible_header(
                    "toggle-persona-advanced",
                    t!("persona.advanced").to_string(),
                    t!("persona.advanced_summary").to_string(),
                    self.advanced_expanded,
                    palette,
                    cx.listener(|this, _, _, cx| this.toggle_advanced(cx)),
                ))
                .when(self.advanced_expanded, |this| {
                    this.child(
                        div()
                            .w_full()
                            .rounded_md()
                            .border_1()
                            .border_color(palette.border)
                            .px_4()
                            .pb_4()
                            .mt_2()
                            .child(
                                div().w_full().flex().gap_4().children([
                                    optional_field(
                                        "toggle-context-messages",
                                        t!("persona.context_messages").to_string(),
                                        hint.clone(),
                                        messages_enabled,
                                        Input::new(form.context_messages)
                                            .disabled(disabled || !messages_enabled),
                                        palette,
                                        cx.listener(|this, _, _, cx| {
                                            this.toggle_context_messages(cx);
                                        }),
                                    ),
                                    optional_field(
                                        "toggle-context-size",
                                        t!("persona.context_size").to_string(),
                                        hint,
                                        size_enabled,
                                        Input::new(form.context_kib)
                                            .disabled(disabled || !size_enabled),
                                        palette,
                                        cx.listener(|this, _, _, cx| this.toggle_context_size(cx)),
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
                                            "persona.context_range_hint",
                                            messages = CONTEXT_MESSAGES_MAX,
                                            size = CONTEXT_KIB_MAX
                                        )
                                        .to_string(),
                                    ),
                            ),
                    )
                })
            })
            .into_any_element()
    }
}

impl Render for PersonaSettingsView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let palette = AgentPalette::from_app(cx);
        let status = self.status().map(str::to_owned);
        let saving = self.is_saving();
        let confirm = self.confirm_prompt();
        div()
            .relative()
            .size_full()
            .min_w_0()
            .flex()
            .flex_col()
            .text_color(palette.foreground)
            .child(page_header(
                "save-persona-settings",
                t!("settings.persona_title").to_string(),
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
                    .child(self.render_persona_list(cx))
                    .child(self.render_editor(cx)),
            )
            .when_some(status, |this, status| {
                this.child(status_toast(status, palette))
            })
            .when_some(confirm, |this, confirm| {
                this.child(confirm_overlay(
                    confirm.title,
                    confirm.message,
                    confirm.confirm,
                    t!("common.cancel").to_string(),
                    palette,
                    cx.listener(|this, _, window, cx| this.accept_confirm(window, cx)),
                    cx.listener(|this, _, _, cx| this.cancel_confirm(cx)),
                ))
            })
    }
}

fn format_kib(bytes: usize) -> String {
    format!("{:.1} KiB", bytes as f64 / 1024.0)
}
