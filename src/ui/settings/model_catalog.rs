//! 管理模型选择事务以及人格 Live2D 绑定的运行时解析。

use std::path::{Path, PathBuf};

use gpui::Context;
use rust_i18n::t;

use crate::{
    config::{CONFIG, ConfigWriteError},
    model::{ModelCatalog, ModelManifest},
};

use super::{
    ActivePersonaModelBinding, ModelSelectionBaseline, ModelSelectionWriteState,
    PendingModelRuntime, PendingModelSelectionWrite, SettingsEvent, SettingsView,
};

#[derive(Clone, Copy)]
pub(super) enum ModelSelectionApplyMode {
    PersonaChange,
    AdoptPreappliedRuntime,
    RestoreCommitted,
    ReloadCatalog,
}

impl ModelSelectionBaseline {
    pub(super) fn initial(catalog: &ModelCatalog, global_selection: Option<PathBuf>) -> Self {
        let runtime_selection = catalog.selected_relative_path().map(Path::to_path_buf);
        let runtime_model_path = runtime_selection
            .as_deref()
            .and_then(|relative_path| catalog.model_path(relative_path));
        Self {
            runtime_selection,
            runtime_model_path,
            global_selection,
            applied_persona_id: None,
            applied_persona_model: None,
            active_outfit: None,
        }
    }
}

impl ModelSelectionWriteState {
    pub(super) fn new(committed: ModelSelectionBaseline) -> Self {
        Self {
            next_revision: 0,
            committed,
            pending: None,
        }
    }

    fn stage(
        &mut self,
        requested_selection: Option<PathBuf>,
        current_baseline: ModelSelectionBaseline,
        runtime: Option<PendingModelRuntime>,
    ) -> u64 {
        if self
            .pending
            .as_ref()
            .is_none_or(|pending| pending.runtime.is_none())
        {
            self.committed = current_baseline;
        }
        self.next_revision = self.next_revision.wrapping_add(1).max(1);
        let save_revision = self.next_revision;
        self.pending = Some(PendingModelSelectionWrite {
            save_revision,
            requested_selection,
            runtime,
        });
        save_revision
    }

    pub(super) fn synchronize_committed(&mut self, committed: ModelSelectionBaseline) {
        self.committed = committed;
    }

    fn has_pending(&self) -> bool {
        self.pending.is_some()
    }

    fn take_pending(
        &mut self,
        save_revision: u64,
        requested_selection: &Option<PathBuf>,
    ) -> Option<PendingModelSelectionWrite> {
        let pending = self.pending.as_ref()?;
        if pending.save_revision != save_revision
            || pending.requested_selection.as_deref() != requested_selection.as_deref()
        {
            return None;
        }
        self.pending.take()
    }

    fn release_pending_runtime(&mut self) {
        if let Some(pending) = &mut self.pending {
            pending.runtime = None;
        }
    }
}

impl SettingsView {
    fn global_live2d_fallback(
        &self,
        configured: Option<&Path>,
    ) -> (Option<PathBuf>, Option<ModelManifest>) {
        if let Some(relative) = configured
            && let Some(path) = self.catalog.model_path(relative)
        {
            return (Some(relative.to_path_buf()), Some(path));
        }
        let [family] = self.catalog.families() else {
            return (None, None);
        };
        let Some(variant) = family.variants().first() else {
            return (None, None);
        };
        let relative = variant.relative_path().to_path_buf();
        let path = self.catalog.model_path(&relative);
        (Some(relative), path)
    }

    pub(super) fn resolve_persona_live2d_model(
        &self,
        bound: Option<&Path>,
        global: Option<&Path>,
    ) -> (Option<PathBuf>, Option<ModelManifest>, Option<String>) {
        if let Some(bound) = bound {
            if let Some(path) = self.catalog.model_path(bound) {
                return (Some(bound.to_path_buf()), Some(path), None);
            }
            let (fallback, path) = self.global_live2d_fallback(global);
            let warning = if path.is_some() {
                t!(
                    "persona.live2d_fallback",
                    path = bound.to_string_lossy().into_owned()
                )
                .to_string()
            } else {
                t!(
                    "persona.live2d_missing",
                    path = bound.to_string_lossy().into_owned()
                )
                .to_string()
            };
            return (fallback, path, Some(warning));
        }
        let (relative, path) = self.global_live2d_fallback(global);
        (relative, path, None)
    }

    pub(super) fn active_persona_model_binding() -> ActivePersonaModelBinding {
        let personas = CONFIG.persona_settings();
        let active = personas.active();
        ActivePersonaModelBinding {
            persona_id: active.map(|persona| persona.id.clone()),
            relative_path: active.and_then(|persona| persona.live2d_model.clone()),
        }
    }

