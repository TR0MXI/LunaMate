//! 提供设置实体测试所需的受限状态注入与只读观测接口。

use std::{
    path::{Path, PathBuf},
    sync::Arc,
};

use gpui::{Context, Window};

use crate::{
    config::{
        AppLanguage, AppearanceSettings, CONFIG, ConfigWriteError, LoggingSettings,
        ModelExpressionCategory, ModelResourceKey, ModelResourceKind, ModelWindowSize,
    },
    logging::ApplyLoggingSettingsOutcome,
    model::{ModelCatalog, ModelPreviewCapabilities},
};

use super::{
    ActivePersonaModelBinding, AgentOutfitAction, ConfigSection, SettingsEvent, SettingsView,
    next_save_revision,
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(in crate::ui) enum SettingsEventKindForTest {
    Model,
    ModelResources,
    ModelWindowSize,
    EyeTracking,
    NativeTrayMenu,
    Appearance,
}

impl SettingsView {
    /// 返回当前 toast 状态文本，供测试断言扫描与失败提示。
    pub(in crate::ui) fn status_for_test(&self) -> Option<&str> {
        self.status.as_deref()
    }

    pub(in crate::ui) fn logging_settings_for_test(&self) -> (LoggingSettings, LoggingSettings) {
        (self.logging, self.persisted_logging)
    }

    pub(in crate::ui) fn stage_logging_settings_for_test(
        &mut self,
        requested: LoggingSettings,
    ) -> u64 {
        self.logging = requested;
        next_save_revision(&mut self.preference_save_revisions.logging)
    }

    pub(in crate::ui) fn finish_logging_settings_write_for_test(
        &mut self,
        ui_revision: u64,
        requested: LoggingSettings,
        outcome: ApplyLoggingSettingsOutcome,
        cx: &mut Context<Self>,
    ) {
        self.finish_logging_write(ui_revision, requested, requested, Ok(Some(outcome)), cx);
    }

    /// 注入尚未发布的外观草稿，验证窗口激活只采用已发布配置。
    pub(in crate::ui) fn set_appearance_language_for_test(&mut self, language: AppLanguage) {
        self.appearance.language = language;
    }

    pub(in crate::ui) fn appearance_language_for_test(&self) -> AppLanguage {
        self.appearance.language
    }

    pub(in crate::ui) fn applied_appearance_for_test(&self) -> &AppearanceSettings {
        &self.applied.appearance
    }

    pub(in crate::ui) fn model_window_size_for_test(&self) -> ModelWindowSize {
        self.model_window_size
    }

    pub(in crate::ui) fn applied_model_window_size_for_test(&self) -> ModelWindowSize {
        self.applied.model_window_size
    }

    pub(in crate::ui) fn eye_tracking_for_test(&self) -> bool {
        self.eye_tracking
    }

    pub(in crate::ui) fn applied_eye_tracking_for_test(&self) -> bool {
        self.applied.eye_tracking
    }

    pub(in crate::ui) fn use_native_tray_menu_for_test(&self) -> bool {
        self.use_native_tray_menu
    }

    pub(in crate::ui) fn applied_use_native_tray_menu_for_test(&self) -> bool {
        self.applied.use_native_tray_menu
    }

    pub(in crate::ui) fn emitted_event_count_for_test(
        &self,
        kind: SettingsEventKindForTest,
    ) -> usize {
        self.emitted_settings_events
            .iter()
            .filter(|event| settings_event_kind(event) == Some(kind))
            .count()
    }

    pub(in crate::ui) fn emitted_model_paths_for_test(&self) -> Vec<Option<PathBuf>> {
        self.emitted_settings_events
            .iter()
            .filter_map(|event| match event {
                SettingsEvent::ModelChanged(path) => {
                    Some(path.as_ref().map(|manifest| manifest.path().to_path_buf()))
                }
                _ => None,
            })
            .collect()
    }

    pub(in crate::ui) fn inject_model_window_size_failure_for_test(
        &mut self,
        requested: ModelWindowSize,
        cx: &mut Context<Self>,
    ) {
        self.model_window_size = requested;
        let ui_revision = next_save_revision(&mut self.preference_save_revisions.model_window_size);
        self.finish_model_window_size_write(
            ui_revision,
            requested,
            Err(ConfigWriteError::PersistenceUnavailable),
            cx,
        );
    }

    pub(in crate::ui) fn replace_model_window_size_draft_for_test(
        &mut self,
        requested: ModelWindowSize,
    ) -> u64 {
        self.model_window_size = requested;
        next_save_revision(&mut self.preference_save_revisions.model_window_size)
    }

    pub(in crate::ui) fn finish_model_window_size_write_for_test(
        &mut self,
        ui_revision: u64,
        requested: ModelWindowSize,
        result: Result<Option<()>, ConfigWriteError>,
        cx: &mut Context<Self>,
    ) {
        self.finish_model_window_size_write(ui_revision, requested, result, cx);
    }

    pub(in crate::ui) fn inject_eye_tracking_failure_for_test(
        &mut self,
        requested: bool,
        cx: &mut Context<Self>,
    ) {
        self.eye_tracking = requested;
        let ui_revision = next_save_revision(&mut self.preference_save_revisions.eye_tracking);
        self.finish_eye_tracking_write(
            ui_revision,
            requested,
            Err(ConfigWriteError::PersistenceUnavailable),
            cx,
        );
    }

    pub(in crate::ui) fn inject_native_tray_menu_failure_for_test(
        &mut self,
        requested: bool,
        cx: &mut Context<Self>,
    ) {
        self.use_native_tray_menu = requested;
        let ui_revision =
            next_save_revision(&mut self.preference_save_revisions.use_native_tray_menu);
        self.finish_use_native_tray_menu_write(
            ui_revision,
            requested,
            Err(ConfigWriteError::PersistenceUnavailable),
            cx,
        );
    }

    pub(in crate::ui) fn inject_appearance_failure_for_test(
        &mut self,
        requested: AppearanceSettings,
        cx: &mut Context<Self>,
    ) {
        let requested = requested.normalized().expect("测试外观配置必须有效");
        self.appearance = requested.clone();
        let ui_revision = next_save_revision(&mut self.preference_save_revisions.appearance);
        self.finish_appearance_write(
            ui_revision,
            requested,
            false,
            Err(ConfigWriteError::PersistenceUnavailable),
            cx,
        );
    }

    /// 返回已发现的模型家族与服装总数。
    pub(in crate::ui) fn catalog_counts_for_test(&self) -> (usize, usize) {
        self.catalog.counts()
    }

    pub(in crate::ui) fn global_model_selection_for_test(&self) -> Option<&Path> {
        self.global_model_selection.as_deref()
    }

    pub(in crate::ui) fn applied_global_model_selection_for_test(&self) -> Option<&Path> {
        self.applied.global_model_selection.as_deref()
    }

    pub(in crate::ui) fn applied_persona_model_for_test(&self) -> Option<&Path> {
        self.applied_persona_model.as_deref()
    }

    pub(in crate::ui) fn applied_persona_id_for_test(&self) -> Option<&str> {
        self.applied_persona_id.as_deref()
    }

    pub(in crate::ui) fn pending_model_runtime_for_test(&self) -> Option<&Path> {
        self.model_selection_write_state
            .pending
            .as_ref()
            .and_then(|pending| pending.runtime.as_ref())
            .and_then(|runtime| runtime.selection.as_deref())
    }

    /// 返回运行时模型清单，供模型绑定与全局选择隔离测试使用。
    pub(in crate::ui) fn runtime_model_selection_for_test(&self) -> Option<&Path> {
        self.catalog.selected_relative_path()
    }

    /// 返回设置窗口收到模型目录变化事件的次数与最近候选数量。
    pub(in crate::ui) fn persona_live2d_refresh_for_test(&self) -> (u64, usize) {
        (
            self.persona_live2d_refresh_revision,
            self.persona_live2d_candidate_count,
        )
    }

    /// 在不启动配置写任务的情况下准备全局模型选择测试状态。
    pub(in crate::ui) fn set_model_selections_for_test(
        &mut self,
        global: PathBuf,
        runtime: PathBuf,
        outfit: Option<&str>,
    ) {
        self.global_model_selection = Some(global.clone());
        self.applied.global_model_selection = Some(global);
        self.catalog
            .set_runtime_selection(Some(&runtime))
            .expect("测试运行时模型必须属于目录扫描结果");
        self.active_outfit = outfit.map(str::to_owned);
        let baseline = self.capture_model_selection_baseline();
        self.model_selection_write_state
            .synchronize_committed(baseline);
    }

    /// 准备一次全局模型写入，但不接触进程级配置，供异步结果测试使用。
    pub(in crate::ui) fn stage_global_model_selection_for_test(
        &mut self,
        relative_path: PathBuf,
        cx: &mut Context<Self>,
    ) -> u64 {
        self.stage_model_selection(Some(relative_path), cx)
    }

    /// 模拟 Agent 已取得真实模型路径并预应用 runtime，但不启动配置写任务。
    pub(in crate::ui) fn stage_preapplied_model_selection_for_test(
        &mut self,
        relative_path: PathBuf,
        cx: &mut Context<Self>,
    ) -> (u64, PathBuf) {
        let baseline = self.capture_model_selection_baseline();
        let model_path = self
            .catalog
            .select_variant(&relative_path)
            .expect("测试预应用模型必须属于目录扫描结果");
        self.active_outfit = None;
        let save_revision =
            self.stage_preapplied_model_selection(Some(relative_path), baseline, cx);
        (save_revision, model_path.path().to_path_buf())
    }

    /// 模拟无关模型目录状态变化，验证其不会使全局保存结果过期。
    pub(in crate::ui) fn invalidate_catalog_revision_for_test(&mut self) {
        self.catalog_revision = self.catalog_revision.wrapping_add(1);
    }

    /// 注入全局模型写入完成结果，供不写用户配置的回归测试使用。
    pub(in crate::ui) fn finish_global_model_selection_for_test(
        &mut self,
        save_revision: u64,
        requested_selection: Option<PathBuf>,
        persisted_selection: Option<PathBuf>,
        result: Result<Option<()>, ConfigWriteError>,
        cx: &mut Context<Self>,
    ) {
        self.finish_model_selection_write(
            save_revision,
            requested_selection,
            result,
            persisted_selection,
            cx,
        );
    }

    pub(in crate::ui) fn apply_published_persona_model_for_test(
        &mut self,
        global_selection: PathBuf,
        persona_id: &str,
        persona_binding: Option<&Path>,
        cx: &mut Context<Self>,
    ) {
        self.reconcile_published_persona_model_for_test(
            Some(global_selection),
            Some(persona_id.to_owned()),
            persona_binding,
            cx,
        );
    }

    pub(in crate::ui) fn finish_global_model_selection_with_persona_for_test(
        &mut self,
        save_revision: u64,
        requested_selection: Option<PathBuf>,
        persisted_selection: Option<PathBuf>,
        result: Result<Option<()>, ConfigWriteError>,
        persona_binding: (&str, Option<&Path>),
        cx: &mut Context<Self>,
    ) {
        self.finish_model_selection_write_with_persona(
            save_revision,
            requested_selection,
            result,
            persisted_selection,
            ActivePersonaModelBinding {
                persona_id: Some(persona_binding.0.to_owned()),
                relative_path: persona_binding.1.map(Path::to_path_buf),
            },
            cx,
        );
    }

    /// 注入扫描任务已经产出的目录，并明确给出完成时复核到的配置与人格。
    pub(in crate::ui) fn finish_model_scan_for_test(
        &mut self,
        catalog_revision: u64,
        catalog: ModelCatalog,
        committed_selection: Option<PathBuf>,
        persona_binding: (&str, Option<&Path>),
        cx: &mut Context<Self>,
    ) {
        self.finish_model_scan(
            catalog_revision,
            Ok(catalog),
            committed_selection,
            ActivePersonaModelBinding {
                persona_id: Some(persona_binding.0.to_owned()),
                relative_path: persona_binding.1.map(Path::to_path_buf),
            },
            cx,
        );
    }

    pub(in crate::ui) fn model_scan_revision_for_test(&self) -> u64 {
        self.catalog_revision
    }

    /// 解析测试提供的人格与全局绑定，不读取或修改进程级配置。
    pub(in crate::ui) fn resolve_persona_live2d_model_for_test(
        &self,
        bound: Option<&Path>,
        global: Option<&Path>,
    ) -> (Option<PathBuf>, Option<PathBuf>, bool) {
        let (relative, path, warning) = self.resolve_persona_live2d_model(bound, global);
        (
            relative,
            path.map(|manifest| manifest.path().to_path_buf()),
            warning.is_some(),
        )
    }

    /// 只切换测试实体中的换装工具状态，不写入用户配置。
    pub(in crate::ui) fn set_agent_outfit_tool_enabled_for_test(&mut self, enabled: bool) {
        self.allow_agent_outfit_change = enabled;
        self.applied.allow_agent_outfit_change = enabled;
    }

    /// 返回设置窗口是否已经创建输入组件。
    pub(in crate::ui) fn window_is_active_for_test(&self) -> bool {
        self.provider_settings_view.is_some()
            && self.persona_settings_view.is_some()
            && self.custom_frame_rate_input.is_some()
            && self.model_resource_name_input.is_some()
            && self.shortcut_focus.is_some()
    }

    /// 返回后台模型扫描是否仍在进行。
    pub(in crate::ui) fn is_refreshing_for_test(&self) -> bool {
        self.is_refreshing
    }

    /// 发起绑定到当前设置窗口的手动扫描。
    pub(in crate::ui) fn refresh_models_for_test(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.refresh_models(window, cx);
    }

    /// 返回当前主模型 generation 上报的可预览能力。
    pub(in crate::ui) fn preview_capabilities_for_test(&self) -> &ModelPreviewCapabilities {
        &self.preview_capabilities
    }

    /// 只修改测试实体中的资源显示名，不写入用户配置。
    pub(in crate::ui) fn set_model_resource_name_for_test(
        &mut self,
        kind: ModelResourceKind,
        runtime_id: &str,
        name: &str,
    ) {
        let key = if kind == ModelResourceKind::Variant {
            Self::variant_resource_key(Path::new(runtime_id))
        } else {
            self.selected_resource_key(kind, runtime_id)
                .expect("测试模型必须已经选择清单")
        };
        let settings = self
            .model_resources
            .with_name(key, Some(name))
            .expect("测试资源名称必须有效");
        let settings = Arc::new(settings);
        self.model_resources = settings.clone();
        self.applied.model_resources = settings;
    }

    /// 只修改测试实体中的根目录表达式分类，不写入用户配置。
    pub(in crate::ui) fn set_expression_category_for_test(
        &mut self,
        runtime_id: &str,
        category: ModelExpressionCategory,
    ) {
        let key = self
            .selected_resource_key(ModelResourceKind::Expression, runtime_id)
            .expect("测试模型必须已经选择清单");
        let settings = self
            .model_resources
            .with_expression_category(key, category)
            .expect("测试表达式分类必须有效");
        let settings = Arc::new(settings);
        self.model_resources = settings.clone();
        self.applied.model_resources = settings;
    }

    pub(in crate::ui) fn inject_model_resource_name_failure_for_test(
        &mut self,
        key: ModelResourceKey,
        name: &str,
        cx: &mut Context<Self>,
    ) {
        let requested = self
            .model_resources
            .with_name(key, Some(name))
            .expect("测试资源名称必须有效");
        self.model_resources = Arc::new(requested.clone());
        self.model_resource_save_revision =
            self.model_resource_save_revision.wrapping_add(1).max(1);
        let ui_revision = self.model_resource_save_revision;
        self.finish_model_resource_settings_write(
            ui_revision,
            requested,
            Err(ConfigWriteError::PersistenceUnavailable),
            cx,
        );
    }

    pub(in crate::ui) fn model_resource_names_for_test(
        &self,
        key: &ModelResourceKey,
    ) -> (Option<&str>, Option<&str>) {
        (
            self.model_resources.name(key),
            self.applied.model_resources.name(key),
        )
    }

    /// 切换到指定配置分区，使对应页面在下一帧参与渲染。
    pub(in crate::ui) fn select_section_for_test(
        &mut self,
        section: usize,
        cx: &mut Context<Self>,
    ) {
        let section = match section {
            0 => ConfigSection::Model,
            1 => ConfigSection::Provider,
            2 => ConfigSection::Persona,
            3 => ConfigSection::Shortcut,
            4 => ConfigSection::Tool,
            5 => ConfigSection::System,
            _ => ConfigSection::Debug,
        };
        self.set_section(section, cx);
    }

    /// 返回配置分区总数，供测试遍历全部页面。
    pub(in crate::ui) const fn section_count_for_test() -> usize {
        7
    }

    /// 使用明确的人格绑定状态提交测试换装，避免测试写入进程级配置。
    pub(in crate::ui) fn commit_agent_outfit_with_binding_for_test(
        &mut self,
        action: AgentOutfitAction,
        active_has_binding: bool,
        cx: &mut Context<Self>,
    ) -> Result<Option<PathBuf>, String> {
        self.commit_agent_outfit_with_binding(action, active_has_binding, cx)
            .map(|manifest| manifest.map(|manifest| manifest.path().to_path_buf()))
    }

    /// 准备已经应用的人格模型与表达式服装状态，供重复配置事件回归测试使用。
    pub(in crate::ui) fn set_applied_persona_model_for_test(
        &mut self,
        relative_path: PathBuf,
        outfit: &str,
    ) {
        self.global_model_selection = Some(relative_path.clone());
        self.applied.global_model_selection = Some(relative_path.clone());
        self.applied_persona_id = CONFIG
            .persona_settings()
            .active()
            .map(|persona| persona.id.clone());
        self.applied_persona_model = Some(relative_path.clone());
        self.catalog
            .set_runtime_selection(Some(&relative_path))
            .expect("测试模型必须属于目录扫描结果");
        self.active_outfit = Some(outfit.to_owned());
        let baseline = self.capture_model_selection_baseline();
        self.model_selection_write_state
            .synchronize_committed(baseline);
    }

    pub(in crate::ui) fn reapply_persona_model_for_test(&mut self, cx: &mut Context<Self>) {
        self.apply_active_persona_live2d_model(cx);
    }

    pub(in crate::ui) fn active_outfit_for_test(&self) -> Option<&str> {
        self.active_outfit.as_deref()
    }
}

fn settings_event_kind(event: &SettingsEvent) -> Option<SettingsEventKindForTest> {
    match event {
        SettingsEvent::ModelChanged(_) => Some(SettingsEventKindForTest::Model),
        SettingsEvent::ModelResourcesChanged => Some(SettingsEventKindForTest::ModelResources),
        SettingsEvent::ModelWindowSizeChanged(_) => Some(SettingsEventKindForTest::ModelWindowSize),
        SettingsEvent::EyeTrackingChanged(_) => Some(SettingsEventKindForTest::EyeTracking),
        SettingsEvent::NativeTrayMenuChanged(_) => Some(SettingsEventKindForTest::NativeTrayMenu),
        SettingsEvent::AppearanceChanged(_) => Some(SettingsEventKindForTest::Appearance),
        _ => None,
    }
}
