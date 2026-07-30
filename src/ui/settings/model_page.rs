//! 渲染模型目录、资源重命名、表达式分类以及预览控件。

use std::path::Path;

use gpui::{
    AnyElement, Context, IntoElement, Pixels, Point, Render, Window, div, prelude::*, px, svg,
};
use gpui_component::{Sizable, StyledExt, input::Input, tooltip::Tooltip};
use rust_i18n::t;

use crate::{
    config::{ModelExpressionCategory, ModelResourceKey, ModelResourceKind},
    model::{ModelFamily, ModelPreviewExpression, ModelPreviewResource, ModelVariant},
    ui::UiPalette,
};

use super::{
    ModelExpressionDrag, SettingsView,
    components::{control_section, empty_control_text, page_header},
};

struct ExpressionDragPreview {
    label: String,
    position: Point<Pixels>,
}

impl Render for ExpressionDragPreview {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let palette = UiPalette::from_app(cx);
        div().pl(self.position.x).pt(self.position.y).child(
            div()
                .max_w(px(220.0))
                .rounded_md()
                .border_1()
                .border_color(palette.border)
                .bg(palette.popover.opacity(0.92))
                .px_3()
                .py_2()
                .text_sm()
                .overflow_hidden()
                .text_ellipsis()
                .shadow_lg()
                .child(self.label.clone()),
        )
    }
}

impl SettingsView {
    fn global_selected_family(&self) -> Option<&ModelFamily> {
        let selected = self.global_model_selection.as_deref()?;
        self.catalog
            .families()
            .iter()
            .find(|family| family.contains(selected))
    }

