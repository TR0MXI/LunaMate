//! 保存设置视图核心状态，并通过职责子模块处理用户动作与热更新发布。

mod agent;
mod components;
mod lifecycle;
mod model_catalog;
mod model_page;
mod model_resources;
mod model_scan;
mod preferences;
mod render;
mod shortcut_page;
mod shortcuts;
mod system_page;
#[cfg(test)]
mod test_support;
mod tool_page;
mod window;

use std::{path::PathBuf, sync::Arc};

use gpui::{Entity, FocusHandle, Subscription, Task};
use gpui_component::input::InputState;
use lunamate_agent::Agent;
use rapidhash::RapidHashMap;

use crate::{
    config::{
        AppearanceSettings, FrameRate, LoggingSettings, ModelResourceKey, ModelWindowSize,
        SharedModelResourceSettings, ShortcutAction, ShortcutSettings, VoiceSettings,
    },
    model::{ModelCatalog, ModelManifest, ModelPreviewCapabilities},
};

use agent::InputEditSession;
pub(in crate::ui) use agent::{
    ContextMutationCompletion, PersonaSettingsDraft, PersonaSettingsEvent, PersonaSettingsView,
    ProviderSettingsDraft, ProviderSettingsEvent, ProviderSettingsView,
};

#[cfg(test)]
pub(in crate::ui) use agent::{
    MemoryScope, next_model_id_for_test, next_persona_id_for_test, non_empty_for_test,
    provider_display_name_for_test, provider_from_display_name_for_test, provider_icon_for_test,
    provider_option_index_for_test, reasoning_index_for_test, reasoning_option_count_for_test,
    tts_model_option_index_for_test,
};
pub(in crate::ui) use preferences::custom_frame_rate_seed;
#[cfg(test)]
pub(in crate::ui) use preferences::parse_custom_frame_rate;
#[cfg(test)]
pub(in crate::ui) use test_support::SettingsEventKindForTest;
pub(crate) use window::SettingsWindowView;

/// 设置界面向桌宠主视图发送的热更新事件。
#[derive(Clone, Debug)]
pub(crate) enum SettingsEvent {
    /// 当前模型或服装清单发生变化。
    ModelChanged(Option<ModelManifest>),
    /// 模型目录扫描结果已经替换，窗口绑定的编辑器应刷新候选项。
    ModelCatalogChanged,
    /// 渲染帧率已更新，后台调度器应尽快重新读取原子配置。
    FrameRateChanged,
    /// 眼部跟随开关已更新。
    EyeTrackingChanged(bool),
    /// 主窗口帧率显示开关已更新。
    ShowFpsChanged(bool),
    /// 托盘右键菜单是否强制回退为系统原生实现。
    NativeTrayMenuChanged(bool),
    /// 桌宠主窗口尺寸档位已更新。
    ModelWindowSizeChanged(ModelWindowSize),
    /// 请求主模型 generation 播放一个动作。
    PreviewMotion(String),
    /// 请求主模型 generation 应用一个表情或服装表达式。
    PreviewExpression(String),
    /// 请求主模型 generation 恢复模型清单中的默认表情。
    ResetExpression,
    /// 供应商或人格配置已经发布。
    AgentChanged,
    /// Agent 换装工具开关已更新，当前服装快照应立即发布或撤销。
    AgentOutfitToolChanged(bool),
    /// 模型资源显示名或表达式分类已经持久化发布。
    ModelResourcesChanged,
    /// 外观设置已经发布，所有窗口应刷新主题和语言。
    AppearanceChanged(AppearanceSettings),
    /// 本地语音配置已经持久化并发布。
    VoiceChanged(VoiceSettings),
    /// 全局快捷键配置已经持久化并发布。
    ShortcutsChanged(ShortcutSettings),
    /// 快捷键录入开始或结束，运行时应暂时释放或恢复系统注册。
    ShortcutRecordingChanged(bool),
    /// 已清除持久化位置，所有现存窗口应立即返回默认位置。
    WindowPositionsReset,
}

/// Agent 换装工具在当前模型状态下解析出的语义动作。
#[derive(Clone, Debug, Eq, PartialEq)]
pub(in crate::ui) enum AgentOutfitAction {
    Unchanged,
    LoadVariant(PathBuf),
    PreviewExpression(String),
    ResetExpression,
}

#[derive(Clone)]
enum AgentOutfitTarget {
    Variant(PathBuf),
    Expression(String),
}

struct AgentOutfitCandidate {
    id: String,
    label: String,
    target: AgentOutfitTarget,
}

/// 单次运行时协调采用的当前人格身份与 Live2D 绑定。
struct ActivePersonaModelBinding {
    persona_id: Option<String>,
    relative_path: Option<PathBuf>,
}

/// 当前配置与人格绑定解析出的已提交模型边界。
#[derive(Clone)]
struct ModelSelectionBaseline {
    runtime_selection: Option<PathBuf>,
    runtime_model_path: Option<ModelManifest>,
    global_selection: Option<PathBuf>,
    applied_persona_id: Option<String>,
    applied_persona_model: Option<PathBuf>,
    active_outfit: Option<String>,
}

/// 最新模型写入仍在 runtime 上保留的即时预应用。
struct PendingModelRuntime {
    selection: Option<PathBuf>,
}

/// 最新模型写入请求；旧回调无权取得或结束该请求。
struct PendingModelSelectionWrite {
    save_revision: u64,
    requested_selection: Option<PathBuf>,
    runtime: Option<PendingModelRuntime>,
}

/// 分离最新已提交基线与仍待完成的模型写入，避免迟到回调改写更新的 runtime。
struct ModelSelectionWriteState {
    next_revision: u64,
    committed: ModelSelectionBaseline,
    pending: Option<PendingModelSelectionWrite>,
}

