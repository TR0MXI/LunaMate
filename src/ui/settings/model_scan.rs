//! 管理模型候选、目录选择以及异步扫描结果的发布生命周期。

use std::path::{Path, PathBuf};

use gpui::{Context, Window};
use rust_i18n::t;

use crate::{
    config::CONFIG,
    model::{ModelCatalog, ensure_model_directory},
};

use super::{
    ActivePersonaModelBinding, SettingsEvent, SettingsView, model_catalog::ModelSelectionApplyMode,
};

impl SettingsView {
    pub(super) fn persona_live2d_models(&self) -> Vec<(String, PathBuf)> {
        self.catalog
            .families()
            .iter()
            .flat_map(|family| {
                let variants = family.variants();
                variants.iter().map(move |variant| {
                    let default_name = if variants.len() == 1 {
                        family.display_name()
                    } else {
                        variant.display_name()
                    };
                    let key = Self::variant_resource_key(variant.relative_path());
                    let display_name = self.model_resource_name(&key, default_name);
                    let label = if variants.len() == 1 {
                        display_name
                    } else {
                        format!("{} / {display_name}", family.display_name())
                    };
                    (label, variant.relative_path().to_path_buf())
                })
            })
            .collect()
    }

    pub(super) fn refresh_persona_live2d_models(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let models = self.persona_live2d_models();
        #[cfg(test)]
        let candidate_count = models.len();
        if let Some(persona) = &self.persona_settings_view {
            persona.update(cx, |persona, cx| {
                persona.refresh_live2d_models(models, window, cx);
            });
            #[cfg(test)]
            {
                self.persona_live2d_refresh_revision =
                    self.persona_live2d_refresh_revision.wrapping_add(1).max(1);
                self.persona_live2d_candidate_count = candidate_count;
            }
        }
    }

    pub(super) fn select_family(&mut self, index: usize, cx: &mut Context<Self>) {
        let relative_path = self.catalog.families().get(index).and_then(|family| {
            self.global_model_selection
                .as_deref()
                .filter(|selected| family.contains(selected))
                .map(Path::to_path_buf)
                .or_else(|| {
                    family
                        .variants()
                        .first()
                        .map(|variant| variant.relative_path().to_path_buf())
                })
        });
        let Some(relative_path) = relative_path else {
            self.set_status(
                t!("status.model_action_failed", error = "模型家族没有可用清单").to_string(),
                cx,
            );
            return;
        };
        self.select_global_model(relative_path, cx);
    }

    pub(super) fn select_variant(&mut self, relative_path: PathBuf, cx: &mut Context<Self>) {
        if self.global_model_selection.as_deref() == Some(relative_path.as_path()) {
            if !self.active_persona_has_live2d_binding()
                && self.catalog.selected_relative_path() == Some(relative_path.as_path())
                && self.active_outfit.take().is_some()
            {
                self.emit_settings_event(SettingsEvent::ResetExpression, cx);
                cx.notify();
            }
            return;
        }
        self.select_global_model(relative_path, cx);
    }

    fn select_global_model(&mut self, relative_path: PathBuf, cx: &mut Context<Self>) {
        if self.catalog.model_path(&relative_path).is_none() {
            self.set_status(
                t!(
                    "status.model_action_failed",
                    error = format!("模型不在当前目录扫描结果中：{}", relative_path.display())
                )
                .to_string(),
                cx,
            );
            return;
        }
        self.commit_model_selection(Some(relative_path), cx);
    }

    pub(super) fn active_persona_has_live2d_binding(&self) -> bool {
        CONFIG
            .persona_settings()
            .active()
            .is_some_and(|persona| persona.live2d_model.is_some())
    }

    /// 在首窗建立后启动初始模型扫描，避免目录 I/O 阻塞 GPUI 初始化。
    pub(crate) fn start_initial_scan(
        &mut self,
        configured_selection: Option<PathBuf>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.global_model_selection
            .clone_from(&configured_selection);
        self.applied
            .global_model_selection
            .clone_from(&configured_selection);
        let baseline = self.capture_current_model_selection_baseline();
        self.model_selection_write_state
            .synchronize_committed(baseline);
        self.refresh_models_with_selection(configured_selection, false, window, cx);
    }

