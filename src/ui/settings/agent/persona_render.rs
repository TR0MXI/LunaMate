//! 渲染人格列表、五个编辑分区、上下文气泡与危险操作确认框。

use gpui::{
    AnyElement, Context, Entity, IntoElement, MouseButton, Pixels, Point, Render, Window, canvas,
    div, prelude::*, px, svg,
};
use gpui_component::{
    Sizable as _, StyledExt as _,
    input::Input,
    menu::{ContextMenuExt as _, PopupMenuItem},
    select::Select,
    tooltip::Tooltip,
};
use rust_i18n::t;

use lunamate_agent::ChatRole;

use crate::ui::UiPalette;

use super::{
    components::{confirm_overlay, form_field, section_label, status_toast},
    persona::{ContextMessageEditor, MemoryScope, PersonaPage, PersonaSettingsView},
    provider_display_name, provider_icon,
};

const PERSONA_LIST_WIDTH: f32 = 224.0;
const MESSAGE_ICON_SIZE: f32 = 28.0;

impl PersonaSettingsView {
    fn render_persona_list(&self, cx: &mut Context<Self>) -> AnyElement {
        let palette = UiPalette::from_app(cx);
        let editing_index = self.editing_index();
        let draft = self.draft();
        let providers = self.providers().clone();
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
        let active = self.active_page();
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
        let form = self.form();
        div()
            .id("persona-definition-scroll")
            .size_full()
            .overflow_y_scroll()
            .px_6()
            .pb_6()
            .child(div().pt_3().child(form_field(
                t!("persona.name").to_string(),
                Input::new(form.name),
                palette,
            )))
            .child(section_label(t!("llm.system_prompt").to_string(), palette))
            .child(
                div()
                    .w_full()
                    .h(px(220.0))
                    .child(Input::new(form.system_prompt).h_full()),
            )
            .child(section_label(
                t!("persona.input_prompt").to_string(),
                palette,
            ))
            .child(
                div()
                    .w_full()
                    .h(px(150.0))
                    .child(Input::new(form.input_prompt).h_full()),
            )
            .into_any_element()
    }

