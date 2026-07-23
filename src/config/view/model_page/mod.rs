//! 渲染模型目录、服装选择以及动作和表情预览控件。

use std::path::Path;

use gpui::{AnyElement, Context, IntoElement, div, prelude::*, px, svg};
use gpui_component::StyledExt;
use rust_i18n::t;

use crate::theme::UiPalette;

use super::{
    ConfigView,
    components::{control_section, empty_control_text, option_button, page_header},
};
use crate::config::model_catalog::ModelFamily;

impl ConfigView {
    pub(super) fn render_model_page(&self, cx: &mut Context<Self>) -> AnyElement {
        let palette = UiPalette::from_app(cx);
        let selected_path = self.catalog.selected_relative_path();
        let refresh_label = if self.is_refreshing {
            t!("model.scanning").to_string()
        } else {
            t!("model.rescan").to_string()
        };

        div()
            .size_full()
            .min_w_0()
            .flex()
            .flex_col()
            .child(
                page_header(t!("settings.model_title").to_string(), palette).child(
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
                        .on_click(cx.listener(|this, _, _, cx| this.refresh_models(cx)))
                        .child(
                            svg()
                                .path("icons/refresh-cw.svg")
                                .size_4()
                                .text_color(palette.primary),
                        )
                        .child(refresh_label),
                ),
            )
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .flex()
                    .child(self.render_model_list(selected_path, cx))
                    .child(self.render_model_controls(
                        self.catalog.selected_family(),
                        selected_path,
                        cx,
                    )),
            )
            .into_any_element()
    }

    fn render_model_list(
        &self,
        selected_path: Option<&Path>,
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
                        let outfit_count = family.outfit_count();
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
        let outfits = self.preview_capabilities.outfits();
        let motions = self.preview_capabilities.motions();
        let variants = selected_family
            .map(ModelFamily::variants)
            .unwrap_or_default();
        let default_outfit = variants.len() == 1;
        let outfit_presets = selected_family
            .map(ModelFamily::outfits)
            .unwrap_or_default()
            .iter()
            .filter(|outfit| {
                outfits.is_empty() || outfits.iter().any(|name| name == outfit.expression_name())
            });
        let has_outfit_presets = outfit_presets.clone().next().is_some();
        let expressions = self
            .preview_capabilities
            .expressions()
            .iter()
            .filter(|name| !outfits.contains(name));
        let has_expressions = expressions.clone().next().is_some();
        div()
            .id("model-controls")
            .flex_1()
            .min_w_0()
            .h_full()
            .flex_shrink_0()
            .overflow_y_scroll()
            .child(
                control_section(t!("model.outfits").to_string(), palette).child(
                    div()
                        .flex()
                        .flex_wrap()
                        .gap_2()
                        .when(variants.is_empty() && !has_outfit_presets, |this| {
                            this.child(empty_control_text(
                                t!("model.no_outfits").to_string(),
                                palette,
                            ))
                        })
                        .children(variants.iter().enumerate().map(|(index, variant)| {
                            let relative_path = variant.relative_path().to_path_buf();
                            let selected = self.active_outfit.is_none()
                                && selected_path == Some(relative_path.as_path());
                            let label = if default_outfit {
                                t!("model.default_outfit").to_string()
                            } else {
                                variant.display_name().to_owned()
                            };
                            option_button(("outfit", index), label, selected, palette).on_click(
                                cx.listener(move |this, _, _, cx| {
                                    this.select_variant(relative_path.clone(), cx);
                                }),
                            )
                        }))
                        .children(outfit_presets.enumerate().map(|(index, outfit)| {
                            let name = outfit.expression_name().to_owned();
                            let selected = self.active_outfit.as_deref() == Some(name.as_str());
                            option_button(
                                ("external-outfit", index),
                                outfit.display_name().to_owned(),
                                selected,
                                palette,
                            )
                            .on_click(cx.listener(
                                move |this, _, _, cx| {
                                    this.preview_outfit(name.clone(), cx);
                                },
                            ))
                        })),
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
                                .when(motions.is_empty(), |this| {
                                    this.child(empty_control_text(
                                        t!("model.no_actions").to_string(),
                                        palette,
                                    ))
                                })
                                .children(motions.iter().enumerate().map(|(index, motion)| {
                                    let requested_motion = motion.clone();
                                    div()
                                        .id(("motion", index))
                                        .h(px(34.0))
                                        .flex()
                                        .items_center()
                                        .justify_between()
                                        .rounded_md()
                                        .px_2()
                                        .text_sm()
                                        .cursor_pointer()
                                        .hover(move |style| style.bg(palette.accent))
                                        .on_click(cx.listener(move |this, _, _, cx| {
                                            this.preview_motion(requested_motion.clone(), cx);
                                        }))
                                        .child(
                                            div()
                                                .min_w_0()
                                                .overflow_hidden()
                                                .text_ellipsis()
                                                .child(motion.clone()),
                                        )
                                        .child(
                                            svg()
                                                .path("icons/play.svg")
                                                .size_4()
                                                .text_color(palette.primary),
                                        )
                                })),
                        ),
                    control_section(t!("model.expression_preview").to_string(), palette)
                        .flex_1()
                        .min_w_0()
                        .child(
                            div()
                                .flex()
                                .flex_wrap()
                                .gap_2()
                                .when(!has_expressions, |this| {
                                    this.child(empty_control_text(
                                        t!("model.no_expressions").to_string(),
                                        palette,
                                    ))
                                })
                                .children(expressions.enumerate().map(|(index, expression)| {
                                    let requested_expression = expression.clone();
                                    option_button(
                                        ("expression", index),
                                        expression.clone(),
                                        false,
                                        palette,
                                    )
                                    .on_click(cx.listener(
                                        move |this, _, _, cx| {
                                            this.preview_expression(
                                                requested_expression.clone(),
                                                cx,
                                            );
                                        },
                                    ))
                                })),
                        ),
                ]),
            )
            .into_any_element()
    }
}