    pub(super) fn render_model_page(&self, cx: &mut Context<Self>) -> AnyElement {
        let palette = UiPalette::from_app(cx);
        let selected_path = self.global_model_selection.as_deref();
        let runtime_path = self.catalog.selected_relative_path();
        let selected_family = self.global_selected_family();
        let refresh_label = if self.is_refreshing {
            t!("model.scanning").to_string()
        } else {
            t!("model.rescan").to_string()
        };
        let open_folder_label = t!("model.open_folder").to_string();

        div()
            .size_full()
            .min_w_0()
            .flex()
            .flex_col()
            .child(
                page_header(t!("settings.model_title").to_string(), palette).child(
                    div()
                        .flex()
                        .items_center()
                        .gap_2()
                        .child(
                            div()
                                .id("open-model-directory")
                                .size(px(28.0))
                                .flex_shrink_0()
                                .flex()
                                .items_center()
                                .justify_center()
                                .rounded_md()
                                .bg(palette.secondary)
                                .cursor_pointer()
                                .hover(move |style| style.bg(palette.accent))
                                .on_click(
                                    cx.listener(|this, _, _, cx| this.open_model_directory(cx)),
                                )
                                .tooltip(move |window, cx| {
                                    Tooltip::new(open_folder_label.clone()).build(window, cx)
                                })
                                .child(
                                    svg()
                                        .path("icons/folder-open.svg")
                                        .size_4()
                                        .text_color(palette.primary),
                                ),
                        )
                        .child(
                            div()
                                .id("refresh-models")
                                .flex()
                                .items_center()
                                .gap_2()
                                .rounded_md()
                                .px_3()
                                .py_1()
                                .text_xs()
                                .bg(palette.secondary)
                                .text_color(palette.secondary_foreground)
                                .cursor_pointer()
                                .hover(move |style| style.bg(palette.accent))
                                .on_click(cx.listener(|this, _, window, cx| {
                                    this.refresh_models(window, cx);
                                }))
                                .child(
                                    svg()
                                        .path("icons/refresh-cw.svg")
                                        .size_4()
                                        .text_color(palette.primary),
                                )
                                .child(refresh_label),
                        ),
                ),
            )
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .flex()
                    .child(self.render_model_list(selected_path, runtime_path, cx))
                    .child(self.render_model_controls(selected_family, selected_path, cx)),
            )
            .into_any_element()
    }

    fn render_model_list(
        &self,
        selected_path: Option<&Path>,
        runtime_path: Option<&Path>,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let palette = UiPalette::from_app(cx);
        let families = self.catalog.families();
        div()
            .w(px(200.0))
            .h_full()
            .flex_shrink_0()
            .flex()
            .flex_col()
            .border_r_1()
            .border_color(palette.border)
            .child(
                div()
                    .h(px(38.0))
                    .flex()
                    .items_center()
                    .px_4()
                    .text_xs()
                    .text_color(palette.muted_foreground)
                    .child(t!("model.count", count = families.len()).to_string()),
            )
            .child(
                div()
                    .id("model-family-list")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .px_2()
                    .pb_2()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .when(families.is_empty(), |this| {
                        this.child(
                            div()
                                .px_3()
                                .py_4()
                                .text_sm()
                                .text_color(palette.muted_foreground)
                                .child(t!("model.none").to_string()),
                        )
                    })
                    .children(families.iter().enumerate().map(|(index, family)| {
                        let selected = selected_path.is_some_and(|path| family.contains(path));
                        let expression_outfits =
                            if runtime_path.is_some_and(|path| family.contains(path)) {
                                self.preview_capabilities
                                    .expressions()
                                    .iter()
                                    .filter(|expression| {
                                        self.expression_category(expression)
                                            == ModelExpressionCategory::Outfit
                                    })
                                    .count()
                            } else {
                                0
                            };
                        let outfit_count = family.outfit_count().saturating_add(expression_outfits);
                        div()
                            .id(("model-family", index))
                            .rounded_md()
                            .px_3()
                            .py_2()
                            .cursor_pointer()
                            .bg(if selected {
                                palette.accent
                            } else {
                                palette.background
                            })
                            .hover(move |style| style.bg(palette.secondary))
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.select_family(index, cx);
                            }))
                            .child(
                                div()
                                    .text_sm()
                                    .font_medium()
                                    .overflow_hidden()
                                    .text_ellipsis()
                                    .child(family.display_name().to_owned()),
                            )
                            .child(
                                div()
                                    .mt_1()
                                    .text_xs()
                                    .text_color(if selected {
                                        palette.accent_foreground
                                    } else {
                                        palette.muted_foreground
                                    })
                                    .child(
                                        t!("model.outfit_count", count = outfit_count).to_string(),
                                    ),
                            )
                    })),
            )
            .into_any_element()
    }

    fn render_model_controls(
        &self,
        selected_family: Option<&ModelFamily>,
        selected_path: Option<&Path>,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let palette = UiPalette::from_app(cx);
        let variants = selected_family
            .map(ModelFamily::variants)
            .unwrap_or_default();
        let default_outfit = variants.len() == 1;
        let outfit_expressions = self
            .preview_capabilities
            .expressions()
            .iter()
            .filter(|expression| {
                self.expression_category(expression) == ModelExpressionCategory::Outfit
            })
            .collect::<Vec<_>>();
        let expressions = self
            .preview_capabilities
            .expressions()
            .iter()
            .filter(|expression| {
                self.expression_category(expression) == ModelExpressionCategory::Expression
            })
            .collect::<Vec<_>>();

        div()
            .id("model-controls")
            .flex_1()
            .min_w_0()
            .h_full()
            .flex_shrink_0()
            .overflow_y_scroll()
            .child(
                control_section(t!("model.outfits").to_string(), palette)
                    .id("outfit-drop-target")
                    .drag_over::<ModelExpressionDrag>(move |style, _, _, _| {
                        style.bg(palette.accent.opacity(0.24))
                    })
                    .can_drop(|value, _, _| value.is::<ModelExpressionDrag>())
                    .on_drop::<ModelExpressionDrag>(cx.listener(|this, drag, _, cx| {
                        this.move_expression_to_category(drag, ModelExpressionCategory::Outfit, cx);
                    }))
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap_1()
                            .when(
                                variants.is_empty() && outfit_expressions.is_empty(),
                                |this| {
                                    this.child(empty_control_text(
                                        t!("model.no_outfits").to_string(),
                                        palette,
                                    ))
                                },
                            )
                            .children(variants.iter().enumerate().map(|(index, variant)| {
                                self.render_variant_row(
                                    variant,
                                    index,
                                    default_outfit,
                                    selected_path,
                                    palette,
                                    cx,
                                )
                            }))
                            .children(outfit_expressions.iter().enumerate().filter_map(
                                |(index, expression)| {
                                    self.render_expression_row(expression, index, true, palette, cx)
                                },
                            )),
                    ),
            )
            .child(
                control_section(t!("model.idle_motions").to_string(), palette).child(
                    div()
                        .flex()
                        .flex_col()
                        .gap_1()
                        .when(
                            self.preview_capabilities.idle_motions().is_empty(),
                            |this| {
                                this.child(empty_control_text(
                                    t!("model.no_idle_motions").to_string(),
                                    palette,
                                ))
                            },
                        )
                        .children(
                            self.preview_capabilities
                                .idle_motions()
                                .iter()
                                .enumerate()
                                .filter_map(|(index, motion)| {
                                    self.render_motion_row(motion, index, true, palette, cx)
                                }),
                        ),
                ),
            )
            .child(
                div().flex().min_h_0().children([
                    control_section(t!("model.action_preview").to_string(), palette)
                        .flex_1()
                        .min_w_0()
                        .child(
                            div()
                                .flex()
                                .flex_col()
                                .gap_1()
                                .when(self.preview_capabilities.motions().is_empty(), |this| {
                                    this.child(empty_control_text(
                                        t!("model.no_actions").to_string(),
                                        palette,
                                    ))
                                })
                                .children(
                                    self.preview_capabilities
                                        .motions()
                                        .iter()
                                        .enumerate()
                                        .filter_map(|(index, motion)| {
                                            self.render_motion_row(
                                                motion, index, false, palette, cx,
                                            )
                                        }),
                                ),
                        )
                        .into_any_element(),
                    control_section(t!("model.expression_preview").to_string(), palette)
                        .id("expression-drop-target")
                        .flex_1()
                        .min_w_0()
                        .drag_over::<ModelExpressionDrag>(move |style, _, _, _| {
                            style.bg(palette.accent.opacity(0.24))
                        })
                        .can_drop(|value, _, _| value.is::<ModelExpressionDrag>())
                        .on_drop::<ModelExpressionDrag>(cx.listener(|this, drag, _, cx| {
                            this.move_expression_to_category(
                                drag,
                                ModelExpressionCategory::Expression,
                                cx,
                            );
                        }))
                        .child(
                            div()
                                .flex()
                                .flex_col()
                                .gap_1()
                                .when(expressions.is_empty(), |this| {
                                    this.child(empty_control_text(
                                        t!("model.no_expressions").to_string(),
                                        palette,
                                    ))
                                })
                                .children(expressions.iter().enumerate().filter_map(
                                    |(index, expression)| {
                                        self.render_expression_row(
                                            expression, index, false, palette, cx,
                                        )
                                    },
                                )),
                        )
                        .into_any_element(),
                ]),
            )
            .into_any_element()
    }

    fn render_variant_row(
        &self,
        variant: &ModelVariant,
        index: usize,
        default_outfit: bool,
        selected_path: Option<&Path>,
        palette: UiPalette,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let relative_path = variant.relative_path().to_path_buf();
        let selected = selected_path == Some(relative_path.as_path())
            && (self.catalog.selected_relative_path() != selected_path
                || self.active_outfit.is_none());
        let default_name = if default_outfit {
            t!("model.default_outfit").to_string()
        } else {
            variant.display_name().to_owned()
        };
        let key = Self::variant_resource_key(&relative_path);
        let display_name = self.model_resource_name(&key, &default_name);

        self.resource_row(("outfit", index), selected, palette)
            .cursor_pointer()
            .on_click(cx.listener(move |this, _, _, cx| {
                this.select_variant(relative_path.clone(), cx);
            }))
            .child(self.render_resource_name(
                key,
                default_name,
                display_name,
                format!("variant-{index}"),
                palette,
                cx,
            ))
            .child(
                svg()
                    .path(if selected {
                        "icons/check.svg"
                    } else {
                        "icons/square.svg"
                    })
                    .size_4()
                    .flex_shrink_0()
                    .text_color(palette.primary),
            )
            .into_any_element()
    }

    /// 返回模型页按全局选择展示的清单变体，供状态隔离回归测试使用。
    #[cfg(test)]
    pub(in crate::ui) fn global_model_variants_for_test(&self) -> Vec<std::path::PathBuf> {
        self.global_selected_family()
            .map(|family| {
                family
                    .variants()
                    .iter()
                    .map(|variant| variant.relative_path().to_path_buf())
                    .collect()
            })
            .unwrap_or_default()
    }

    fn render_motion_row(
        &self,
        motion: &ModelPreviewResource,
        index: usize,
        idle: bool,
        palette: UiPalette,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let key = self.selected_resource_key(ModelResourceKind::Motion, motion.runtime_id())?;
        let default_name = motion.default_name().to_owned();
        let display_name = self.model_resource_name(&key, &default_name);
        let requested_id = motion.runtime_id().to_owned();
        let requested_name = display_name.clone();
        let play_label = t!("model.play_motion").to_string();
        let row_id = if idle {
            ("idle-motion", index)
        } else {
            ("motion", index)
        };
        let name_id = if idle {
            format!("idle-motion-{index}")
        } else {
            format!("motion-{index}")
        };
        let play_id = if idle {
            ("play-idle-motion", index)
        } else {
            ("play-motion", index)
        };

        Some(
            self.resource_row(row_id, false, palette)
                .child(self.render_resource_name(
                    key,
                    default_name,
                    display_name,
                    name_id,
                    palette,
                    cx,
                ))
                .child(
                    self.icon_button(play_id, "icons/play.svg", palette)
                        .tooltip(move |window, cx| {
                            Tooltip::new(play_label.clone()).build(window, cx)
                        })
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.preview_motion(requested_id.clone(), requested_name.clone(), cx);
                        })),
                )
                .into_any_element(),
        )
    }

    fn render_expression_row(
        &self,
        expression: &ModelPreviewExpression,
        index: usize,
        outfit: bool,
        palette: UiPalette,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let resource = expression.resource();
        let key =
            self.selected_resource_key(ModelResourceKind::Expression, resource.runtime_id())?;
        let default_name = resource.default_name().to_owned();
        let display_name = self.model_resource_name(&key, &default_name);
        let requested_id = resource.runtime_id().to_owned();
        let requested_name = display_name.clone();
        let selected = outfit && self.active_outfit.as_deref() == Some(resource.runtime_id());
        let preview_label = if outfit {
            t!("model.apply_outfit").to_string()
        } else {
            t!("model.preview_expression").to_string()
        };
        let drag = self.expression_drag(expression);

        Some(
            self.resource_row(
                if outfit {
                    ("expression-outfit", index)
                } else {
                    ("expression", index)
                },
                selected,
                palette,
            )
            .when_some(drag, |this, drag| {
                let preview_name = display_name.clone();
                let drag_label = t!("model.move_expression").to_string();
                this.child(
                    self.icon_button(
                        if outfit {
                            ("move-outfit", index)
                        } else {
                            ("move-expression", index)
                        },
                        "icons/move.svg",
                        palette,
                    )
                    .cursor_move()
                    .tooltip(move |window, cx| Tooltip::new(drag_label.clone()).build(window, cx))
                    .on_drag(drag, move |_, position, _, cx| {
                        let label = preview_name.clone();
                        cx.new(|_| ExpressionDragPreview { label, position })
                    }),
                )
            })
            .child(self.render_resource_name(
                key,
                default_name,
                display_name,
                if outfit {
                    format!("expression-outfit-{index}")
                } else {
                    format!("expression-{index}")
                },
                palette,
                cx,
            ))
            .child(
                self.icon_button(
                    if outfit {
                        ("apply-expression-outfit", index)
                    } else {
                        ("preview-expression", index)
                    },
                    "icons/play.svg",
                    palette,
                )
                .tooltip(move |window, cx| Tooltip::new(preview_label.clone()).build(window, cx))
                .on_click(cx.listener(move |this, _, _, cx| {
                    if outfit {
                        this.preview_outfit(requested_id.clone(), requested_name.clone(), cx);
                    } else {
                        this.preview_expression(requested_id.clone(), requested_name.clone(), cx);
                    }
                })),
            )
            .into_any_element(),
        )
    }

    fn render_resource_name(
        &self,
        key: ModelResourceKey,
        default_name: String,
        display_name: String,
        id_token: String,
        palette: UiPalette,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let editing = self
            .editing_model_resource
            .as_ref()
            .is_some_and(|editing| editing.key == key);
        if editing && let Some(input) = self.model_resource_name_input.clone() {
            let input_for_commit = input.clone();
            let save_label = t!("model.save_name").to_string();
            return div()
                .min_w_0()
                .flex_1()
                .flex()
                .items_center()
                .gap_1()
                .child(div().min_w_0().flex_1().child(Input::new(&input).small()))
                .child(
                    self.icon_button(
                        format!("save-model-resource-name:{id_token}"),
                        "icons/check.svg",
                        palette,
                    )
                    .tooltip(move |window, cx| Tooltip::new(save_label.clone()).build(window, cx))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        cx.stop_propagation();
                        this.commit_model_resource_name(&input_for_commit, cx);
                    })),
                )
                .into_any_element();
        }

        let renamed = self.model_resource_is_renamed(&key);
        let key_for_edit = key.clone();
        let default_for_edit = default_name.clone();
        let current_for_edit = display_name.clone();
        let edit_label = t!("model.rename").to_string();
        let reset_label = t!("model.restore_name").to_string();
        div()
            .min_w_0()
            .flex_1()
            .flex()
            .items_center()
            .gap_1()
            .child(
                div()
                    .min_w_0()
                    .flex_1()
                    .overflow_hidden()
                    .text_ellipsis()
                    .child(display_name),
            )
            .when(renamed, |this| {
                let key = key.clone();
                this.child(
                    self.icon_button(
                        format!("restore-model-resource-name:{id_token}"),
                        "icons/x.svg",
                        palette,
                    )
                    .tooltip(move |window, cx| Tooltip::new(reset_label.clone()).build(window, cx))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        cx.stop_propagation();
                        this.reset_model_resource_name(key.clone(), cx);
                    })),
                )
            })
            .child(
                self.icon_button(
                    format!("rename-model-resource:{id_token}"),
                    "icons/pencil.svg",
                    palette,
                )
                .tooltip(move |window, cx| Tooltip::new(edit_label.clone()).build(window, cx))
                .on_click(cx.listener(move |this, _, window, cx| {
                    cx.stop_propagation();
                    this.begin_model_resource_rename(
                        key_for_edit.clone(),
                        default_for_edit.clone(),
                        current_for_edit.clone(),
                        window,
                        cx,
                    );
                })),
            )
            .into_any_element()
    }

    fn resource_row(
        &self,
        id: impl Into<gpui::ElementId>,
        selected: bool,
        palette: UiPalette,
    ) -> gpui::Stateful<gpui::Div> {
        div()
            .id(id)
            .h(px(38.0))
            .min_w_0()
            .flex()
            .items_center()
            .gap_1()
            .rounded_md()
            .border_1()
            .border_color(if selected {
                palette.primary
            } else {
                palette.border
            })
            .bg(if selected {
                palette.accent
            } else {
                palette.background
            })
            .px_2()
            .text_sm()
            .hover(move |style| style.bg(palette.secondary))
    }

    fn icon_button(
        &self,
        id: impl Into<gpui::ElementId>,
        icon: &'static str,
        palette: UiPalette,
    ) -> gpui::Stateful<gpui::Div> {
        div()
            .id(id)
            .size(px(26.0))
            .flex_shrink_0()
            .flex()
            .items_center()
            .justify_center()
            .rounded_md()
            .cursor_pointer()
            .hover(move |style| style.bg(palette.accent))
            .child(svg().path(icon).size_4().text_color(palette.primary))
    }
}
