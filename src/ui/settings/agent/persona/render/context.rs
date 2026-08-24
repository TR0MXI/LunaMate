//! 渲染上下文统计、消息气泡、右键操作和框选区域。

use gpui::{
    AnyElement, Context, Entity, MouseButton, Pixels, Point, Window, canvas, div, prelude::*, px,
    svg,
};
use gpui_component::{
    Sizable as _, StyledExt as _,
    input::{Input, InputState, Textarea},
    menu::{ContextMenuExt as _, PopupMenuItem},
    tooltip::Tooltip,
};
use lunamate_agent::ChatRole;
use rust_i18n::t;

use crate::ui::UiPalette;

use super::super::memory::MemoryScope;
use super::super::{ContextMessageEditor, ContextMessageLayout, PersonaSettingsView};

const MESSAGE_ICON_SIZE: f32 = 28.0;

impl PersonaSettingsView {
    pub(super) fn render_context(&self, cx: &mut Context<Self>) -> AnyElement {
        let palette = UiPalette::from_app(cx);
        let usage = self.usage;
        let usage_error = self.usage_error.clone();
        let selected_count = self.context_selected.len();
        let clear_label = t!("persona.context_delete_all").to_string();
        let delete_selected_label =
            t!("persona.context_delete_selected", count = selected_count).to_string();

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
                        &self.context_messages_input,
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
                        &self.context_tokens_input,
                        88.0,
                        palette,
                    ))
                    .when(selected_count > 0, |this| {
                        this.child(
                            div()
                                .id("delete-selected-context-messages")
                                .debug_selector(|| "delete-selected-context-messages".to_owned())
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
                                    Tooltip::new(delete_selected_label.clone()).build(window, cx)
                                })
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.request_delete_selected_context_messages(cx);
                                }))
                                .child(
                                    svg()
                                        .path("icons/trash-2.svg")
                                        .size_4()
                                        .text_color(palette.danger),
                                ),
                        )
                    })
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
        let loading = self.context_loading;
        let error = self.context_error.clone();
        let empty = !loading && error.is_none() && self.context_editors.is_empty();
        let bounds = self.context_view_bounds.clone();
        let scroll_offset = self.context_scroll.offset();
        let selection_rect = self
            .context_selection_drag
            .as_ref()
            .filter(|drag| drag.moved)
            .map(|drag| (drag.start + scroll_offset, drag.current + scroll_offset));
        let messages = if loading || error.is_some() {
            Vec::new()
        } else {
            self.context_editors
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
            .overflow_hidden()
            .rounded_md()
            .border_1()
            .border_color(palette.border)
            .bg(palette.sidebar.opacity(0.45))
            .mt_3()
            .track_focus(&self.context_focus)
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, event, window, cx| {
                    this.start_context_selection_drag(event, window, cx);
                    cx.stop_propagation();
                }),
            )
            .on_mouse_move(cx.listener(|this, event, window, cx| {
                this.update_context_selection_position(event, window, cx);
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
            .child(
                div()
                    .id("context-message-content")
                    .absolute()
                    .inset_0()
                    .overflow_y_scroll()
                    .track_scroll(&self.context_scroll)
                    .p_3()
                    .on_scroll_wheel(cx.listener(|this, _, window, cx| {
                        this.context_selection_scrolled(window, cx);
                    }))
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
                    .children(messages),
            )
            .when_some(selection_rect, |this, (start, current)| {
                let viewport = self.context_view_bounds.get();
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
        let selected = self.context_selected.contains(&message_id);
        let editing = self.context_editing == Some(message_id);
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
        let message_layout = message.layout.clone();
        let context_scroll = self.context_scroll.clone();

        div()
            .id(("context-message", message_id))
            .debug_selector(move || format!("context-card-{message_id}"))
            .relative()
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
            .when(!editing, |this| this.cursor_pointer())
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
                    .map(|view| view.read(cx).context_selected.len())
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
                canvas(
                    move |bounds, _, _| {
                        message_layout
                            .set(ContextMessageLayout::new(bounds, context_scroll.offset()));
                    },
                    |_, _, _, _| {},
                )
                .absolute()
                .inset_0(),
            )
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
            .debug_selector(move || format!("context-bubble-{}", message.id))
            .min_w(px(64.0))
            .max_w(px(520.0))
            .when(editing, |this| this.w(px(520.0)).max_w_full())
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
                    div()
                        .w_full()
                        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                        .child(
                            Textarea::new(&message.input)
                                .w_full()
                                .appearance(false)
                                .bordered(false),
                        ),
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
    input: &Entity<InputState>,
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
                        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
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