    pub(super) fn refresh_models(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let previous_selection = self.global_model_selection.clone();
        self.refresh_models_with_selection(previous_selection, true, window, cx);
    }

    pub(super) fn open_model_directory(&mut self, cx: &mut Context<Self>) {
        let root = self.catalog.root().to_path_buf();
        let background = cx.background_executor().clone();
        cx.spawn(async move |this, cx| {
            let result = background
                .spawn(async move {
                    ensure_model_directory(&root)
                        .map_err(|error| format!("{}：{error}", root.display()))?;
                    Ok::<PathBuf, String>(root)
                })
                .await;
            let _ = this.update(cx, |this, cx| match result {
                Ok(directory) => {
                    log::info!("event=model_directory_open_requested");
                    cx.open_with_system(&directory);
                    this.set_status(t!("status.opening_model_directory").to_string(), cx);
                }
                Err(error) => {
                    log::warn!("event=model_directory_open_failed stage=prepare_or_launch");
                    this.set_status(
                        t!("status.open_model_directory_failed", error = error).to_string(),
                        cx,
                    );
                }
            });
        })
        .detach();
    }

    fn refresh_models_with_selection(
        &mut self,
        previous_selection: Option<PathBuf>,
        window_scoped: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.is_refreshing {
            return;
        }
        self.is_refreshing = true;
        self.refresh_window_scoped = window_scoped;
        self.set_status(t!("status.scanning_models").to_string(), cx);
        let root = self.catalog.root().to_path_buf();
        let catalog_revision = self.catalog_revision;
        let background = cx.background_executor().clone();
        log::debug!("event=model_catalog_scan_started scan_revision={catalog_revision}");
        cx.notify();

        self.refresh_task = Some(cx.spawn_in(window, async move |this, cx| {
            let catalog = background
                .spawn(async move {
                    ModelCatalog::load(root, previous_selection.as_deref())
                        .map_err(|error| error.to_string())
                })
                .await;
            let _ = cx.update(|_window, app| {
                let _ = this.update(app, |this, cx| {
                    // 配置与人格都可能在扫描期间完成发布，只能在接纳结果时复核。
                    let committed_selection = CONFIG.selected_model();
                    let persona_binding = Self::active_persona_model_binding();
                    this.finish_model_scan(
                        catalog_revision,
                        catalog,
                        committed_selection,
                        persona_binding,
                        cx,
                    );
                });
            });
        }));
    }

    pub(super) fn finish_model_scan(
        &mut self,
        catalog_revision: u64,
        catalog: Result<ModelCatalog, String>,
        committed_selection: Option<PathBuf>,
        persona_binding: ActivePersonaModelBinding,
        cx: &mut Context<Self>,
    ) {
        self.is_refreshing = false;
        self.refresh_window_scoped = false;
        if self.catalog_revision != catalog_revision {
            self.set_status(t!("status.scan_stale").to_string(), cx);
            return;
        }
        match catalog {
            Ok(catalog) => {
                let (families, outfits) = catalog.counts();
                let warning = catalog.warning().map(str::to_owned);
                if warning.is_some() {
                    log::warn!(
                        "event=model_catalog_scan_completed scan_revision={catalog_revision} families={families} outfits={outfits} warning=true"
                    );
                } else {
                    log::info!(
                        "event=model_catalog_scan_completed scan_revision={catalog_revision} families={families} outfits={outfits} warning=false"
                    );
                }
                self.catalog = catalog;
                let status = match warning {
                    Some(warning) => t!(
                        "status.scan_result_warning",
                        families = families,
                        outfits = outfits,
                        warning = warning
                    )
                    .to_string(),
                    None => {
                        t!("status.scan_result", families = families, outfits = outfits).to_string()
                    }
                };
                self.set_status(status, cx);
                self.emit_settings_event(SettingsEvent::ModelCatalogChanged, cx);
                self.reconcile_committed_model_selection(
                    committed_selection,
                    persona_binding,
                    ModelSelectionApplyMode::ReloadCatalog,
                    cx,
                );
            }
            Err(error) => {
                log::warn!(
                    "event=model_catalog_scan_failed scan_revision={catalog_revision} stage=root_scan"
                );
                self.set_status(t!("status.scan_failed", error = error).to_string(), cx);
            }
        }
    }
}