    fn model_selection_baseline(
        &self,
        global_selection: Option<PathBuf>,
        persona_binding: &ActivePersonaModelBinding,
        reset_outfit: bool,
    ) -> (ModelSelectionBaseline, Option<String>) {
        let (runtime_selection, runtime_model_path, warning) = self.resolve_persona_live2d_model(
            persona_binding.relative_path.as_deref(),
            global_selection.as_deref(),
        );
        let runtime_is_current = self.catalog.selected_relative_path()
            == runtime_selection.as_deref()
            && self.applied_persona_id.as_deref() == persona_binding.persona_id.as_deref()
            && self.applied_persona_model.as_deref() == runtime_selection.as_deref();
        let committed_runtime_is_target = self
            .model_selection_write_state
            .committed
            .runtime_selection
            .as_deref()
            == runtime_selection.as_deref()
            && self
                .model_selection_write_state
                .committed
                .applied_persona_id
                .as_deref()
                == persona_binding.persona_id.as_deref()
            && self
                .model_selection_write_state
                .committed
                .applied_persona_model
                .as_deref()
                == runtime_selection.as_deref();
        let active_outfit = if reset_outfit {
            None
        } else if runtime_is_current {
            self.active_outfit.clone()
        } else if committed_runtime_is_target {
            self.model_selection_write_state
                .committed
                .active_outfit
                .clone()
        } else {
            None
        };
        (
            ModelSelectionBaseline {
                runtime_selection: runtime_selection.clone(),
                runtime_model_path,
                global_selection,
                applied_persona_id: persona_binding.persona_id.clone(),
                applied_persona_model: runtime_selection,
                active_outfit,
            },
            warning,
        )
    }

    pub(super) fn capture_current_model_selection_baseline(&self) -> ModelSelectionBaseline {
        let runtime_selection = self.catalog.selected_relative_path().map(Path::to_path_buf);
        let runtime_model_path = runtime_selection
            .as_deref()
            .and_then(|relative_path| self.catalog.model_path(relative_path));
        ModelSelectionBaseline {
            runtime_selection,
            runtime_model_path,
            global_selection: self.applied.global_model_selection.clone(),
            applied_persona_id: self.applied_persona_id.clone(),
            applied_persona_model: self.applied_persona_model.clone(),
            active_outfit: self.active_outfit.clone(),
        }
    }

    pub(super) fn apply_active_persona_live2d_model(&mut self, cx: &mut Context<Self>) {
        let persona_binding = Self::active_persona_model_binding();
        self.reconcile_committed_model_selection(
            CONFIG.selected_model(),
            persona_binding,
            ModelSelectionApplyMode::PersonaChange,
            cx,
        );
    }

    #[cfg(test)]
    pub(super) fn reconcile_published_persona_model_for_test(
        &mut self,
        global_selection: Option<PathBuf>,
        persona_id: Option<String>,
        persona_binding: Option<&Path>,
        cx: &mut Context<Self>,
    ) {
        let persona_binding = ActivePersonaModelBinding {
            persona_id,
            relative_path: persona_binding.map(Path::to_path_buf),
        };
        self.reconcile_committed_model_selection(
            global_selection,
            persona_binding,
            ModelSelectionApplyMode::PersonaChange,
            cx,
        );
    }

    pub(super) fn reconcile_committed_model_selection(
        &mut self,
        global_selection: Option<PathBuf>,
        persona_binding: ActivePersonaModelBinding,
        mode: ModelSelectionApplyMode,
        cx: &mut Context<Self>,
    ) {
        let reset_outfit = matches!(mode, ModelSelectionApplyMode::ReloadCatalog);
        let (baseline, warning) =
            self.model_selection_baseline(global_selection, &persona_binding, reset_outfit);
        let adopt_preapplied_runtime = matches!(mode, ModelSelectionApplyMode::PersonaChange)
            && self
                .model_selection_write_state
                .pending
                .as_ref()
                .and_then(|pending| pending.runtime.as_ref())
                .is_some_and(|runtime| {
                    runtime.selection.as_deref() == baseline.runtime_selection.as_deref()
                        && self.catalog.selected_relative_path()
                            == baseline.runtime_selection.as_deref()
                });
        let apply_mode = if adopt_preapplied_runtime {
            ModelSelectionApplyMode::AdoptPreappliedRuntime
        } else {
            mode
        };
        self.applied
            .global_model_selection
            .clone_from(&baseline.global_selection);
        if !self.model_selection_write_state.has_pending() {
            self.global_model_selection
                .clone_from(&baseline.global_selection);
        }
        self.model_selection_write_state
            .synchronize_committed(baseline.clone());
        if self.apply_model_selection_baseline(&baseline, apply_mode, cx) {
            self.model_selection_write_state.release_pending_runtime();
        } else {
            cx.notify();
        }
        if let Some(warning) = warning {
            self.set_status(warning, cx);
        }
    }

