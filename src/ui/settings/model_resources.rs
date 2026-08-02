//! 管理模型资源显示名、表达式分类、预览命令与 Agent 换装语义。

use std::{collections::HashSet, path::Path, sync::Arc};

use gpui::{Context, Entity};
use gpui_component::input::InputState;
use lunamate_agent::tools::OutfitOption;
use rust_i18n::t;

use crate::{
    config::{
        CONFIG, ModelExpressionCategory, ModelResourceKey, ModelResourceKind, ModelResourceSettings,
    },
    model::{ModelManifest, ModelPreviewCapabilities, ModelPreviewExpression},
};

use super::{
    AgentOutfitAction, AgentOutfitCandidate, AgentOutfitTarget, EditingModelResource,
    ModelExpressionDrag, SettingsEvent, SettingsView,
};

impl SettingsView {
    /// 接收主模型 generation 的能力快照，供设置窗口显示可用控制项。
    pub(crate) fn set_preview_capabilities(
        &mut self,
        capabilities: ModelPreviewCapabilities,
        cx: &mut Context<Self>,
    ) {
        self.preview_capabilities = capabilities;
        self.capabilities_revision = self.capabilities_revision.wrapping_add(1).max(1);
        self.editing_model_resource = None;
        cx.notify();
    }

    pub(super) fn variant_resource_key(relative_path: &Path) -> ModelResourceKey {
        ModelResourceKey::new(
            relative_path,
            ModelResourceKind::Variant,
            relative_path.to_string_lossy().into_owned(),
        )
    }

    pub(super) fn selected_resource_key(
        &self,
        kind: ModelResourceKind,
        runtime_id: &str,
    ) -> Option<ModelResourceKey> {
        self.catalog
            .selected_relative_path()
            .map(|manifest| ModelResourceKey::new(manifest, kind, runtime_id))
    }

    pub(super) fn model_resource_name(&self, key: &ModelResourceKey, default_name: &str) -> String {
        Self::model_resource_name_from(&self.model_resources, key, default_name)
    }

    fn model_resource_name_from(
        settings: &ModelResourceSettings,
        key: &ModelResourceKey,
        default_name: &str,
    ) -> String {
        settings.name(key).unwrap_or(default_name).to_owned()
    }

    pub(super) fn model_resource_is_renamed(&self, key: &ModelResourceKey) -> bool {
        self.model_resources.name(key).is_some()
    }

    pub(super) fn expression_category(
        &self,
        expression: &ModelPreviewExpression,
    ) -> ModelExpressionCategory {
        self.expression_category_from(&self.model_resources, expression)
    }

    fn expression_category_from(
        &self,
        settings: &ModelResourceSettings,
        expression: &ModelPreviewExpression,
    ) -> ModelExpressionCategory {
        if !expression.movable_to_outfit() {
            return ModelExpressionCategory::Expression;
        }
        self.selected_resource_key(
            ModelResourceKind::Expression,
            expression.resource().runtime_id(),
        )
        .map(|key| settings.expression_category(&key))
        .unwrap_or_default()
    }

    pub(super) fn begin_model_resource_rename(
        &mut self,
        key: ModelResourceKey,
        default_name: String,
        current_name: String,
        window: &mut gpui::Window,
        cx: &mut Context<Self>,
    ) {
        let Some(input) = self.model_resource_name_input.clone() else {
            return;
        };
        self.editing_model_resource = Some(EditingModelResource { key, default_name });
        input.update(cx, |input, cx| {
            input.set_value(current_name, window, cx);
            input.focus(window, cx);
        });
        cx.notify();
    }