    fn render_context(&self, cx: &mut Context<Self>) -> AnyElement {
        let palette = UiPalette::from_app(cx);
        let form = self.form();
        let usage = self.usage();
        let usage_error = self.usage_error().map(str::to_owned);
        let clear_label = t!("persona.memory_clear").to_string();

        div()
            .id("persona-context-scroll")
            .debug_selector(|| "persona-context-page".to_owned())
            .size_full()
            .min_h_0()
            .flex()
            .flex_col()
            .overflow_hidden()
            .px_6()
            .pb_6()
            .child(
                div()
                    .debug_selector(|| "context-stats".to_owned())
                    .h(px(58.0))
                    .flex_shrink_0()
                    .flex()
                    .items_center()
                    .gap_4()
                    .border_b_1()
                    .border_color(palette.border)
                    .child(context_stat(
                        "context-stat-messages",
                        "context-limit-messages",
                        t!("persona.context_stat_messages").to_string(),
                        usage.map_or_else(
                            || "-".to_owned(),
                            |usage| usage.context.messages.to_string(),
                        ),
                        form.context_messages,
                        56.0,
                        palette,
                    ))
                    .child(context_stat(
                        "context-stat-tokens",
                        "context-limit-tokens",
                        t!("persona.context_stat_tokens").to_string(),
                        usage.map_or_else(
                            || "-".to_owned(),
                            |usage| usage.context.tokens.to_string(),
                        ),
                        form.context_tokens,
                        88.0,
                        palette,
                    ))
                    .child(
                        div()
                            .id("clear-context-memory")
                            .size(px(30.0))
                            .flex_shrink_0()
                            .flex()
                            .items_center()
                            .justify_center()
                            .rounded_md()
                            .text_color(palette.danger)
                            .cursor_pointer()
                            .hover(move |style| style.bg(palette.danger.opacity(0.12)))
                            .tooltip(move |window, cx| {
                                Tooltip::new(clear_label.clone()).build(window, cx)
                            })
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.request_clear_memory(MemoryScope::Context, cx);
                            }))
                            .child(
                                svg()
                                    .path("icons/trash-2.svg")
                                    .size_4()
                                    .text_color(palette.danger),
                            ),
                    ),
            )
            .when_some(usage_error, |this, error| {
                this.child(
                    div()
                        .pt_3()
                        .text_xs()
                        .text_color(palette.danger)
                        .child(error),
                )
            })
            .child(self.render_context_messages(palette, cx))
            .into_any_element()
    }

    fn render_context_messages(&self, palette: UiPalette, cx: &mut Context<Self>) -> AnyElement {
        let loading = self.context_loading();
        let error = self.context_error().map(str::to_owned);
        let empty = !loading && error.is_none() && self.context_editors().is_empty();
        let bounds = self.context_view_bounds();
        let selection_rect = self.context_selection_rect();
        let messages = if loading || error.is_some() {
            Vec::new()
        } else {
            self.context_editors()
                .iter()
                .map(|message| self.render_context_message(message, palette, cx))
                .collect::<Vec<_>>()
        };

        div()
            .relative()
            .id("context-message-scroll")
            .debug_selector(|| "context-message-scroll".to_owned())
            .w_full()
            .min_h_0()
            .flex_1()
            .overflow_y_scroll()
            .track_scroll(self.context_scroll())
            .rounded_md()
            .border_1()
            .border_color(palette.border)
            .bg(palette.sidebar.opacity(0.45))
            .mt_3()
            .p_3()
            .on_mouse_move(cx.listener(|this, event, _, cx| {
                this.update_context_selection_position(event, cx);
            }))
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|this, event, _, cx| {
                    this.finish_context_selection_drag(event, cx);
                }),
            )
            .on_mouse_up_out(
                MouseButton::Left,
                cx.listener(|this, event, _, cx| {
                    this.finish_context_selection_drag(event, cx);
                }),
            )
            .child(
                canvas(
                    move |canvas_bounds, _, _| bounds.set(canvas_bounds),
                    |_, _, _, _| {},
                )
                .absolute()
                .inset_0(),
            )
            .when(loading, |this| {
                this.child(
                    div()
                        .py_8()
                        .text_center()
                        .text_xs()
                        .text_color(palette.muted_foreground)
                        .child(t!("persona.context_loading").to_string()),
                )
            })
            .when_some(error, |this, error| {
                this.child(
                    div()
                        .py_6()
                        .text_center()
                        .text_xs()
                        .text_color(palette.danger)
                        .child(error),
                )
            })
            .when(empty, |this| {
                this.child(
                    div()
                        .py_8()
                        .text_center()
                        .text_xs()
                        .text_color(palette.muted_foreground)
                        .child(t!("persona.context_empty").to_string()),
                )
            })
            .children(messages)
            .when_some(selection_rect, |this, (start, current)| {
                let viewport = self.context_view_bounds().get();
                let (origin, size) = selection_geometry(start, current, viewport.origin);
                this.child(
                    div()
                        .debug_selector(|| "context-selection-box".to_owned())
                        .absolute()
                        .left(origin.x)
                        .top(origin.y)
                        .w(size.x)
                        .h(size.y)
                        .rounded_md()
                        .border_1()
                        .border_color(palette.primary)
                        .bg(palette.primary.opacity(0.10)),
                )
            })
            .into_any_element()
    }

    fn render_context_message(
        &self,
        message: &ContextMessageEditor,
        palette: UiPalette,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let message_id = message.id;
        let selected = self.context_message_selected(message_id);
        let editing = self.context_message_editing(message_id);
        let assistant = message.role == ChatRole::Assistant;
        let role_icon = if assistant {
            "icons/bot.svg"
        } else {
            "icons/user-round.svg"
        };
        let content = message.input.read(cx).value().to_string();
        let view = cx.entity().downgrade();
        let view_for_edit = view.clone();
        let view_for_delete = view.clone();
        let view_for_copy = view.clone();

        div()
            .id(("context-message", message_id))
            .debug_selector(move || format!("context-card-{message_id}"))
            .w_full()
            .min_h(px(48.0))
            .flex()
            .items_start()
            .px_2()
            .py_1()
            .rounded_md()
            .role(gpui::Role::ListItem)
            .aria_selected(selected)
            .bg(if selected {
                palette.accent.opacity(0.42)
            } else {
                palette.background.opacity(0.0)
            })
            .on_mouse_move(cx.listener(move |this, event, _, cx| {
                this.update_context_selection_drag(message_id, event, cx);
            }))
            .when(!editing, |this| {
                this.cursor_pointer().on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |this, event, _, cx| {
                        this.start_context_selection_drag(message_id, event, cx);
                    }),
                )
            })
            .when(!editing, |this| {
                this.on_mouse_down(
                    MouseButton::Right,
                    cx.listener(move |this, _, _, cx| {
                        this.prepare_context_menu_selection(message_id, cx);
                    }),
                )
            })
            .context_menu(move |menu, _window, cx| {
                if editing {
                    return menu;
                }
                let selected_count = view
                    .upgrade()
                    .map(|view| view.read(cx).selected_context_messages())
                    .unwrap_or(0);
                let edit_view = view_for_edit.clone();
                let delete_view = view_for_delete.clone();
                let copy_view = view_for_copy.clone();
                let edit_label = t!("persona.context_edit").to_string();
                let copy_label = t!("persona.context_copy").to_string();
                let delete_label = t!("persona.context_delete").to_string();

                menu.min_w(px(132.0))
                    .max_w(px(132.0))
                    .item(PopupMenuItem::element(move |_, cx| {
                        let palette = UiPalette::from_app(cx);
                        let edit_view = edit_view.clone();
                        let copy_view = copy_view.clone();
                        let delete_view = delete_view.clone();
                        div()
                            .debug_selector(|| "context-action-menu".to_owned())
                            .h(px(32.0))
                            .flex()
                            .items_center()
                            .justify_center()
                            .gap_1()
                            .child(context_menu_action(
                                "context-action-edit",
                                "icons/pencil.svg",
                                edit_label.clone(),
                                selected_count == 1,
                                false,
                                palette,
                                move |_, window, cx| {
                                    let _ = edit_view.update(cx, |view, cx| {
                                        view.begin_context_message_edit(message_id, window, cx);
                                    });
                                },
                            ))
                            .child(context_menu_action(
                                "context-action-copy",
                                "icons/copy.svg",
                                copy_label.clone(),
                                true,
                                false,
                                palette,
                                move |_, _, cx| {
                                    let _ = copy_view.update(cx, |view, cx| {
                                        view.copy_selected_context_messages(cx);
                                    });
                                },
                            ))
                            .child(context_menu_action(
                                "context-action-delete",
                                "icons/trash-2.svg",
                                delete_label.clone(),
                                true,
                                true,
                                palette,
                                move |_, _, cx| {
                                    let _ = delete_view.update(cx, |view, cx| {
                                        view.request_delete_selected_context_messages(cx);
                                    });
                                },
                            ))
                    }))
            })
            .child(
                div()
                    .w_full()
                    .min_w_0()
                    .flex_1()
                    .flex()
                    .when(!assistant, |this| this.justify_end())
                    .child(
                        div()
                            .max_w_full()
                            .flex()
                            .items_start()
                            .gap_2()
                            .when(assistant, |this| {
                                this.child(message_role_icon(role_icon, palette))
                            })
                            .child(self.render_message_bubble(
                                message, content, editing, assistant, palette,
                            ))
                            .when(!assistant, |this| {
                                this.child(message_role_icon(role_icon, palette))
                            }),
                    ),
            )
            .into_any_element()
    }

    fn render_message_bubble(
        &self,
        message: &ContextMessageEditor,
        content: String,
        editing: bool,
        assistant: bool,
        palette: UiPalette,
    ) -> AnyElement {
        let bubble = div()
            .min_w(px(64.0))
            .max_w(px(520.0))
            .overflow_hidden()
            .rounded_md()
            .border_1()
            .border_color(if assistant {
                palette.border
            } else {
                palette.primary.opacity(0.48)
            })
            .bg(if assistant {
                palette.background
            } else {
                palette.primary.opacity(0.12)
            });
        if editing {
            bubble
                .child(
                    Input::new(&message.input)
                        .appearance(false)
                        .focus_bordered(false),
                )
                .into_any_element()
        } else {
            bubble
                .child(
                    div()
                        .px_3()
                        .py_2()
                        .text_sm()
                        .line_height(px(20.0))
                        .whitespace_normal()
                        .child(content),
                )
                .into_any_element()
        }
    }

    fn render_persona_settings(&self, cx: &mut Context<Self>) -> AnyElement {
        let palette = UiPalette::from_app(cx);
        let form = self.form();
        div()
            .id("persona-settings-scroll")
            .size_full()
            .overflow_y_scroll()
            .px_6()
            .pb_6()
            .child(
                div().pt_3().child(form_field(
                    t!("persona.conversation_model").to_string(),
                    Select::new(form.provider)
                        .search_placeholder(t!("llm.search_provider").to_string()),
                    palette,
                )),
            )
            .child(form_field(
                t!("persona.live2d_model").to_string(),
                Select::new(form.live2d)
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
        let has_persona = self.editing_index().is_some();
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
                    .child(div().min_h_0().flex_1().child(match self.active_page() {
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

fn message_role_icon(icon: &'static str, palette: UiPalette) -> AnyElement {
    div()
        .size(px(MESSAGE_ICON_SIZE))
        .flex_shrink_0()
        .flex()
        .items_center()
        .justify_center()
        .rounded_full()
        .bg(palette.muted)
        .child(svg().path(icon).size_4().text_color(palette.foreground))
        .into_any_element()
}

fn context_menu_action(
    selector: &'static str,
    icon: &'static str,
    label: String,
    enabled: bool,
    danger: bool,
    palette: UiPalette,
    on_click: impl Fn(&gpui::ClickEvent, &mut Window, &mut gpui::App) + 'static,
) -> AnyElement {
    let aria_label = label.clone();
    let color = if !enabled {
        palette.muted_foreground
    } else if danger {
        palette.danger
    } else {
        palette.foreground
    };
    div()
        .id(selector)
        .debug_selector(move || selector.to_owned())
        .size(px(30.0))
        .flex_shrink_0()
        .flex()
        .items_center()
        .justify_center()
        .rounded_md()
        .role(gpui::Role::Button)
        .aria_label(aria_label)
        .tooltip(move |window, cx| Tooltip::new(label.clone()).build(window, cx))
        .when(enabled, |this| {
            this.cursor_pointer()
                .hover(move |style| {
                    style.bg(if danger {
                        palette.danger.opacity(0.12)
                    } else {
                        palette.secondary
                    })
                })
                .on_click(on_click)
        })
        .child(svg().path(icon).size_4().text_color(color))
        .into_any_element()
}

fn context_stat(
    stat_selector: &'static str,
    input_selector: &'static str,
    label: String,
    current: String,
    input: &Entity<gpui_component::input::InputState>,
    input_width: f32,
    palette: UiPalette,
) -> AnyElement {
    div()
        .debug_selector(move || stat_selector.to_owned())
        .min_w_0()
        .flex_1()
        .flex()
        .flex_col()
        .gap(px(2.0))
        .child(
            div()
                .text_xs()
                .text_color(palette.muted_foreground)
                .child(label),
        )
        .child(
            div()
                .h(px(28.0))
                .flex()
                .items_center()
                .gap_1()
                .child(div().text_sm().font_semibold().child(current))
                .child(
                    div()
                        .text_sm()
                        .text_color(palette.muted_foreground)
                        .child("/"),
                )
                .child(
                    div()
                        .debug_selector(move || input_selector.to_owned())
                        .w(px(input_width))
                        .child(Input::new(input).small()),
                ),
        )
        .into_any_element()
}

fn selection_geometry(
    start: Point<Pixels>,
    current: Point<Pixels>,
    viewport_origin: Point<Pixels>,
) -> (Point<Pixels>, Point<Pixels>) {
    let left = if start.x <= current.x {
        start.x
    } else {
        current.x
    };
    let top = if start.y <= current.y {
        start.y
    } else {
        current.y
    };
    let right = if start.x >= current.x {
        start.x
    } else {
        current.x
    };
    let bottom = if start.y >= current.y {
        start.y
    } else {
        current.y
    };
    (
        Point::new(left - viewport_origin.x, top - viewport_origin.y),
        Point::new(right - left, bottom - top),
    )
}

impl Render for PersonaSettingsView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let palette = UiPalette::from_app(cx);
        let status = self.status().map(str::to_owned);
        let confirm = self.confirm_prompt();
        div()
            .relative()
            .size_full()
            .min_w_0()
            .flex()
            .text_color(palette.foreground)
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
