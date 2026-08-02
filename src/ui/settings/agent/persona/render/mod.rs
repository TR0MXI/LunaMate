//! 渲染人格列表、编辑分区与危险操作确认框。

mod context;

use gpui::{
    AnyElement, Context, IntoElement, KeyDownEvent, MouseButton, Render, Window, div, prelude::*,
    px, svg,
};
use gpui_component::{StyledExt as _, input::Input, select::Select, tooltip::Tooltip};
use rust_i18n::t;

use crate::ui::UiPalette;

use super::{
    super::{
        components::{confirm_overlay, form_field, section_label, status_toast},
        provider_display_name, provider_icon,
    },
    PersonaPage, PersonaSettingsView,
    memory::MemoryScope,
};

const PERSONA_LIST_WIDTH: f32 = 224.0;

impl PersonaSettingsView {
    fn render_persona_list(&self, cx: &mut Context<Self>) -> AnyElement {
        let palette = UiPalette::from_app(cx);
        let editing_index = self.editing_index;
        let draft = &self.draft;
        let providers = self.providers.clone();
        let can_delete = draft.personas.len() > 1;

        div()
            .debug_selector(|| "persona-sidebar".to_owned())
            .w(px(PERSONA_LIST_WIDTH))
            .h_full()
            .flex_shrink_0()
            .flex()
            .flex_col()
            .border_r_1()
            .border_color(palette.border)
            .bg(palette.sidebar)
            .child(
                div()
                    .id("persona-list")
                    .debug_selector(|| "persona-list".to_owned())
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .p_2()
                    .children(draft.personas.iter().enumerate().map(|(index, persona)| {
                        let editing = editing_index == Some(index);
                        let persona_id = persona.id.clone();
                        let delete_id = persona.id.clone();
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
                        let delete_label = t!("persona.delete_persona").to_string();

                        div()
                            .id(("persona", index))
                            .min_h(px(58.0))
                            .flex()
                            .items_center()
                            .gap_2()
                            .rounded_md()
                            .border_1()
                            .border_color(if editing {
                                palette.primary
                            } else {
                                palette.border
                            })
                            .bg(if editing {
                                palette.accent
                            } else {
                                palette.sidebar
                            })
                            .pl_2()
                            .pr_1()
                            .py_2()
                            .cursor_pointer()
                            .hover(move |style| style.bg(palette.secondary))
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.select_persona(index, window, cx);
                            }))
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
                                    .min_w_0()
                                    .flex_1()
                                    .child(
                                        div()
                                            .overflow_hidden()
                                            .text_ellipsis()
                                            .whitespace_nowrap()
                                            .text_sm()
                                            .font_medium()
                                            .child(persona.name.clone()),
                                    )
                                    .child(
                                        div()
                                            .mt_1()
                                            .overflow_hidden()
                                            .text_ellipsis()
                                            .whitespace_nowrap()
                                            .text_xs()
                                            .text_color(palette.muted_foreground)
                                            .child(subtitle),
                                    ),
                            )
                            .child(
                                div()
                                    .id(format!("delete-persona:{persona_id}"))
                                    .debug_selector(move || format!("persona-delete-{index}"))
                                    .size(px(28.0))
                                    .flex_shrink_0()
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .rounded_md()
                                    .text_color(if can_delete {
                                        palette.danger
                                    } else {
                                        palette.muted_foreground
                                    })
                                    .tooltip(move |window, cx| {
                                        Tooltip::new(delete_label.clone()).build(window, cx)
                                    })
                                    .when(can_delete, |this| {
                                        this.cursor_pointer()
                                            .hover(move |style| {
                                                style.bg(palette.danger.opacity(0.12))
                                            })
                                            .on_click(cx.listener(move |this, _, _, cx| {
                                                cx.stop_propagation();
                                                this.request_delete_persona(delete_id.clone(), cx);
                                            }))
                                    })
                                    .child(svg().path("icons/trash-2.svg").size_4().text_color(
                                        if can_delete {
                                            palette.danger
                                        } else {
                                            palette.muted_foreground
                                        },
                                    )),
                            )
                    })),
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
                            .debug_selector(|| "persona-count".to_owned())
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
                            .child(draft.personas.len().to_string()),
                    )
                    .child({
                        let label = t!("persona.add_persona").to_string();
                        div()
                            .id("add-persona")
                            .debug_selector(|| "persona-add".to_owned())
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
                                this.add_persona(window, cx);
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

    fn render_tabs(&self, cx: &mut Context<Self>) -> AnyElement {
        let palette = UiPalette::from_app(cx);
        let active = self.active_page;
        let pages = [
            (
                PersonaPage::Definition,
                t!("persona.tab_definition").to_string(),
            ),
            (PersonaPage::Context, t!("persona.tab_context").to_string()),
            (
                PersonaPage::MediumMemory,
                t!("persona.memory_medium").to_string(),
            ),
            (
                PersonaPage::LongMemory,
                t!("persona.memory_long").to_string(),
            ),
            (
                PersonaPage::Settings,
                t!("persona.tab_settings").to_string(),
            ),
        ];

        div()
            .debug_selector(|| "persona-tabs".to_owned())
            .w_full()
            .h(px(46.0))
            .flex_shrink_0()
            .flex()
            .items_end()
            .border_b_1()
            .border_color(palette.border)
            .children(pages.into_iter().map(|(page, label)| {
                let selected = active == page;
                div()
                    .id(("persona-tab", page as usize))
                    .debug_selector(move || format!("persona-tab-{}", page as usize))
                    .h(px(45.0))
                    .min_w_0()
                    .flex_1()
                    .flex()
                    .items_center()
                    .justify_center()
                    .border_b_2()
                    .border_color(if selected {
                        palette.primary
                    } else {
                        palette.primary.opacity(0.0)
                    })
                    .overflow_hidden()
                    .px_2()
                    .text_ellipsis()
                    .whitespace_nowrap()
                    .text_sm()
                    .font_medium()
                    .text_color(if selected {
                        palette.primary
                    } else {
                        palette.muted_foreground
                    })
                    .cursor_pointer()
                    .hover(move |style| style.text_color(palette.foreground))
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.select_page(page, window, cx);
                    }))
                    .child(label)
            }))
            .into_any_element()
    }

    fn render_definition(&self, cx: &mut Context<Self>) -> AnyElement {
        let palette = UiPalette::from_app(cx);
        div()
            .id("persona-definition-scroll")
            .size_full()
            .overflow_y_scroll()
            .px_6()
            .pb_6()
            .child(div().pt_3().child(form_field(
                t!("persona.name").to_string(),
                Input::new(&self.name_input),
                palette,
            )))
            .child(section_label(t!("llm.system_prompt").to_string(), palette))
            .child(
                div()
                    .w_full()
                    .h(px(220.0))
                    .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .child(Input::new(&self.system_prompt_input).h_full()),
            )
            .child(section_label(
                t!("persona.input_prompt").to_string(),
                palette,
            ))
            .child(
                div()
                    .w_full()
                    .h(px(150.0))
                    .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .child(Input::new(&self.input_prompt_input).h_full()),
            )
            .into_any_element()
    }

    fn render_persona_settings(&self, cx: &mut Context<Self>) -> AnyElement {
        let palette = UiPalette::from_app(cx);
        div()
            .id("persona-settings-scroll")
            .size_full()
            .overflow_y_scroll()
            .px_6()
            .pb_6()
            .child(
                div().pt_3().child(form_field(
                    t!("persona.conversation_model").to_string(),
                    Select::new(&self.provider_select)
                        .search_placeholder(t!("llm.search_provider").to_string()),
                    palette,
                )),
            )
            .child(form_field(
                t!("persona.speech_synthesis_model").to_string(),
                Select::new(&self.tts_select)
                    .search_placeholder(t!("llm.search_provider").to_string()),
                palette,
            ))
            .child(form_field(
                t!("persona.live2d_model").to_string(),
                Select::new(&self.live2d_select)
                    .search_placeholder(t!("persona.search_live2d").to_string()),
                palette,
            ))
            .child(
                div()
                    .mt_7()
                    .pt_5()
                    .border_t_1()
                    .border_color(palette.border)
                    .flex()
                    .justify_end()
                    .child(
                        div()
                            .id("clear-all-memory")
                            .h(px(32.0))
                            .flex()
                            .items_center()
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
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.request_clear_memory(MemoryScope::All, cx);
                            }))
                            .child(
                                svg()
                                    .path("icons/trash-2.svg")
                                    .size_4()
                                    .text_color(palette.danger),
                            )
                            .child(t!("persona.memory_clear_all").to_string()),
                    ),
            )
            .into_any_element()
    }

    fn render_editor(&self, cx: &mut Context<Self>) -> AnyElement {
        let palette = UiPalette::from_app(cx);
        let has_persona = self.editing_index.is_some();
        div()
            .min_w_0()
            .flex_1()
            .h_full()
            .flex()
            .flex_col()
            .when(!has_persona, |this| {
                this.child(
                    div()
                        .size_full()
                        .flex()
                        .items_center()
                        .justify_center()
                        .text_sm()
                        .text_color(palette.muted_foreground)
                        .child(t!("persona.none").to_string()),
                )
            })
            .when(has_persona, |this| {
                this.child(self.render_tabs(cx))
                    .child(div().min_h_0().flex_1().child(match self.active_page {
                        PersonaPage::Definition => self.render_definition(cx),
                        PersonaPage::Context => self.render_context(cx),
                        PersonaPage::MediumMemory | PersonaPage::LongMemory => {
                            div().size_full().into_any_element()
                        }
                        PersonaPage::Settings => self.render_persona_settings(cx),
                    }))
            })
            .into_any_element()
    }
}

impl Render for PersonaSettingsView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let palette = UiPalette::from_app(cx);
        let status = self.status.clone();
        let confirm = self.confirm_prompt();
        div()
            .relative()
            .size_full()
            .min_w_0()
            .flex()
            .text_color(palette.foreground)
            .on_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
                this.handle_key_down(event, window, cx);
            }))
            .on_mouse_down(MouseButton::Left, |_, window, _| window.blur())
            .child(self.render_persona_list(cx))
            .child(self.render_editor(cx))
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