    pub(super) fn commit_model_resource_name(
        &mut self,
        input: &Entity<InputState>,
        cx: &mut Context<Self>,
    ) {
        let Some(editing) = self.editing_model_resource.take() else {
            return;
        };
        let value = input.read(cx).value().trim().to_owned();
        let name = (value != editing.default_name).then_some(value.as_str());
        match self.model_resources.with_name(editing.key.clone(), name) {
            Ok(settings) => self.persist_model_resource_settings(settings, cx),
            Err(error) => {
                self.editing_model_resource = Some(editing);
                self.set_status(
                    t!(
                        "status.model_resource_save_failed",
                        error = error.to_string()
                    )
                    .to_string(),
                    cx,
                );
            }
        }
    }

    pub(super) fn reset_model_resource_name(
        &mut self,
        key: ModelResourceKey,
        cx: &mut Context<Self>,
    ) {
        match self.model_resources.with_name(key, None) {
            Ok(settings) => self.persist_model_resource_settings(settings, cx),
            Err(error) => self.set_status(
                t!(
                    "status.model_resource_save_failed",
                    error = error.to_string()
                )
                .to_string(),
                cx,
            ),
        }
    }

    pub(super) fn expression_drag(
        &self,
        expression: &ModelPreviewExpression,
    ) -> Option<ModelExpressionDrag> {
        let manifest = self.catalog.selected_relative_path()?.to_path_buf();
        expression.movable_to_outfit().then(|| ModelExpressionDrag {
            manifest,
            runtime_id: expression.resource().runtime_id().to_owned(),
            capabilities_revision: self.capabilities_revision,
        })
    }

    pub(super) fn move_expression_to_category(
        &mut self,
        drag: &ModelExpressionDrag,
        category: ModelExpressionCategory,
        cx: &mut Context<Self>,
    ) {
        if drag.capabilities_revision != self.capabilities_revision
            || self.catalog.selected_relative_path() != Some(drag.manifest.as_path())
            || !self
                .preview_capabilities
                .expressions()
                .iter()
                .any(|expression| {
                    expression.movable_to_outfit()
                        && expression.resource().runtime_id() == drag.runtime_id
                })
        {
            return;
        }
        let key = ModelResourceKey::new(
            &drag.manifest,
            ModelResourceKind::Expression,
            &drag.runtime_id,
        );
        match self.model_resources.with_expression_category(key, category) {
            Ok(settings) => self.persist_model_resource_settings(settings, cx),
            Err(error) => self.set_status(
                t!(
                    "status.model_resource_save_failed",
                    error = error.to_string()
                )
                .to_string(),
                cx,
            ),
        }
    }

    fn persist_model_resource_settings(
        &mut self,
        settings: ModelResourceSettings,
        cx: &mut Context<Self>,
    ) {
        if self.model_resources.as_ref() == &settings {
            cx.notify();
            return;
        }
        self.model_resources = Arc::new(settings.clone());
        self.model_resource_save_revision =
            self.model_resource_save_revision.wrapping_add(1).max(1);
        let ui_revision = self.model_resource_save_revision;
        let requested = settings.clone();
        let config_revision = CONFIG.reserve_model_resource_settings_revision();
        let background = cx.background_executor().clone();
        cx.notify();
        let task = cx.spawn(async move |this, cx| {
            let result = background
                .spawn(async move {
                    CONFIG.set_model_resource_settings_at_revision(settings, config_revision)
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                this.finish_model_resource_settings_write(ui_revision, requested, result, cx);
            });
        });
        self.track_write_task(task);
    }

    pub(super) fn finish_model_resource_settings_write(
        &mut self,
        ui_revision: u64,
        requested: ModelResourceSettings,
        result: Result<Option<Arc<ModelResourceSettings>>, crate::config::ConfigWriteError>,
        cx: &mut Context<Self>,
    ) {
        match result {
            Ok(Some(settings)) => {
                let current = CONFIG.model_resource_settings();
                if current.as_ref() != settings.as_ref() {
                    return;
                }
                self.applied.model_resources = settings.clone();
                if self.model_resource_save_revision == ui_revision
                    && self.model_resources.as_ref() == &requested
                {
                    self.model_resources = settings;
                }
                self.clear_unpublished_active_outfit();
                self.emit_settings_event(SettingsEvent::ModelResourcesChanged, cx);
                if self.model_resource_save_revision == ui_revision {
                    self.set_status(t!("status.model_resource_saved").to_string(), cx);
                } else {
                    cx.notify();
                }
            }
            Ok(None) => {}
            Err(error) if self.model_resource_save_revision == ui_revision => {
                self.set_status(
                    t!(
                        "status.model_resource_save_failed",
                        error = error.to_string()
                    )
                    .to_string(),
                    cx,
                );
            }
            Err(_) => {}
        }
    }