    fn apply_model_selection_baseline(
        &mut self,
        baseline: &ModelSelectionBaseline,
        mode: ModelSelectionApplyMode,
        cx: &mut Context<Self>,
    ) -> bool {
        let persona_changed = self.applied_persona_id != baseline.applied_persona_id;
        let configured_model_changed = self.applied_persona_model != baseline.applied_persona_model;
        let runtime_changed =
            self.catalog.selected_relative_path() != baseline.runtime_selection.as_deref();
        let outfit_changed = self.active_outfit != baseline.active_outfit;
        let should_apply = match mode {
            ModelSelectionApplyMode::PersonaChange => persona_changed || configured_model_changed,
            ModelSelectionApplyMode::AdoptPreappliedRuntime => true,
            ModelSelectionApplyMode::RestoreCommitted => {
                persona_changed || configured_model_changed || runtime_changed || outfit_changed
            }
            ModelSelectionApplyMode::ReloadCatalog => true,
        };
        if !should_apply {
            return false;
        }
        if (runtime_changed || matches!(mode, ModelSelectionApplyMode::ReloadCatalog))
            && let Err(error) = self
                .catalog
                .set_runtime_selection(baseline.runtime_selection.as_deref())
        {
            self.set_status(
                t!("status.model_action_failed", error = error.to_string()).to_string(),
                cx,
            );
            return false;
        }
        let previous_outfit = self.active_outfit.clone();
        self.applied_persona_id
            .clone_from(&baseline.applied_persona_id);
        self.applied_persona_model
            .clone_from(&baseline.applied_persona_model);
        self.active_outfit.clone_from(&baseline.active_outfit);
        let reload_model = runtime_changed
            || matches!(mode, ModelSelectionApplyMode::ReloadCatalog)
            || (configured_model_changed && matches!(mode, ModelSelectionApplyMode::PersonaChange));
        if reload_model {
            self.emit_settings_event(
                SettingsEvent::ModelChanged(baseline.runtime_model_path.clone()),
                cx,
            );
            cx.notify();
        } else if persona_changed || configured_model_changed || outfit_changed {
            if previous_outfit.is_some() && baseline.active_outfit.is_none() {
                self.emit_settings_event(SettingsEvent::ResetExpression, cx);
            }
            cx.notify();
        }
        true
    }

    pub(super) fn commit_model_selection(
        &mut self,
        relative_path: Option<PathBuf>,
        cx: &mut Context<Self>,
    ) {
        self.commit_model_selection_inner(relative_path, None, cx);
    }

    pub(super) fn commit_preapplied_model_selection(
        &mut self,
        relative_path: Option<PathBuf>,
        baseline: ModelSelectionBaseline,
        cx: &mut Context<Self>,
    ) {
        self.commit_model_selection_inner(relative_path, Some(baseline), cx);
    }

    fn commit_model_selection_inner(
        &mut self,
        relative_path: Option<PathBuf>,
        baseline: Option<ModelSelectionBaseline>,
        cx: &mut Context<Self>,
    ) {
        if let Some(input) = self.model_resource_name_input.clone() {
            self.commit_model_resource_name(&input, cx);
        }
        let model_save_revision = match baseline {
            Some(baseline) => {
                self.stage_preapplied_model_selection(relative_path.clone(), baseline, cx)
            }
            None => self.stage_model_selection(relative_path.clone(), cx),
        };

        let config_revision = CONFIG.reserve_model_revision();
        let background = cx.background_executor().clone();
        let requested_selection = relative_path.clone();
        let task = cx.spawn(async move |this, cx| {
            let result = background
                .spawn(async move {
                    CONFIG.set_selected_model_at_revision(relative_path.as_deref(), config_revision)
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                let persisted_selection = CONFIG.selected_model();
                this.finish_model_selection_write(
                    model_save_revision,
                    requested_selection,
                    result,
                    persisted_selection,
                    cx,
                );
            });
        });
        self.track_write_task(task);
    }

    pub(super) fn stage_model_selection(
        &mut self,
        relative_path: Option<PathBuf>,
        cx: &mut Context<Self>,
    ) -> u64 {
        self.stage_model_selection_inner(relative_path, None, cx)
    }