/// 已经完成持久化并可供其他运行时实体消费的设置快照。
#[derive(Clone)]
struct AppliedSettings {
    frame_rate: FrameRate,
    model_window_size: ModelWindowSize,
    remember_window_positions: bool,
    eye_tracking: bool,
    show_fps: bool,
    use_native_tray_menu: bool,
    allow_agent_outfit_change: bool,
    appearance: AppearanceSettings,
    voice: VoiceSettings,
    shortcuts: ShortcutSettings,
    model_resources: SharedModelResourceSettings,
    global_model_selection: Option<PathBuf>,
}

#[derive(Default)]
struct PreferenceSaveRevisions {
    frame_rate: u64,
    model_window_size: u64,
    remember_window_positions: u64,
    eye_tracking: u64,
    show_fps: u64,
    use_native_tray_menu: u64,
    allow_agent_outfit_change: u64,
    logging: u64,
    appearance: u64,
    reset_window_positions: u64,
}

fn next_save_revision(revision: &mut u64) -> u64 {
    *revision = revision.wrapping_add(1).max(1);
    *revision
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct EditingModelResource {
    key: ModelResourceKey,
    default_name: String,
}

/// 设置页拖动根目录表达式时携带的稳定资源身份。
#[derive(Clone, Debug)]
pub(super) struct ModelExpressionDrag {
    manifest: PathBuf,
    runtime_id: String,
    capabilities_revision: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ConfigSection {
    Model,
    Provider,
    Persona,
    Shortcut,
    Tool,
    System,
    Debug,
}

struct RetiredProviderSettingsEditor {
    view: Entity<ProviderSettingsView>,
    _subscription: Subscription,
}

struct RetiredPersonaSettingsEditor {
    view: Entity<PersonaSettingsView>,
    _subscription: Subscription,
}

/// 独立设置窗口的主体状态。
pub(crate) struct SettingsView {
    catalog: ModelCatalog,
    agent: Arc<Agent>,
    provider_settings_view: Option<Entity<ProviderSettingsView>>,
    provider_settings_draft: Option<ProviderSettingsDraft>,
    persona_settings_view: Option<Entity<PersonaSettingsView>>,
    persona_settings_draft: Option<PersonaSettingsDraft>,
    custom_accent_input: Option<Entity<InputState>>,
    custom_background_input: Option<Entity<InputState>>,
    custom_frame_rate_input: Option<Entity<InputState>>,
    log_max_size_input: Option<Entity<InputState>>,
    log_keep_files_input: Option<Entity<InputState>>,
    model_resource_name_input: Option<Entity<InputState>>,
    input_edit: Option<InputEditSession>,
    shortcut_focus: Option<FocusHandle>,
    preview_capabilities: ModelPreviewCapabilities,
    model_resources: SharedModelResourceSettings,
    editing_model_resource: Option<EditingModelResource>,
    active_outfit: Option<String>,
    global_model_selection: Option<PathBuf>,
    applied_persona_id: Option<String>,
    applied_persona_model: Option<PathBuf>,
    section: ConfigSection,
    status: Option<String>,
    frame_rate: FrameRate,
    model_window_size: ModelWindowSize,
    remember_window_positions: bool,
    eye_tracking: bool,
    show_fps: bool,
    use_native_tray_menu: bool,
    allow_agent_screenshot: bool,
    allow_agent_outfit_change: bool,
    screenshot_permission_retry_required: bool,
    /// 设置控件中的可编辑草稿，可能领先于后台配置写入。
    logging: LoggingSettings,
    /// 最近一次确认写入配置域的值；不代表运行中 file writer 的启动策略。
    persisted_logging: LoggingSettings,
    appearance: AppearanceSettings,
    voice: VoiceSettings,
    shortcuts: ShortcutSettings,
    applied: AppliedSettings,
    shortcut_recording: Option<ShortcutAction>,
    shortcut_runtime_errors: Vec<String>,
    shortcut_runtime_bindings: RapidHashMap<ShortcutAction, String>,
    is_refreshing: bool,
    preference_save_revisions: PreferenceSaveRevisions,
    catalog_revision: u64,
    model_selection_write_state: ModelSelectionWriteState,
    refresh_task: Option<Task<()>>,
    refresh_window_scoped: bool,
    write_tasks: Vec<Task<()>>,
    provider_settings_subscription: Option<Subscription>,
    persona_settings_subscription: Option<Subscription>,
    retired_provider_settings_editors: Vec<RetiredProviderSettingsEditor>,
    retired_persona_settings_editors: Vec<RetiredPersonaSettingsEditor>,
    custom_frame_rate_subscription: Option<Subscription>,
    custom_frame_rate_input_revision: u64,
    custom_frame_rate_save_task: Option<Task<()>>,
    logging_input_subscriptions: Vec<Subscription>,
    appearance_input_subscriptions: Vec<Subscription>,
    model_resource_name_subscription: Option<Subscription>,
    shortcut_focus_subscription: Option<Subscription>,
    logging_input_revision: u64,
    logging_save_task: Option<Task<()>>,
    screenshot_permission_revision: u64,
    toast_revision: u64,
    toast_task: Option<Task<()>>,
    voice_save_revision: u64,
    shortcut_save_revision: u64,
    model_resource_save_revision: u64,
    capabilities_revision: u64,
    #[cfg(test)]
    persona_live2d_refresh_revision: u64,
    #[cfg(test)]
    persona_live2d_candidate_count: usize,
    #[cfg(test)]
    emitted_settings_events: Vec<SettingsEvent>,
}

impl gpui::EventEmitter<SettingsEvent> for SettingsView {}