    fn clear_unpublished_active_outfit(&mut self) {
        let Some(active) = self.active_outfit.as_deref() else {
            return;
        };
        let remains_outfit = self
            .preview_capabilities
            .expressions()
            .iter()
            .any(|expression| {
                expression.resource().runtime_id() == active
                    && self.expression_category_from(&self.applied.model_resources, expression)
                        == ModelExpressionCategory::Outfit
            });
        if !remains_outfit {
            self.active_outfit = None;
        }
    }

    /// 返回当前已加载模型可交给 Agent 选择的稳定 ID 与本地化显示名。
    pub(in crate::ui) fn available_agent_outfits(&self) -> Vec<OutfitOption> {
        self.agent_outfit_candidates()
            .into_iter()
            .map(|candidate| OutfitOption::new(candidate.id, candidate.label))
            .collect()
    }

    /// 将 Agent 传回的稳定 ID 解析为当前目录和 generation 下的语义动作。
    pub(in crate::ui) fn resolve_agent_outfit(
        &self,
        requested_id: &str,
    ) -> Option<AgentOutfitAction> {
        let candidate = self
            .agent_outfit_candidates()
            .into_iter()
            .find(|candidate| candidate.id == requested_id)?;
        Some(match candidate.target {
            AgentOutfitTarget::Variant(relative_path) => {
                if self.catalog.selected_relative_path() == Some(relative_path.as_path()) {
                    if self.active_outfit.is_some() {
                        AgentOutfitAction::ResetExpression
                    } else {
                        AgentOutfitAction::Unchanged
                    }
                } else {
                    AgentOutfitAction::LoadVariant(relative_path)
                }
            }
            AgentOutfitTarget::Expression(name) => {
                if self.active_outfit.as_deref() == Some(name.as_str()) {
                    AgentOutfitAction::Unchanged
                } else {
                    AgentOutfitAction::PreviewExpression(name)
                }
            }
        })
    }

    /// 准备 Agent 的即时换装动作；仅跟随全局模型的人格会持久化清单变体。
    pub(in crate::ui) fn commit_agent_outfit(
        &mut self,
        action: AgentOutfitAction,
        cx: &mut Context<Self>,
    ) -> Result<Option<ModelManifest>, String> {
        let active_has_binding = self.active_persona_has_live2d_binding();
        self.commit_agent_outfit_with_binding(action, active_has_binding, cx)
    }

    pub(super) fn commit_agent_outfit_with_binding(
        &mut self,
        action: AgentOutfitAction,
        active_has_binding: bool,
        cx: &mut Context<Self>,
    ) -> Result<Option<ModelManifest>, String> {
        match action {
            AgentOutfitAction::Unchanged => Ok(None),
            AgentOutfitAction::LoadVariant(relative_path) => {
                let baseline = self.capture_model_selection_baseline();
                let model_path = self
                    .catalog
                    .select_variant(&relative_path)
                    .map_err(|error| {
                        let error = error.to_string();
                        self.set_status(
                            t!("status.model_action_failed", error = error.clone()).to_string(),
                            cx,
                        );
                        error
                    })?;
                self.active_outfit = None;
                if active_has_binding {
                    self.catalog_revision = self.catalog_revision.wrapping_add(1);
                    cx.notify();
                } else {
                    // runtime 先切换，发布 marker 与失败回滚由同一个保存 revision 收口。
                    self.commit_preapplied_model_selection(Some(relative_path), baseline, cx);
                }
                Ok(Some(model_path))
            }
            AgentOutfitAction::PreviewExpression(name) => {
                self.active_outfit = Some(name);
                cx.notify();
                Ok(None)
            }
            AgentOutfitAction::ResetExpression => {
                self.active_outfit = None;
                cx.notify();
                Ok(None)
            }
        }
    }