    pub(super) fn stage_preapplied_model_selection(
        &mut self,
        relative_path: Option<PathBuf>,
        baseline: ModelSelectionBaseline,
        cx: &mut Context<Self>,
    ) -> u64 {
        self.stage_model_selection_inner(relative_path, Some(baseline), cx)
    }

    fn stage_model_selection_inner(
        &mut self,
        relative_path: Option<PathBuf>,
        baseline: Option<ModelSelectionBaseline>,
        cx: &mut Context<Self>,
    ) -> u64 {
        self.catalog_revision = self.catalog_revision.wrapping_add(1);
        let requested_selection = relative_path.clone();
        let (current_baseline, pending_runtime) = match baseline {
            Some(baseline) => (
                baseline,
                Some(PendingModelRuntime {
                    selection: relative_path.clone(),
                }),
            ),
            None => (self.capture_current_model_selection_baseline(), None),
        };
        let save_revision = self.model_selection_write_state.stage(
            requested_selection,
            current_baseline,
            pending_runtime,
        );
        self.global_model_selection = relative_path;
        cx.notify();
        save_revision
    }

    pub(super) fn capture_model_selection_baseline(&self) -> ModelSelectionBaseline {
        self.capture_current_model_selection_baseline()
    }

    pub(super) fn finish_model_selection_write(
        &mut self,
        save_revision: u64,
        requested_selection: Option<PathBuf>,
        result: Result<Option<()>, ConfigWriteError>,
        persisted_selection: Option<PathBuf>,
        cx: &mut Context<Self>,
    ) {
        let persona_binding = Self::active_persona_model_binding();
        self.finish_model_selection_write_with_persona(
            save_revision,
            requested_selection,
            result,
            persisted_selection,
            persona_binding,
            cx,
        );
    }

    pub(super) fn finish_model_selection_write_with_persona(
        &mut self,
        save_revision: u64,
        requested_selection: Option<PathBuf>,
        result: Result<Option<()>, ConfigWriteError>,
        persisted_selection: Option<PathBuf>,
        persona_binding: ActivePersonaModelBinding,
        cx: &mut Context<Self>,
    ) {
        let (baseline, warning) =
            self.model_selection_baseline(persisted_selection, &persona_binding, false);
        self.applied
            .global_model_selection
            .clone_from(&baseline.global_selection);
        self.model_selection_write_state
            .synchronize_committed(baseline.clone());
        if matches!(&result, Ok(Some(()))) {
            // 成功发布可能晚于扫描启动；使携带旧启动基线的扫描结果失效。
            self.catalog_revision = self.catalog_revision.wrapping_add(1);
        }
        let Some(pending) = self
            .model_selection_write_state
            .take_pending(save_revision, &requested_selection)
        else {
            cx.notify();
            return;
        };

        if matches!(&result, Ok(Some(()))) && baseline.global_selection == requested_selection {
            self.global_model_selection
                .clone_from(&baseline.global_selection);
            if !self.publish_preapplied_model_selection(&pending, &baseline) {
                self.apply_model_selection_baseline(
                    &baseline,
                    ModelSelectionApplyMode::RestoreCommitted,
                    cx,
                );
            }
            if let Some(warning) = warning {
                self.set_status(warning, cx);
            } else {
                cx.notify();
            }
            return;
        }

        self.catalog_revision = self.catalog_revision.wrapping_add(1);
        self.global_model_selection
            .clone_from(&baseline.global_selection);
        self.apply_model_selection_baseline(
            &baseline,
            ModelSelectionApplyMode::RestoreCommitted,
            cx,
        );
        if let Err(error) = result {
            self.set_status(
                t!("status.model_save_failed", error = error.to_string()).to_string(),
                cx,
            );
            return;
        }
        cx.notify();
    }

    fn publish_preapplied_model_selection(
        &mut self,
        pending: &PendingModelSelectionWrite,
        baseline: &ModelSelectionBaseline,
    ) -> bool {
        let Some(runtime) = &pending.runtime else {
            return false;
        };
        if runtime.selection.as_deref() != baseline.runtime_selection.as_deref()
            || self.catalog.selected_relative_path() != baseline.runtime_selection.as_deref()
        {
            return false;
        }
        self.applied_persona_id
            .clone_from(&baseline.applied_persona_id);
        self.applied_persona_model
            .clone_from(&baseline.applied_persona_model);
        self.active_outfit.clone_from(&baseline.active_outfit);
        true
    }
}