    fn agent_outfit_candidates(&self) -> Vec<AgentOutfitCandidate> {
        if !self.applied.allow_agent_outfit_change {
            return Vec::new();
        }
        let Some(family) = self.catalog.selected_family() else {
            return Vec::new();
        };
        let variants = family.variants();
        let default_outfit = variants.len() == 1;
        let mut candidates = Vec::new();
        for variant in variants {
            let default_name = if default_outfit {
                t!("model.default_outfit").to_string()
            } else {
                variant.display_name().to_owned()
            };
            let key = Self::variant_resource_key(variant.relative_path());
            candidates.push(AgentOutfitCandidate {
                id: format!("variant:{}", variant.relative_path().to_string_lossy()),
                label: Self::model_resource_name_from(
                    &self.applied.model_resources,
                    &key,
                    &default_name,
                ),
                target: AgentOutfitTarget::Variant(variant.relative_path().to_path_buf()),
            });
        }
        for expression in self.preview_capabilities.expressions() {
            if self.expression_category_from(&self.applied.model_resources, expression)
                != ModelExpressionCategory::Outfit
            {
                continue;
            }
            let resource = expression.resource();
            let Some(key) =
                self.selected_resource_key(ModelResourceKind::Expression, resource.runtime_id())
            else {
                continue;
            };
            candidates.push(AgentOutfitCandidate {
                id: format!("expression:{}", resource.runtime_id()),
                label: Self::model_resource_name_from(
                    &self.applied.model_resources,
                    &key,
                    resource.default_name(),
                ),
                target: AgentOutfitTarget::Expression(resource.runtime_id().to_owned()),
            });
        }
        let mut used_names = HashSet::with_capacity(candidates.len());
        for candidate in &mut candidates {
            candidate.label = unique_outfit_name(&candidate.label, &mut used_names);
        }
        candidates
    }

    pub(super) fn preview_outfit(
        &mut self,
        runtime_id: String,
        display_name: String,
        cx: &mut Context<Self>,
    ) {
        self.active_outfit = Some(runtime_id.clone());
        self.emit_settings_event(SettingsEvent::PreviewExpression(runtime_id), cx);
        self.set_status(
            t!("status.outfit_preview", name = display_name).to_string(),
            cx,
        );
        cx.notify();
    }

    pub(super) fn preview_motion(
        &mut self,
        runtime_id: String,
        display_name: String,
        cx: &mut Context<Self>,
    ) {
        self.emit_settings_event(SettingsEvent::PreviewMotion(runtime_id), cx);
        self.set_status(
            t!("status.motion_preview", name = display_name).to_string(),
            cx,
        );
    }

    pub(super) fn preview_expression(
        &mut self,
        runtime_id: String,
        display_name: String,
        cx: &mut Context<Self>,
    ) {
        self.active_outfit = None;
        self.emit_settings_event(SettingsEvent::PreviewExpression(runtime_id), cx);
        self.set_status(
            t!("status.expression_preview", name = display_name).to_string(),
            cx,
        );
    }
}

fn unique_outfit_name(base: &str, used_names: &mut HashSet<String>) -> String {
    if used_names.insert(base.to_owned()) {
        return base.to_owned();
    }
    for suffix in 2_u32.. {
        let candidate = format!("{base} ({suffix})");
        if used_names.insert(candidate.clone()) {
            return candidate;
        }
    }
    unreachable!("无界递增后缀必须能生成唯一服装名称")
}
