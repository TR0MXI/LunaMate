//! 保存设置视图状态，处理用户动作，并向桌宠主视图发布热更新事件。

mod agent;
mod components;
mod model_page;
mod render;
mod shortcut_page;
mod system_page;
mod tool_page;
mod window;

use std::{
    collections::HashSet,
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

use gpui::{
    AppContext, Context, Entity, EventEmitter, FocusHandle, KeyDownEvent, KeybindingKeystroke,
    Subscription, Task, Window,
};
use gpui_component::input::{InputEvent, InputState, MaskPattern};
use lunamate_agent::{Agent, chat_limits, tools::OutfitOption};
use rapidhash::RapidHashMap;
use rust_i18n::t;

use crate::{
    config::{
        AppLanguage, AppearanceSettings, CONFIG, CUSTOM_FRAME_RATE_MAX, CUSTOM_FRAME_RATE_MIN,
        ConfigWriteError, FrameRate, LOGGING_MAX_FILE_SIZE_MB, LOGGING_MAX_KEEP_FILES,
        LOGGING_MIN_FILE_SIZE_MB, LOGGING_MIN_KEEP_FILES, LoggingSettings, ModelExpressionCategory,
        ModelResourceKey, ModelResourceKind, ModelResourceSettings, ModelWindowSize,
        SharedModelResourceSettings, ShortcutAction, ShortcutSettings, ThemePreset, VoiceMode,
        VoiceSettings,
    },
    model::{
        ModelCatalog, ModelPreviewCapabilities, ModelPreviewExpression, ensure_model_directory,
    },
    shortcut::{ShortcutRuntimeBinding, shortcut_from_keybinding},
};

use super::{apply, apply_language};
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

const CUSTOM_FRAME_RATE_SAVE_DELAY: Duration = Duration::from_millis(250);
const LOGGING_SAVE_DELAY: Duration = Duration::from_millis(250);

pub(crate) use window::SettingsWindowView;

/// 设置界面向桌宠主视图发送的热更新事件。
#[derive(Clone, Debug)]
pub(crate) enum SettingsEvent {
    /// 当前模型或服装清单发生变化。
    ModelChanged(Option<PathBuf>),
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
    logging: LoggingSettings,
    appearance: AppearanceSettings,
    voice: VoiceSettings,
    shortcuts: ShortcutSettings,
    shortcut_recording: Option<ShortcutAction>,
    shortcut_runtime_errors: Vec<String>,
    shortcut_runtime_bindings: RapidHashMap<ShortcutAction, String>,
    is_refreshing: bool,
    revision: u64,
    catalog_revision: u64,
    global_model_save_revision: u64,
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
}

impl SettingsView {
    /// 使用启动阶段得到的模型目录和配置诊断创建界面。
    pub(crate) fn new(
        catalog: ModelCatalog,
        agent: Arc<Agent>,
        status: Option<String>,
        cx: &mut Context<Self>,
    ) -> Self {
        // 最后一个窗口关闭时实体可能先于 quit observer 释放；配置写任务必须脱离实体继续完成。
        cx.on_release(|this, _| {
            for task in std::mem::take(&mut this.write_tasks) {
                task.detach();
            }
        })
        .detach();
        let mut view = Self {
            catalog,
            agent,
            provider_settings_view: None,
            provider_settings_draft: None,
            persona_settings_view: None,
            persona_settings_draft: None,
            custom_accent_input: None,
            custom_background_input: None,
            custom_frame_rate_input: None,
            log_max_size_input: None,
            log_keep_files_input: None,
            model_resource_name_input: None,
            input_edit: None,
            shortcut_focus: None,
            preview_capabilities: ModelPreviewCapabilities::default(),
            model_resources: CONFIG.model_resource_settings(),
            editing_model_resource: None,
            active_outfit: None,
            global_model_selection: CONFIG.selected_model(),
            applied_persona_id: None,
            applied_persona_model: None,
            section: ConfigSection::Model,
            status: None,
            frame_rate: CONFIG.frame_rate(),
            model_window_size: CONFIG.model_window_size(),
            remember_window_positions: CONFIG.remember_window_positions(),
            eye_tracking: CONFIG.eye_tracking(),
            show_fps: CONFIG.show_fps(),
            use_native_tray_menu: CONFIG.use_native_tray_menu(),
            allow_agent_screenshot: CONFIG.allow_agent_screenshot(),
            allow_agent_outfit_change: CONFIG.allow_agent_outfit_change(),
            screenshot_permission_retry_required: CONFIG
                .agent_screenshot_permission_retry_required(),
            logging: *CONFIG.logging_settings(),
            appearance: CONFIG.appearance().as_ref().clone(),
            voice: CONFIG.voice_settings().as_ref().clone(),
            shortcuts: CONFIG.shortcut_settings().as_ref().clone(),
            shortcut_recording: None,
            shortcut_runtime_errors: Vec::new(),
            shortcut_runtime_bindings: RapidHashMap::default(),
            is_refreshing: false,
            revision: 0,
            catalog_revision: 0,
            global_model_save_revision: 0,
            refresh_task: None,
            refresh_window_scoped: false,
            write_tasks: Vec::new(),
            provider_settings_subscription: None,
            persona_settings_subscription: None,
            retired_provider_settings_editors: Vec::new(),
            retired_persona_settings_editors: Vec::new(),
            custom_frame_rate_subscription: None,
            custom_frame_rate_input_revision: 0,
            custom_frame_rate_save_task: None,
            logging_input_subscriptions: Vec::new(),
            appearance_input_subscriptions: Vec::new(),
            model_resource_name_subscription: None,
            shortcut_focus_subscription: None,
            logging_input_revision: 0,
            logging_save_task: None,
            screenshot_permission_revision: 0,
            toast_revision: 0,
            toast_task: None,
            voice_save_revision: 0,
            shortcut_save_revision: 0,
            model_resource_save_revision: 0,
            capabilities_revision: 0,
            #[cfg(test)]
            persona_live2d_refresh_revision: 0,
            #[cfg(test)]
            persona_live2d_candidate_count: 0,
        };
        if let Some(status) = status {
            view.set_status(status, cx);
        }
        view.start_pending_persona_cleanup(cx);
        view
    }

    /// 启动时幂等清理持久化 tombstone，避免必须再次打开设置窗口才删除旧记忆。
    fn start_pending_persona_cleanup(&mut self, cx: &mut Context<Self>) {
        let memory = self.agent.memory();
        for persona in CONFIG.persona_settings().pending_deletions.clone() {
            let memory = memory.clone();
            if !memory.claim_deleted_persona_cleanup(&persona) {
                continue;
            }
            let cleanup_memory = memory.clone();
            let cleanup_persona = persona.clone();
            let cleanup = gpui_tokio::Tokio::spawn(cx, async move {
                cleanup_memory
                    .cleanup_deleted_persona(&cleanup_persona)
                    .await
            });
            let task = cx.spawn(async move |this, cx| match cleanup.await {
                Ok(Ok(())) => {
                    memory.complete_deleted_persona_cleanup(&persona);
                    log::info!("启动时已完成待清理人格的幂等记忆删除");
                    let _ = this.update(cx, |this, cx| {
                        this.finish_deleted_persona_cleanup(persona, cx);
                    });
                }
                Ok(Err(error)) => {
                    memory.fail_deleted_persona_cleanup(&persona);
                    log::error!("启动时清理已删除人格失败：{error}");
                }
                Err(error) => {
                    memory.fail_deleted_persona_cleanup(&persona);
                    log::error!("启动时人格清理任务异常结束：{error}");
                }
            });
            self.write_tasks.push(task);
        }
    }

    fn finish_deleted_persona_cleanup(&mut self, _persona: String, cx: &mut Context<Self>) {
        let completed = self
            .agent
            .memory()
            .completed_deleted_persona_cleanups()
            .into_iter()
            .filter(|persona| {
                CONFIG
                    .persona_settings()
                    .pending_deletions
                    .contains(persona)
            })
            .collect::<Vec<_>>();
        if let Some(active) = self.persona_settings_view.clone() {
            active.update(cx, |active, cx| {
                for persona in &completed {
                    active.finish_persona_cleanup(persona, cx);
                }
            });
            self.release_published_persona_cleanups(cx);
            return;
        }

        let draft = self
            .persona_settings_draft
            .get_or_insert_with(PersonaSettingsDraft::current);
        let mut changed = false;
        for persona in &completed {
            changed |= draft.finish_persona_cleanup(persona);
        }
        if !changed {
            self.release_published_persona_cleanups(cx);
            return;
        }
        let Some(write) = draft.prepare_write() else {
            self.release_published_persona_cleanups(cx);
            return;
        };
        let background = cx.background_executor().clone();
        let task = cx.spawn(async move |this, cx| {
            let result = background.spawn(async move { write.persist() }).await;
            let _ = this.update(cx, |this, cx| match result {
                Ok(_) => this.release_published_persona_cleanups(cx),
                Err(error) => log::error!("移除已清理人格 tombstone 失败：{error}"),
            });
        });
        self.track_write_task(task);
    }

    fn release_published_persona_cleanups(&mut self, cx: &mut Context<Self>) {
        let pending = CONFIG.persona_settings().pending_deletions.clone();
        let published = self
            .agent
            .memory()
            .completed_deleted_persona_cleanups()
            .into_iter()
            .filter(|persona| !pending.contains(persona))
            .collect::<Vec<_>>();
        if published.is_empty() {
            return;
        }
        if let Some(active) = self.persona_settings_view.clone() {
            active.update(cx, |active, cx| {
                for persona in &published {
                    active.persona_cleanup_was_published(persona, cx);
                }
            });
        }
        if let Some(draft) = &mut self.persona_settings_draft {
            for persona in &published {
                draft.persona_cleanup_was_published(persona);
            }
        }
        let memory = self.agent.memory();
        for persona in published {
            memory.release_deleted_persona_cleanup(&persona);
        }
    }

    /// 设置窗口打开时创建输入组件，并把当前外观同步到全局主题。
    pub(crate) fn activate_window(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.allow_agent_screenshot = CONFIG.requested_allow_agent_screenshot();
        self.screenshot_permission_retry_required =
            CONFIG.agent_screenshot_permission_retry_required();
        self.appearance = CONFIG.appearance().as_ref().clone();
        apply_language(self.appearance.language);
        apply(&self.appearance, Some(window), cx);
        let shortcut_focus = cx.focus_handle();
        self.shortcut_focus_subscription =
            Some(cx.on_blur(&shortcut_focus, window, |this, _, cx| {
                this.stop_shortcut_recording(cx)
            }));
        self.shortcut_focus = Some(shortcut_focus);
        let draft = self
            .provider_settings_draft
            .take()
            .unwrap_or_else(ProviderSettingsDraft::current);
        let provider_settings_view = cx.new(|cx| ProviderSettingsView::new(draft, window, cx));
        self.activate_persona_settings(window, cx);
        let custom_accent_input = cx.new(|cx| {
            InputState::new(window, cx).default_value(self.appearance.custom.accent.clone())
        });
        let custom_background_input = cx.new(|cx| {
            InputState::new(window, cx).default_value(self.appearance.custom.background.clone())
        });
        self.appearance_input_subscriptions =
            [custom_accent_input.clone(), custom_background_input.clone()]
                .into_iter()
                .map(|appearance_input| {
                    cx.subscribe_in(
                        &appearance_input,
                        window,
                        |this, input, event: &InputEvent, window, cx| match event {
                            InputEvent::Focus => this.begin_input_edit(input, cx),
                            InputEvent::PressEnter { .. } => {
                                this.apply_custom_theme(window, cx);
                                window.blur();
                            }
                            InputEvent::Blur => this.finish_input_edit(input),
                            InputEvent::Change => {}
                        },
                    )
                })
                .collect();
        self.custom_accent_input = Some(custom_accent_input);
        self.custom_background_input = Some(custom_background_input);
        let custom_frame_rate = custom_frame_rate_seed(self.frame_rate);
        let custom_frame_rate_input = cx.new(|cx| {
            InputState::new(window, cx)
                .default_value(custom_frame_rate.to_string())
                .mask_pattern(MaskPattern::Number {
                    separator: None,
                    fraction: Some(0),
                })
                .step(1.0)
                .min(f64::from(CUSTOM_FRAME_RATE_MIN))
                .max(f64::from(CUSTOM_FRAME_RATE_MAX))
        });
        self.custom_frame_rate_subscription = Some(cx.subscribe_in(
            &custom_frame_rate_input,
            window,
            |this, input, event: &InputEvent, window, cx| match event {
                InputEvent::Change => this.schedule_custom_frame_rate_save(input, cx),
                InputEvent::PressEnter { .. } => {
                    this.commit_custom_frame_rate_input(input, window, cx);
                    window.blur();
                }
                InputEvent::Blur => {
                    this.finish_input_edit(input);
                    this.commit_custom_frame_rate_input(input, window, cx);
                }
                InputEvent::Focus => this.begin_input_edit(input, cx),
            },
        ));
        self.custom_frame_rate_input = Some(custom_frame_rate_input);
        let log_max_size_input = cx.new(|cx| {
            InputState::new(window, cx)
                .default_value(self.logging.max_size_mb.to_string())
                .mask_pattern(MaskPattern::Number {
                    separator: None,
                    fraction: Some(0),
                })
                .step(1.0)
                .min(f64::from(LOGGING_MIN_FILE_SIZE_MB))
                .max(f64::from(LOGGING_MAX_FILE_SIZE_MB))
        });
        let log_keep_files_input = cx.new(|cx| {
            InputState::new(window, cx)
                .default_value(self.logging.keep_files.to_string())
                .mask_pattern(MaskPattern::Number {
                    separator: None,
                    fraction: Some(0),
                })
                .step(1.0)
                .min(f64::from(LOGGING_MIN_KEEP_FILES))
                .max(f64::from(LOGGING_MAX_KEEP_FILES))
        });
        self.logging_input_subscriptions = vec![
            cx.subscribe_in(
                &log_max_size_input,
                window,
                |this, input, event: &InputEvent, window, cx| match event {
                    InputEvent::Change => {
                        this.schedule_logging_save(input, Self::set_log_max_size_from_input, cx);
                    }
                    InputEvent::PressEnter { .. } => {
                        this.commit_logging_input(input, Self::set_log_max_size_from_input, cx);
                        window.blur();
                    }
                    InputEvent::Blur => {
                        this.finish_input_edit(input);
                        this.commit_logging_input(input, Self::set_log_max_size_from_input, cx);
                    }
                    InputEvent::Focus => this.begin_input_edit(input, cx),
                },
            ),
            cx.subscribe_in(
                &log_keep_files_input,
                window,
                |this, input, event: &InputEvent, window, cx| match event {
                    InputEvent::Change => {
                        this.schedule_logging_save(input, Self::set_log_keep_files_from_input, cx);
                    }
                    InputEvent::PressEnter { .. } => {
                        this.commit_logging_input(input, Self::set_log_keep_files_from_input, cx);
                        window.blur();
                    }
                    InputEvent::Blur => {
                        this.finish_input_edit(input);
                        this.commit_logging_input(input, Self::set_log_keep_files_from_input, cx);
                    }
                    InputEvent::Focus => this.begin_input_edit(input, cx),
                },
            ),
        ];
        self.log_max_size_input = Some(log_max_size_input);
        self.log_keep_files_input = Some(log_keep_files_input);
        let model_resource_name_input = cx.new(|cx| InputState::new(window, cx));
        self.model_resource_name_subscription = Some(cx.subscribe_in(
            &model_resource_name_input,
            window,
            |this, input, event: &InputEvent, window, cx| match event {
                InputEvent::Focus => this.begin_input_edit(input, cx),
                InputEvent::PressEnter { .. } => {
                    this.commit_model_resource_name(input, cx);
                    window.blur();
                }
                InputEvent::Blur => {
                    this.finish_input_edit(input);
                    this.commit_model_resource_name(input, cx);
                }
                InputEvent::Change => {}
            },
        ));
        self.model_resource_name_input = Some(model_resource_name_input);
        // 供应商目录变化会改变人格可绑定的候选项，两个编辑器必须保持同步。
        self.provider_settings_subscription = Some(cx.subscribe(
            &provider_settings_view,
            |this, editor, event: &ProviderSettingsEvent, cx| match event {
                ProviderSettingsEvent::Saved => {
                    cx.emit(SettingsEvent::AgentChanged);
                }
                ProviderSettingsEvent::SaveFinished => {
                    let editor_id = editor.entity_id();
                    this.retired_provider_settings_editors
                        .retain(|retired| retired.view.entity_id() != editor_id);
                }
            },
        ));
        self.provider_settings_view = Some(provider_settings_view);
        cx.notify();
    }

    fn begin_input_edit(&mut self, input: &Entity<InputState>, cx: &Context<Self>) {
        self.input_edit = Some(InputEditSession::begin(input, cx));
    }

    fn finish_input_edit(&mut self, input: &Entity<InputState>) {
        if self
            .input_edit
            .as_ref()
            .is_some_and(|edit| edit.belongs_to(input))
        {
            self.input_edit = None;
        }
    }

    fn handle_input_key_down(
        &mut self,
        event: &KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !event.keystroke.key.eq_ignore_ascii_case("escape") {
            return;
        }
        let Some(edit) = self.input_edit.take() else {
            return;
        };
        window.prevent_default();
        cx.stop_propagation();
        edit.restore(window, cx);
    }

    fn activate_persona_settings(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.write_tasks.retain(|task| !task.is_ready());
        let draft = self
            .persona_settings_draft
            .take()
            .unwrap_or_else(PersonaSettingsDraft::current);
        let memory = self.agent.memory();
        let live2d_models = self.persona_live2d_models();
        let view = cx.new(|cx| PersonaSettingsView::new(draft, memory, live2d_models, window, cx));
        self.persona_settings_subscription = Some(cx.subscribe(
            &view,
            |this, editor, event: &PersonaSettingsEvent, cx| match event {
                PersonaSettingsEvent::Saved => {
                    cx.emit(SettingsEvent::AgentChanged);
                    this.apply_active_persona_live2d_model(cx);
                }
                PersonaSettingsEvent::SaveFinished => {
                    let editor_id = editor.entity_id();
                    this.retired_persona_settings_editors
                        .retain(|retired| retired.view.entity_id() != editor_id);
                }
                PersonaSettingsEvent::CleanupFinished { persona } => {
                    this.finish_deleted_persona_cleanup(persona.clone(), cx);
                }
                PersonaSettingsEvent::ClearContext {
                    persona,
                    completion,
                } => this.clear_agent_context(persona, completion.clone(), cx),
                PersonaSettingsEvent::EditContextMessage {
                    persona,
                    message_id,
                    content,
                    completion,
                } => this.edit_agent_context_message(
                    persona,
                    *message_id,
                    content.clone(),
                    completion.clone(),
                    cx,
                ),
                PersonaSettingsEvent::DeleteContextMessages {
                    persona,
                    message_ids,
                    completion,
                } => this.delete_agent_context_messages(
                    persona,
                    message_ids.clone(),
                    completion.clone(),
                    cx,
                ),
            },
        ));
        self.persona_settings_view = Some(view.clone());
        let completed = CONFIG
            .persona_settings()
            .pending_deletions
            .iter()
            .filter(|persona| {
                self.agent
                    .memory()
                    .deleted_persona_cleanup_is_completed(persona)
            })
            .cloned()
            .collect::<Vec<_>>();
        view.update(cx, |view, cx| {
            for persona in completed {
                view.finish_persona_cleanup(&persona, cx);
            }
            view.resume_pending_work(cx);
        });
        self.release_published_persona_cleanups(cx);
    }

    fn clear_agent_context(
        &self,
        persona: &str,
        completion: Option<ContextMutationCompletion>,
        cx: &Context<Self>,
    ) {
        let agent = self.agent.clone();
        let persona = persona.to_owned();
        gpui_tokio::Tokio::spawn(cx, async move {
            let result = agent
                .clear_context(&persona)
                .await
                .map_err(|error| error.to_string());
            complete_agent_context_mutation(completion.as_ref(), result);
        })
        .detach();
    }

    fn edit_agent_context_message(
        &self,
        persona: &str,
        message_id: u64,
        content: String,
        completion: Option<ContextMutationCompletion>,
        cx: &Context<Self>,
    ) {
        let Some(limits) = agent_context_limits(persona) else {
            complete_agent_context_mutation(completion.as_ref(), Err("人格不存在".to_owned()));
            return;
        };
        let agent = self.agent.clone();
        let persona = persona.to_owned();
        gpui_tokio::Tokio::spawn(cx, async move {
            let result = agent
                .edit_context_message(&persona, limits, message_id, content)
                .await
                .map_err(|error| error.to_string());
            complete_agent_context_mutation(completion.as_ref(), result);
        })
        .detach();
    }

    fn delete_agent_context_messages(
        &self,
        persona: &str,
        message_ids: Vec<u64>,
        completion: Option<ContextMutationCompletion>,
        cx: &Context<Self>,
    ) {
        let Some(limits) = agent_context_limits(persona) else {
            complete_agent_context_mutation(completion.as_ref(), Err("人格不存在".to_owned()));
            return;
        };
        let agent = self.agent.clone();
        let persona = persona.to_owned();
        gpui_tokio::Tokio::spawn(cx, async move {
            let result = agent
                .delete_context_messages(&persona, limits, message_ids)
                .await
                .map_err(|error| error.to_string());
            complete_agent_context_mutation(completion.as_ref(), result);
        })
        .detach();
    }

    fn persona_live2d_models(&self) -> Vec<(String, PathBuf)> {
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

    fn refresh_persona_live2d_models(&mut self, window: &mut Window, cx: &mut Context<Self>) {
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

    fn global_live2d_fallback(
        &self,
        configured: Option<&Path>,
    ) -> (Option<PathBuf>, Option<PathBuf>) {
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

    fn resolve_persona_live2d_model(
        &self,
        bound: Option<&Path>,
        global: Option<&Path>,
    ) -> (Option<PathBuf>, Option<PathBuf>, Option<String>) {
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

    fn active_persona_live2d_model(&self) -> (Option<PathBuf>, Option<PathBuf>, Option<String>) {
        let personas = CONFIG.persona_settings();
        self.resolve_persona_live2d_model(
            personas
                .active()
                .and_then(|persona| persona.live2d_model.as_deref()),
            self.global_model_selection.as_deref(),
        )
    }

    fn apply_active_persona_live2d_model(&mut self, cx: &mut Context<Self>) {
        self.apply_active_persona_live2d_model_inner(false, cx);
    }

    fn apply_active_persona_live2d_model_after_scan(&mut self, cx: &mut Context<Self>) {
        self.apply_active_persona_live2d_model_inner(true, cx);
    }

    fn apply_active_persona_live2d_model_inner(
        &mut self,
        force_runtime_selection: bool,
        cx: &mut Context<Self>,
    ) {
        let persona_id = CONFIG
            .persona_settings()
            .active()
            .map(|persona| persona.id.clone());
        let (relative, model_path, warning) = self.active_persona_live2d_model();
        let persona_changed = self.applied_persona_id != persona_id;
        let configured_model_changed = self.applied_persona_model != relative;
        if !force_runtime_selection && !persona_changed && !configured_model_changed {
            if let Some(warning) = warning {
                self.set_status(warning, cx);
            }
            return;
        }
        let runtime_changed = self.catalog.selected_relative_path() != relative.as_deref();
        if (force_runtime_selection || runtime_changed)
            && let Err(error) = self.catalog.set_runtime_selection(relative.as_deref())
        {
            self.set_status(
                t!("status.model_action_failed", error = error.to_string()).to_string(),
                cx,
            );
            return;
        }
        self.applied_persona_id = persona_id;
        self.applied_persona_model = relative;
        if runtime_changed || configured_model_changed || force_runtime_selection {
            self.active_outfit = None;
            cx.emit(SettingsEvent::ModelChanged(model_path));
            cx.notify();
        } else if persona_changed || configured_model_changed {
            if self.active_outfit.take().is_some() {
                cx.emit(SettingsEvent::ResetExpression);
            }
            cx.notify();
        }
        if let Some(warning) = warning {
            self.set_status(warning, cx);
        }
    }

    /// 设置窗口关闭时丢弃绑定到旧窗口的输入状态。
    pub(crate) fn deactivate_window(&mut self, cx: &mut Context<Self>) {
        self.stop_shortcut_recording(cx);
        if self.is_refreshing && self.refresh_window_scoped {
            self.catalog_revision = self.catalog_revision.wrapping_add(1);
            self.refresh_task = None;
            self.is_refreshing = false;
            self.refresh_window_scoped = false;
        }
        if let Some(provider_settings_view) = self.provider_settings_view.take() {
            let (draft, pending) =
                provider_settings_view.update(cx, |view, cx| view.take_window_state(cx));
            self.provider_settings_draft = Some(draft);
            let has_pending = pending.iter().any(|task| !task.is_ready());
            let subscription = self.provider_settings_subscription.take();
            if has_pending && let Some(subscription) = subscription {
                self.retired_provider_settings_editors
                    .push(RetiredProviderSettingsEditor {
                        view: provider_settings_view,
                        _subscription: subscription,
                    });
            }
            self.write_tasks.extend(pending);
        }
        if let Some(persona_settings_view) = self.persona_settings_view.take() {
            let (draft, pending, retain_editor) =
                persona_settings_view.update(cx, |view, cx| view.take_window_state(cx));
            self.persona_settings_draft = Some(draft);
            let has_pending = retain_editor && pending.iter().any(|task| !task.is_ready());
            let subscription = self.persona_settings_subscription.take();
            if has_pending && let Some(subscription) = subscription {
                self.retired_persona_settings_editors
                    .push(RetiredPersonaSettingsEditor {
                        view: persona_settings_view,
                        _subscription: subscription,
                    });
            }
            self.write_tasks.extend(pending);
        }
        self.flush_custom_frame_rate_input(cx);
        self.flush_logging_inputs(cx);
        if let Some(input) = self.model_resource_name_input.clone() {
            self.commit_model_resource_name(&input, cx);
        }
        self.custom_accent_input = None;
        self.custom_background_input = None;
        self.custom_frame_rate_input = None;
        self.custom_frame_rate_save_task = None;
        self.log_max_size_input = None;
        self.log_keep_files_input = None;
        self.model_resource_name_input = None;
        self.input_edit = None;
        self.model_resource_name_subscription = None;
        self.editing_model_resource = None;
        self.shortcut_focus = None;
        self.shortcut_focus_subscription = None;
        self.provider_settings_subscription = None;
        self.persona_settings_subscription = None;
        self.custom_frame_rate_subscription = None;
        self.logging_input_subscriptions.clear();
        self.appearance_input_subscriptions.clear();
        cx.notify();
    }

    /// 当前设置窗口接到 Agent 配置发布事件后刷新人格可绑定的 Provider 候选。
    pub(crate) fn refresh_persona_providers(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(persona) = &self.persona_settings_view {
            persona.update(cx, |persona, cx| persona.refresh_providers(window, cx));
        }
    }

    /// 取出设置主体、供应商与人格编辑器中尚未完成的写入任务。
    pub(crate) fn take_pending_write_tasks(&mut self, cx: &mut Context<Self>) -> Vec<Task<()>> {
        self.flush_custom_frame_rate_input(cx);
        self.flush_logging_inputs(cx);
        if let Some(input) = self.model_resource_name_input.clone() {
            self.commit_model_resource_name(&input, cx);
        }
        if let Some(provider_settings_view) = &self.provider_settings_view {
            let provider_settings_view = provider_settings_view.clone();
            let (draft, pending) =
                provider_settings_view.update(cx, |view, cx| view.take_window_state(cx));
            self.provider_settings_draft = Some(draft);
            self.write_tasks.extend(pending);
        }
        if let Some(persona_settings_view) = &self.persona_settings_view {
            let persona_settings_view = persona_settings_view.clone();
            let (draft, pending, _) =
                persona_settings_view.update(cx, |view, cx| view.take_window_state(cx));
            self.persona_settings_draft = Some(draft);
            self.write_tasks.extend(pending);
        }
        if let Some(write) = self
            .persona_settings_draft
            .as_ref()
            .and_then(PersonaSettingsDraft::prepare_write)
        {
            let background = cx.background_executor().clone();
            let task = cx.spawn(async move |_, _| {
                let result = background.spawn(async move { write.persist() }).await;
                if let Err(error) = result {
                    log::error!("退出前保存人格草稿失败：{error}");
                }
            });
            self.write_tasks.push(task);
        }
        std::mem::take(&mut self.write_tasks)
    }

    fn track_write_task(&mut self, task: Task<()>) {
        self.write_tasks.retain(|task| !task.is_ready());
        self.write_tasks.push(task);
    }

    fn set_section(&mut self, section: ConfigSection, cx: &mut Context<Self>) {
        if self.section == section {
            return;
        }
        if self.section == ConfigSection::Shortcut {
            self.stop_shortcut_recording(cx);
        }
        self.section = section;
        cx.notify();
    }

    fn begin_shortcut_recording(
        &mut self,
        action: ShortcutAction,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let starting = self.shortcut_recording.is_none();
        self.shortcut_recording = Some(action);
        if let Some(focus) = &self.shortcut_focus {
            focus.focus(window, cx);
        }
        if starting {
            cx.emit(SettingsEvent::ShortcutRecordingChanged(true));
        }
        cx.notify();
    }

    fn stop_shortcut_recording(&mut self, cx: &mut Context<Self>) {
        if self.shortcut_recording.take().is_some() {
            cx.emit(SettingsEvent::ShortcutRecordingChanged(false));
            cx.notify();
        }
    }

    fn handle_shortcut_key_down(&mut self, event: &KeyDownEvent, cx: &mut Context<Self>) {
        let Some(action) = self.shortcut_recording else {
            return;
        };
        if event.is_held {
            return;
        }
        if event.keystroke.key.eq_ignore_ascii_case("escape") {
            self.commit_shortcut(action, None, cx);
            return;
        }
        let keystroke = KeybindingKeystroke::new_with_mapper(
            event.keystroke.clone(),
            false,
            cx.keyboard_mapper().as_ref(),
        );
        match shortcut_from_keybinding(&keystroke) {
            Ok(Some(shortcut)) => self.commit_shortcut(action, Some(shortcut), cx),
            Ok(None) => {}
            Err(error) => self.set_status(error, cx),
        }
    }

    fn commit_shortcut(
        &mut self,
        action: ShortcutAction,
        shortcut: Option<crate::config::KeyboardShortcut>,
        cx: &mut Context<Self>,
    ) {
        self.stop_shortcut_recording(cx);
        let mut settings = self.shortcuts.clone();
        settings.assign(action, shortcut);
        if settings == self.shortcuts {
            return;
        }
        self.shortcuts = settings.clone();
        self.shortcut_save_revision = self.shortcut_save_revision.wrapping_add(1).max(1);
        let ui_revision = self.shortcut_save_revision;
        let config_revision = CONFIG.reserve_shortcut_settings_revision();
        let background = cx.background_executor().clone();
        let task = cx.spawn(async move |this, cx| {
            let result = background
                .spawn(async move {
                    CONFIG.set_shortcut_settings_at_revision(settings, config_revision)
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                if let Ok(Some(settings)) = &result {
                    let current = CONFIG.shortcut_settings();
                    if current.as_ref() == settings.as_ref() {
                        cx.emit(SettingsEvent::ShortcutsChanged(current.as_ref().clone()));
                    }
                }
                if this.shortcut_save_revision != ui_revision {
                    return;
                }
                match result {
                    Ok(Some(settings)) => {
                        this.shortcuts = settings.as_ref().clone();
                        this.set_status(t!("shortcut.saved").to_string(), cx);
                    }
                    Ok(None) => {}
                    Err(error) => {
                        this.shortcuts = CONFIG.shortcut_settings().as_ref().clone();
                        this.set_status(
                            t!("shortcut.save_failed", error = error.to_string()).to_string(),
                            cx,
                        );
                    }
                }
            });
        });
        self.track_write_task(task);
        cx.notify();
    }

    /// 把系统注册失败反馈到仍可复用的设置实体。
    pub(crate) fn report_shortcut_runtime_errors(
        &mut self,
        errors: Vec<String>,
        cx: &mut Context<Self>,
    ) {
        self.shortcut_runtime_errors = errors;
        let message = (!self.shortcut_runtime_errors.is_empty()).then(|| {
            t!(
                "shortcut.registration_failed",
                error = self
                    .shortcut_runtime_errors
                    .join(t!("common.status_separator").as_ref())
            )
            .to_string()
        });
        if let Some(message) = message {
            self.set_status(message, cx);
        } else {
            cx.notify();
        }
    }

    /// 显示 Wayland 合成器实际确认的触发方式，而不是 preferred trigger。
    pub(crate) fn report_shortcut_runtime_bindings(
        &mut self,
        bindings: Vec<ShortcutRuntimeBinding>,
        cx: &mut Context<Self>,
    ) {
        self.shortcut_runtime_bindings.clear();
        self.shortcut_runtime_bindings.extend(
            bindings
                .into_iter()
                .filter(|binding| !binding.trigger_description().is_empty())
                .map(|binding| (binding.action(), binding.trigger_description().to_owned())),
        );
        cx.notify();
    }

    fn persist_setting(
        &mut self,
        revision: u64,
        write: impl FnOnce() -> Result<Option<()>, ConfigWriteError> + Send + 'static,
        cx: &mut Context<Self>,
    ) {
        let background = cx.background_executor().clone();
        let task = cx.spawn(async move |this, cx| {
            let result = background.spawn(async move { write() }).await;
            let _ = this.update(cx, |this, cx| {
                if this.revision != revision {
                    return;
                }
                if let Err(error) = result {
                    this.set_status(
                        t!("status.setting_failed", error = error.to_string()).to_string(),
                        cx,
                    );
                } else {
                    cx.notify();
                }
            });
        });
        self.track_write_task(task);
    }

    /// 返回当前 toast 状态文本，供测试断言扫描与失败提示。
    #[cfg(test)]
    pub(in crate::ui) fn status_for_test(&self) -> Option<&str> {
        self.status.as_deref()
    }

    /// 注入尚未发布的外观草稿，验证窗口激活只采用已发布配置。
    #[cfg(test)]
    pub(in crate::ui) fn set_appearance_language_for_test(&mut self, language: AppLanguage) {
        self.appearance.language = language;
    }

    #[cfg(test)]
    pub(in crate::ui) fn appearance_language_for_test(&self) -> AppLanguage {
        self.appearance.language
    }

    /// 返回已发现的模型家族与服装总数。
    #[cfg(test)]
    pub(in crate::ui) fn catalog_counts_for_test(&self) -> (usize, usize) {
        self.catalog.counts()
    }

    #[cfg(test)]
    pub(in crate::ui) fn global_model_selection_for_test(&self) -> Option<&Path> {
        self.global_model_selection.as_deref()
    }

    /// 返回运行时模型清单，供模型绑定与全局选择隔离测试使用。
    #[cfg(test)]
    pub(in crate::ui) fn runtime_model_selection_for_test(&self) -> Option<&Path> {
        self.catalog.selected_relative_path()
    }

    /// 返回设置窗口收到模型目录变化事件的次数与最近候选数量。
    #[cfg(test)]
    pub(in crate::ui) fn persona_live2d_refresh_for_test(&self) -> (u64, usize) {
        (
            self.persona_live2d_refresh_revision,
            self.persona_live2d_candidate_count,
        )
    }

    /// 在不启动配置写任务的情况下准备全局模型选择测试状态。
    #[cfg(test)]
    pub(in crate::ui) fn set_model_selections_for_test(
        &mut self,
        global: PathBuf,
        runtime: PathBuf,
        outfit: Option<&str>,
    ) {
        self.global_model_selection = Some(global);
        self.catalog
            .set_runtime_selection(Some(&runtime))
            .expect("测试运行时模型必须属于目录扫描结果");
        self.active_outfit = outfit.map(str::to_owned);
    }

    /// 准备一次全局模型写入，但不接触进程级配置，供异步结果测试使用。
    #[cfg(test)]
    pub(in crate::ui) fn stage_global_model_selection_for_test(
        &mut self,
        relative_path: PathBuf,
        cx: &mut Context<Self>,
    ) -> u64 {
        self.stage_model_selection(Some(relative_path), cx)
    }

    /// 模拟无关模型目录状态变化，验证其不会使全局保存结果过期。
    #[cfg(test)]
    pub(in crate::ui) fn invalidate_catalog_revision_for_test(&mut self) {
        self.catalog_revision = self.catalog_revision.wrapping_add(1);
    }

    /// 注入全局模型写入完成结果，供不写用户配置的回归测试使用。
    #[cfg(test)]
    pub(in crate::ui) fn finish_global_model_selection_for_test(
        &mut self,
        save_revision: u64,
        persisted_selection: Option<PathBuf>,
        result: Result<Option<()>, ConfigWriteError>,
        cx: &mut Context<Self>,
    ) {
        self.finish_model_selection_write(save_revision, result, persisted_selection, cx);
    }

    /// 解析测试提供的人格与全局绑定，不读取或修改进程级配置。
    #[cfg(test)]
    pub(in crate::ui) fn resolve_persona_live2d_model_for_test(
        &self,
        bound: Option<&Path>,
        global: Option<&Path>,
    ) -> (Option<PathBuf>, Option<PathBuf>, bool) {
        let (relative, path, warning) = self.resolve_persona_live2d_model(bound, global);
        (relative, path, warning.is_some())
    }

    /// 只切换测试实体中的换装工具状态，不写入用户配置。
    #[cfg(test)]
    pub(in crate::ui) fn set_agent_outfit_tool_enabled_for_test(&mut self, enabled: bool) {
        self.allow_agent_outfit_change = enabled;
    }

    /// 返回设置窗口是否已经创建输入组件。
    #[cfg(test)]
    pub(in crate::ui) fn window_is_active_for_test(&self) -> bool {
        self.provider_settings_view.is_some()
            && self.persona_settings_view.is_some()
            && self.custom_frame_rate_input.is_some()
            && self.model_resource_name_input.is_some()
            && self.shortcut_focus.is_some()
    }

    /// 返回后台模型扫描是否仍在进行。
    #[cfg(test)]
    pub(in crate::ui) fn is_refreshing_for_test(&self) -> bool {
        self.is_refreshing
    }

    /// 发起绑定到当前设置窗口的手动扫描。
    #[cfg(test)]
    pub(in crate::ui) fn refresh_models_for_test(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.refresh_models(window, cx);
    }

    /// 返回当前主模型 generation 上报的可预览能力。
    #[cfg(test)]
    pub(in crate::ui) fn preview_capabilities_for_test(&self) -> &ModelPreviewCapabilities {
        &self.preview_capabilities
    }

    /// 只修改测试实体中的资源显示名，不写入用户配置。
    #[cfg(test)]
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
        self.model_resources = Arc::new(settings);
    }

    /// 只修改测试实体中的根目录表达式分类，不写入用户配置。
    #[cfg(test)]
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
        self.model_resources = Arc::new(settings);
    }

    /// 切换到指定配置分区，使对应页面在下一帧参与渲染。
    #[cfg(test)]
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
    #[cfg(test)]
    pub(in crate::ui) const fn section_count_for_test() -> usize {
        7
    }

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

    fn variant_resource_key(relative_path: &Path) -> ModelResourceKey {
        ModelResourceKey::new(
            relative_path,
            ModelResourceKind::Variant,
            relative_path.to_string_lossy().into_owned(),
        )
    }

    fn selected_resource_key(
        &self,
        kind: ModelResourceKind,
        runtime_id: &str,
    ) -> Option<ModelResourceKey> {
        self.catalog
            .selected_relative_path()
            .map(|manifest| ModelResourceKey::new(manifest, kind, runtime_id))
    }

    fn model_resource_name(&self, key: &ModelResourceKey, default_name: &str) -> String {
        self.model_resources
            .name(key)
            .unwrap_or(default_name)
            .to_owned()
    }

    fn model_resource_is_renamed(&self, key: &ModelResourceKey) -> bool {
        self.model_resources.name(key).is_some()
    }

    fn expression_category(&self, expression: &ModelPreviewExpression) -> ModelExpressionCategory {
        if !expression.movable_to_outfit() {
            return ModelExpressionCategory::Expression;
        }
        self.selected_resource_key(
            ModelResourceKind::Expression,
            expression.resource().runtime_id(),
        )
        .map(|key| self.model_resources.expression_category(&key))
        .unwrap_or_default()
    }

    fn begin_model_resource_rename(
        &mut self,
        key: ModelResourceKey,
        default_name: String,
        current_name: String,
        window: &mut Window,
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

    fn commit_model_resource_name(&mut self, input: &Entity<InputState>, cx: &mut Context<Self>) {
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

    fn reset_model_resource_name(&mut self, key: ModelResourceKey, cx: &mut Context<Self>) {
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

    fn expression_drag(&self, expression: &ModelPreviewExpression) -> Option<ModelExpressionDrag> {
        let manifest = self.catalog.selected_relative_path()?.to_path_buf();
        expression.movable_to_outfit().then(|| ModelExpressionDrag {
            manifest,
            runtime_id: expression.resource().runtime_id().to_owned(),
            capabilities_revision: self.capabilities_revision,
        })
    }

    fn move_expression_to_category(
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
            Ok(settings) => {
                if category == ModelExpressionCategory::Expression
                    && self.active_outfit.as_deref() == Some(drag.runtime_id.as_str())
                {
                    self.active_outfit = None;
                }
                self.persist_model_resource_settings(settings, cx);
            }
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
                if this.model_resource_save_revision != ui_revision {
                    return;
                }
                match result {
                    Ok(Some(settings)) => {
                        this.model_resources = settings;
                        cx.emit(SettingsEvent::ModelResourcesChanged);
                        this.set_status(t!("status.model_resource_saved").to_string(), cx);
                    }
                    Ok(None) => {
                        this.model_resources = CONFIG.model_resource_settings();
                        cx.notify();
                    }
                    Err(error) => {
                        this.model_resources = CONFIG.model_resource_settings();
                        this.set_status(
                            t!(
                                "status.model_resource_save_failed",
                                error = error.to_string()
                            )
                            .to_string(),
                            cx,
                        );
                    }
                }
            });
        });
        self.track_write_task(task);
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

    /// 在桌宠受理模型命令后提交换装状态；仅跟随全局模型的人格会持久化清单变体。
    pub(in crate::ui) fn commit_agent_outfit(
        &mut self,
        action: AgentOutfitAction,
        cx: &mut Context<Self>,
    ) -> Result<Option<PathBuf>, String> {
        let active_has_binding = self.active_persona_has_live2d_binding();
        self.commit_agent_outfit_with_binding(action, active_has_binding, cx)
    }

    fn commit_agent_outfit_with_binding(
        &mut self,
        action: AgentOutfitAction,
        active_has_binding: bool,
        cx: &mut Context<Self>,
    ) -> Result<Option<PathBuf>, String> {
        match action {
            AgentOutfitAction::Unchanged => Ok(None),
            AgentOutfitAction::LoadVariant(relative_path) => {
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
                    self.commit_model_selection(Some(relative_path.clone()), cx);
                    self.applied_persona_id = CONFIG
                        .persona_settings()
                        .active()
                        .map(|persona| persona.id.clone());
                    self.applied_persona_model = Some(relative_path);
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

    /// 使用明确的人格绑定状态提交测试换装，避免测试写入进程级配置。
    #[cfg(test)]
    pub(in crate::ui) fn commit_agent_outfit_with_binding_for_test(
        &mut self,
        action: AgentOutfitAction,
        active_has_binding: bool,
        cx: &mut Context<Self>,
    ) -> Result<Option<PathBuf>, String> {
        self.commit_agent_outfit_with_binding(action, active_has_binding, cx)
    }

    /// 准备已经应用的人格模型与表达式服装状态，供重复配置事件回归测试使用。
    #[cfg(test)]
    pub(in crate::ui) fn set_applied_persona_model_for_test(
        &mut self,
        relative_path: PathBuf,
        outfit: &str,
    ) {
        self.global_model_selection = Some(relative_path.clone());
        self.applied_persona_id = CONFIG
            .persona_settings()
            .active()
            .map(|persona| persona.id.clone());
        self.applied_persona_model = Some(relative_path.clone());
        self.catalog
            .set_runtime_selection(Some(&relative_path))
            .expect("测试模型必须属于目录扫描结果");
        self.active_outfit = Some(outfit.to_owned());
    }

    #[cfg(test)]
    pub(in crate::ui) fn reapply_persona_model_for_test(&mut self, cx: &mut Context<Self>) {
        self.apply_active_persona_live2d_model(cx);
    }

    #[cfg(test)]
    pub(in crate::ui) fn active_outfit_for_test(&self) -> Option<&str> {
        self.active_outfit.as_deref()
    }

    fn agent_outfit_candidates(&self) -> Vec<AgentOutfitCandidate> {
        if !self.allow_agent_outfit_change {
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
                label: self.model_resource_name(&key, &default_name),
                target: AgentOutfitTarget::Variant(variant.relative_path().to_path_buf()),
            });
        }
        for expression in self.preview_capabilities.expressions() {
            if self.expression_category(expression) != ModelExpressionCategory::Outfit {
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
                label: self.model_resource_name(&key, resource.default_name()),
                target: AgentOutfitTarget::Expression(resource.runtime_id().to_owned()),
            });
        }
        let mut used_names = HashSet::with_capacity(candidates.len());
        for candidate in &mut candidates {
            candidate.label = unique_outfit_name(&candidate.label, &mut used_names);
        }
        candidates
    }

    fn set_status(&mut self, message: impl Into<String>, cx: &mut Context<Self>) {
        const TOAST_LIFETIME: std::time::Duration = std::time::Duration::from_millis(3_000);

        self.toast_revision = self.toast_revision.wrapping_add(1).max(1);
        let revision = self.toast_revision;
        self.status = Some(message.into());
        let background = cx.background_executor().clone();
        self.toast_task = Some(cx.spawn(async move |this, cx| {
            background.timer(TOAST_LIFETIME).await;
            let _ = this.update(cx, |this, cx| {
                if this.toast_revision == revision {
                    this.status = None;
                    cx.notify();
                }
            });
        }));
        cx.notify();
    }

    fn select_family(&mut self, index: usize, cx: &mut Context<Self>) {
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

    fn select_variant(&mut self, relative_path: PathBuf, cx: &mut Context<Self>) {
        if self.global_model_selection.as_deref() == Some(relative_path.as_path()) {
            if !self.active_persona_has_live2d_binding()
                && self.catalog.selected_relative_path() == Some(relative_path.as_path())
                && self.active_outfit.take().is_some()
            {
                cx.emit(SettingsEvent::ResetExpression);
                cx.notify();
            }
            return;
        }
        self.select_global_model(relative_path, cx);
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
        self.refresh_models_with_selection(configured_selection, false, window, cx);
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
        if !self.active_persona_has_live2d_binding()
            && let Err(error) = self.catalog.set_runtime_selection(Some(&relative_path))
        {
            self.set_status(
                t!("status.model_action_failed", error = error.to_string()).to_string(),
                cx,
            );
            return;
        }
        self.commit_model_selection(Some(relative_path), cx);
        self.apply_active_persona_live2d_model(cx);
    }

    fn active_persona_has_live2d_binding(&self) -> bool {
        CONFIG
            .persona_settings()
            .active()
            .is_some_and(|persona| persona.live2d_model.is_some())
    }

    fn commit_model_selection(&mut self, relative_path: Option<PathBuf>, cx: &mut Context<Self>) {
        if let Some(input) = self.model_resource_name_input.clone() {
            self.commit_model_resource_name(&input, cx);
        }
        let global_model_save_revision = self.stage_model_selection(relative_path.clone(), cx);

        let config_revision = CONFIG.reserve_model_revision();
        let background = cx.background_executor().clone();
        let task = cx.spawn(async move |this, cx| {
            let result = background
                .spawn(async move {
                    CONFIG.set_selected_model_at_revision(relative_path.as_deref(), config_revision)
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                let persisted_selection = CONFIG.selected_model();
                this.finish_model_selection_write(
                    global_model_save_revision,
                    result,
                    persisted_selection,
                    cx,
                );
            });
        });
        self.track_write_task(task);
    }

    fn stage_model_selection(
        &mut self,
        relative_path: Option<PathBuf>,
        cx: &mut Context<Self>,
    ) -> u64 {
        self.revision = self.revision.wrapping_add(1);
        self.catalog_revision = self.catalog_revision.wrapping_add(1);
        self.global_model_save_revision = self.global_model_save_revision.wrapping_add(1).max(1);
        self.global_model_selection = relative_path;
        cx.notify();
        self.global_model_save_revision
    }

    fn finish_model_selection_write(
        &mut self,
        save_revision: u64,
        result: Result<Option<()>, ConfigWriteError>,
        persisted_selection: Option<PathBuf>,
        cx: &mut Context<Self>,
    ) {
        if self.global_model_save_revision != save_revision {
            return;
        }
        match result {
            Ok(Some(())) => cx.notify(),
            Ok(None) => self.restore_global_model_selection(persisted_selection, cx),
            Err(error) => {
                self.restore_global_model_selection(persisted_selection, cx);
                self.set_status(
                    t!("status.model_save_failed", error = error.to_string()).to_string(),
                    cx,
                );
            }
        }
    }

    fn restore_global_model_selection(
        &mut self,
        persisted_selection: Option<PathBuf>,
        cx: &mut Context<Self>,
    ) {
        self.catalog_revision = self.catalog_revision.wrapping_add(1);
        self.global_model_selection = persisted_selection;
        self.apply_active_persona_live2d_model(cx);
        cx.notify();
    }

    fn refresh_models(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let previous_selection = self.global_model_selection.clone();
        self.refresh_models_with_selection(previous_selection, true, window, cx);
    }

    fn open_model_directory(&mut self, cx: &mut Context<Self>) {
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
                    log::info!("正在通过系统文件管理器打开模型目录");
                    cx.open_with_system(&directory);
                    this.set_status(t!("status.opening_model_directory").to_string(), cx);
                }
                Err(error) => {
                    log::warn!("无法打开模型目录：stage=prepare_or_launch");
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
        log::debug!("开始扫描 Live2D 模型目录：scan_revision={catalog_revision}");
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
                    this.is_refreshing = false;
                    this.refresh_window_scoped = false;
                    if this.catalog_revision != catalog_revision {
                        this.set_status(t!("status.scan_stale").to_string(), cx);
                        return;
                    }
                    match catalog {
                        Ok(catalog) => {
                            let (families, outfits) = catalog.counts();
                            let warning = catalog.warning().map(str::to_owned);
                            if warning.is_some() {
                                log::warn!(
                                    "Live2D 模型扫描完成但存在可恢复问题：scan_revision={catalog_revision}, families={families}, outfits={outfits}"
                                );
                            } else {
                                log::info!(
                                    "Live2D 模型扫描完成：scan_revision={catalog_revision}, families={families}, outfits={outfits}"
                                );
                            }
                            this.catalog = catalog;
                            let status = match warning {
                                Some(warning) => t!(
                                    "status.scan_result_warning",
                                    families = families,
                                    outfits = outfits,
                                    warning = warning
                                )
                                .to_string(),
                                None => t!(
                                    "status.scan_result",
                                    families = families,
                                    outfits = outfits
                                )
                                .to_string(),
                            };
                            this.set_status(status, cx);
                            cx.emit(SettingsEvent::ModelCatalogChanged);

                            this.apply_active_persona_live2d_model_after_scan(cx);
                        }
                        Err(error) => {
                            log::warn!(
                                "Live2D 模型扫描失败：scan_revision={catalog_revision}, stage=root_scan"
                            );
                            this.set_status(
                                t!("status.scan_failed", error = error).to_string(),
                                cx,
                            );
                        }
                    }
                });
            });
        }));
    }

    fn set_frame_rate(&mut self, frame_rate: FrameRate, cx: &mut Context<Self>) {
        if !matches!(frame_rate, FrameRate::Custom(_)) {
            self.custom_frame_rate_input_revision =
                self.custom_frame_rate_input_revision.wrapping_add(1);
            self.custom_frame_rate_save_task = None;
        }
        if self.frame_rate == frame_rate {
            return;
        }
        self.frame_rate = frame_rate;
        self.revision = self.revision.wrapping_add(1);
        let revision = self.revision;
        cx.notify();

        let config_revision = CONFIG.reserve_frame_rate_revision();
        let background = cx.background_executor().clone();
        let task = cx.spawn(async move |this, cx| {
            let result = background
                .spawn(
                    async move { CONFIG.set_frame_rate_at_revision(frame_rate, config_revision) },
                )
                .await;
            let _ = this.update(cx, |this, cx| {
                if matches!(&result, Ok(Some(()))) {
                    cx.emit(SettingsEvent::FrameRateChanged);
                }
                if this.revision == revision {
                    if let Err(error) = result {
                        this.set_status(
                            t!("status.frame_rate_failed", error = error.to_string()).to_string(),
                            cx,
                        );
                    } else {
                        cx.notify();
                    }
                }
            });
        });
        self.track_write_task(task);
    }

    fn select_custom_frame_rate(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if matches!(self.frame_rate, FrameRate::Custom(_)) {
            return;
        }
        self.custom_frame_rate_input_revision =
            self.custom_frame_rate_input_revision.wrapping_add(1);
        self.custom_frame_rate_save_task = None;
        let fps = custom_frame_rate_seed(self.frame_rate);
        if let Some(input) = &self.custom_frame_rate_input
            && input.read(cx).value() != fps.to_string()
        {
            input.update(cx, |input, cx| {
                input.set_value(fps.to_string(), window, cx);
            });
        }
        if let Ok(frame_rate) = FrameRate::custom(fps) {
            self.set_frame_rate(frame_rate, cx);
        }
    }

    fn schedule_custom_frame_rate_save(
        &mut self,
        input: &Entity<InputState>,
        cx: &mut Context<Self>,
    ) {
        if !matches!(self.frame_rate, FrameRate::Custom(_)) {
            return;
        }
        self.custom_frame_rate_input_revision =
            self.custom_frame_rate_input_revision.wrapping_add(1);
        let revision = self.custom_frame_rate_input_revision;
        self.custom_frame_rate_save_task = None;
        let input = input.clone();
        let background = cx.background_executor().clone();
        self.custom_frame_rate_save_task = Some(cx.spawn(async move |this, cx| {
            background.timer(CUSTOM_FRAME_RATE_SAVE_DELAY).await;
            let _ = this.update(cx, |this, cx| {
                if this.custom_frame_rate_input_revision == revision {
                    this.apply_custom_frame_rate_input(&input, cx);
                }
            });
        }));
    }

    fn commit_custom_frame_rate_input(
        &mut self,
        input: &Entity<InputState>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.custom_frame_rate_input_revision =
            self.custom_frame_rate_input_revision.wrapping_add(1);
        self.custom_frame_rate_save_task = None;
        if self.apply_custom_frame_rate_input(input, cx) {
            let Some(fps) = self.frame_rate.limit() else {
                return;
            };
            if input.read(cx).value() != fps.to_string() {
                input.update(cx, |input, cx| {
                    input.set_value(fps.to_string(), window, cx);
                });
            }
            return;
        }
        let Some(fps) = self.frame_rate.limit() else {
            return;
        };
        input.update(cx, |input, cx| {
            input.set_value(fps.to_string(), window, cx);
        });
    }

    fn apply_custom_frame_rate_input(
        &mut self,
        input: &Entity<InputState>,
        cx: &mut Context<Self>,
    ) -> bool {
        if !matches!(self.frame_rate, FrameRate::Custom(_)) {
            return false;
        }
        let Some(frame_rate) = parse_custom_frame_rate(&input.read(cx).value()) else {
            return false;
        };
        self.set_frame_rate(frame_rate, cx);
        true
    }

    fn flush_custom_frame_rate_input(&mut self, cx: &mut Context<Self>) {
        self.custom_frame_rate_input_revision =
            self.custom_frame_rate_input_revision.wrapping_add(1);
        self.custom_frame_rate_save_task = None;
        if let Some(input) = self.custom_frame_rate_input.clone() {
            self.apply_custom_frame_rate_input(&input, cx);
        }
    }

    fn set_model_window_size(&mut self, size: ModelWindowSize, cx: &mut Context<Self>) {
        if self.model_window_size == size {
            return;
        }
        self.model_window_size = size;
        cx.emit(SettingsEvent::ModelWindowSizeChanged(size));
        self.revision = self.revision.wrapping_add(1);
        let revision = self.revision;
        cx.notify();
        let config_revision = CONFIG.reserve_model_window_size_revision();
        self.persist_setting(
            revision,
            move || CONFIG.set_model_window_size_at_revision(size, config_revision),
            cx,
        );
    }

    fn set_remember_window_positions(&mut self, remember: bool, cx: &mut Context<Self>) {
        if self.remember_window_positions == remember {
            return;
        }
        self.remember_window_positions = remember;
        self.revision = self.revision.wrapping_add(1);
        let revision = self.revision;
        cx.notify();

        let config_revision = CONFIG.reserve_remember_positions_revision();
        self.persist_setting(
            revision,
            move || CONFIG.set_remember_window_positions_at_revision(remember, config_revision),
            cx,
        );
    }

    fn set_eye_tracking(&mut self, enabled: bool, cx: &mut Context<Self>) {
        if self.eye_tracking == enabled {
            return;
        }
        self.eye_tracking = enabled;
        cx.emit(SettingsEvent::EyeTrackingChanged(enabled));
        self.revision = self.revision.wrapping_add(1);
        let revision = self.revision;
        cx.notify();

        let config_revision = CONFIG.reserve_eye_tracking_revision();
        self.persist_setting(
            revision,
            move || CONFIG.set_eye_tracking_at_revision(enabled, config_revision),
            cx,
        );
    }

    fn set_show_fps(&mut self, show: bool, cx: &mut Context<Self>) {
        if self.show_fps == show {
            return;
        }
        self.show_fps = show;
        cx.emit(SettingsEvent::ShowFpsChanged(show));
        self.revision = self.revision.wrapping_add(1);
        let revision = self.revision;
        cx.notify();

        let config_revision = CONFIG.reserve_show_fps_revision();
        self.persist_setting(
            revision,
            move || CONFIG.set_show_fps_at_revision(show, config_revision),
            cx,
        );
    }

    fn set_use_native_tray_menu(&mut self, enabled: bool, cx: &mut Context<Self>) {
        if self.use_native_tray_menu == enabled {
            return;
        }
        self.use_native_tray_menu = enabled;
        cx.emit(SettingsEvent::NativeTrayMenuChanged(enabled));
        self.revision = self.revision.wrapping_add(1);
        let revision = self.revision;
        cx.notify();

        let config_revision = CONFIG.reserve_use_native_tray_menu_revision();
        self.persist_setting(
            revision,
            move || CONFIG.set_use_native_tray_menu_at_revision(enabled, config_revision),
            cx,
        );
    }

    fn set_allow_agent_screenshot(&mut self, allowed: bool, cx: &mut Context<Self>) {
        if self.allow_agent_screenshot == allowed && !self.screenshot_permission_retry_required {
            return;
        }
        self.allow_agent_screenshot = allowed;
        self.screenshot_permission_retry_required = false;
        self.screenshot_permission_revision =
            self.screenshot_permission_revision.wrapping_add(1).max(1);
        let ui_revision = self.screenshot_permission_revision;
        let config_revision = CONFIG.reserve_allow_agent_screenshot_revision(allowed);
        let background = cx.background_executor().clone();
        cx.notify();

        let task = cx.spawn(async move |this, cx| {
            let result = background
                .spawn(async move {
                    CONFIG.set_allow_agent_screenshot_at_revision(allowed, config_revision)
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                if this.screenshot_permission_revision != ui_revision {
                    return;
                }
                this.allow_agent_screenshot = CONFIG.allow_agent_screenshot();
                this.screenshot_permission_retry_required =
                    CONFIG.agent_screenshot_permission_retry_required();
                if let Err(error) = result {
                    this.set_status(
                        t!("status.setting_failed", error = error.to_string()).to_string(),
                        cx,
                    );
                } else {
                    cx.notify();
                }
            });
        });
        self.track_write_task(task);
    }

    fn set_allow_agent_outfit_change(&mut self, allowed: bool, cx: &mut Context<Self>) {
        if self.allow_agent_outfit_change == allowed {
            return;
        }
        self.allow_agent_outfit_change = allowed;
        cx.emit(SettingsEvent::AgentOutfitToolChanged(allowed));
        self.revision = self.revision.wrapping_add(1);
        let revision = self.revision;
        cx.notify();

        let config_revision = CONFIG.reserve_allow_agent_outfit_change_revision();
        self.persist_setting(
            revision,
            move || CONFIG.set_allow_agent_outfit_change_at_revision(allowed, config_revision),
            cx,
        );
    }

    fn set_voice_mode(&mut self, mode: VoiceMode, cx: &mut Context<Self>) {
        if self.voice.mode == mode {
            return;
        }
        self.voice.mode = mode;
        self.voice_save_revision = self.voice_save_revision.wrapping_add(1).max(1);
        let ui_revision = self.voice_save_revision;
        let settings = self.voice.clone();
        let config_revision = CONFIG.reserve_voice_settings_revision();
        let background = cx.background_executor().clone();
        cx.notify();
        let task = cx.spawn(async move |this, cx| {
            let result = background
                .spawn(
                    async move { CONFIG.set_voice_settings_at_revision(settings, config_revision) },
                )
                .await;
            let _ = this.update(cx, |this, cx| {
                if let Ok(Some(settings)) = &result {
                    let current = CONFIG.voice_settings();
                    if current.as_ref() == settings.as_ref() {
                        cx.emit(SettingsEvent::VoiceChanged(current.as_ref().clone()));
                    }
                }
                if this.voice_save_revision != ui_revision {
                    return;
                }
                match result {
                    Ok(Some(settings)) => {
                        this.voice = settings.as_ref().clone();
                        cx.notify();
                    }
                    Ok(None) => {}
                    Err(error) => {
                        this.voice = CONFIG.voice_settings().as_ref().clone();
                        this.set_status(
                            t!("status.setting_failed", error = error.to_string()).to_string(),
                            cx,
                        );
                    }
                }
            });
        });
        self.track_write_task(task);
    }

    fn set_logging_settings(&mut self, settings: LoggingSettings, cx: &mut Context<Self>) {
        if self.logging == settings {
            return;
        }
        self.logging = settings;
        self.revision = self.revision.wrapping_add(1);
        let revision = self.revision;
        cx.notify();

        let config_revision = CONFIG.reserve_logging_settings_revision();
        let background = cx.background_executor().clone();
        let task = cx.spawn(async move |this, cx| {
            let result = background
                .spawn(async move {
                    let persisted = CONFIG
                        .set_logging_settings_at_revision(settings, config_revision)
                        .map_err(|error| ("persist", error.to_string()))?;
                    if persisted.is_some() {
                        crate::logging::apply_current_settings()
                            .map_err(|error| ("apply_runtime", error))?;
                    }
                    Ok::<Option<()>, (&'static str, String)>(persisted)
                })
                .await;
            if let Err(("apply_runtime", _)) = &result {
                log::error!("更新运行时日志配置失败：phase=apply_runtime");
            }
            let _ = this.update(cx, |this, cx| {
                if this.revision != revision {
                    return;
                }
                if let Err((_, error)) = result {
                    this.set_status(t!("status.setting_failed", error = error).to_string(), cx);
                } else {
                    cx.notify();
                }
            });
        });
        self.track_write_task(task);
    }

    /// 输入过程中延迟提交，避免每次按键都触发一次完整配置写盘与日志器重建。
    fn schedule_logging_save(
        &mut self,
        input: &Entity<InputState>,
        apply: fn(&mut Self, &Entity<InputState>, &mut Context<Self>),
        cx: &mut Context<Self>,
    ) {
        self.logging_input_revision = self.logging_input_revision.wrapping_add(1);
        let revision = self.logging_input_revision;
        let input = input.clone();
        let background = cx.background_executor().clone();
        self.logging_save_task = Some(cx.spawn(async move |this, cx| {
            background.timer(LOGGING_SAVE_DELAY).await;
            let _ = this.update(cx, |this, cx| {
                if this.logging_input_revision == revision {
                    apply(this, &input, cx);
                }
            });
        }));
    }

    fn commit_logging_input(
        &mut self,
        input: &Entity<InputState>,
        apply: fn(&mut Self, &Entity<InputState>, &mut Context<Self>),
        cx: &mut Context<Self>,
    ) {
        self.logging_input_revision = self.logging_input_revision.wrapping_add(1);
        self.logging_save_task = None;
        apply(self, input, cx);
    }

    /// 在设置窗口关闭或应用退出前提交尚未到期的日志输入。
    fn flush_logging_inputs(&mut self, cx: &mut Context<Self>) {
        self.logging_input_revision = self.logging_input_revision.wrapping_add(1);
        self.logging_save_task = None;
        if let Some(input) = self.log_max_size_input.clone() {
            self.set_log_max_size_from_input(&input, cx);
        }
        if let Some(input) = self.log_keep_files_input.clone() {
            self.set_log_keep_files_from_input(&input, cx);
        }
    }

    fn set_log_max_size_from_input(&mut self, input: &Entity<InputState>, cx: &mut Context<Self>) {
        let Ok(max_size_mb) = input.read(cx).value().parse::<u32>() else {
            return;
        };
        let settings = LoggingSettings {
            max_size_mb,
            ..self.logging
        };
        if settings.normalized().is_ok() {
            self.set_logging_settings(settings, cx);
        }
    }

    fn set_log_keep_files_from_input(
        &mut self,
        input: &Entity<InputState>,
        cx: &mut Context<Self>,
    ) {
        let Ok(keep_files) = input.read(cx).value().parse::<u32>() else {
            return;
        };
        let settings = LoggingSettings {
            keep_files,
            ..self.logging
        };
        if settings.normalized().is_ok() {
            self.set_logging_settings(settings, cx);
        }
    }

    fn reset_window_positions(&mut self, cx: &mut Context<Self>) {
        self.revision = self.revision.wrapping_add(1);
        let revision = self.revision;
        self.set_status(t!("status.positions_clearing").to_string(), cx);

        let config_revision = CONFIG.reserve_reset_positions_revision();
        let background = cx.background_executor().clone();
        let task = cx.spawn(async move |this, cx| {
            let result = background
                .spawn(async move { CONFIG.reset_window_positions_at_revision(config_revision) })
                .await;
            let _ = this.update(cx, |this, cx| {
                if matches!(&result, Ok(Some(()))) {
                    cx.emit(SettingsEvent::WindowPositionsReset);
                }
                if this.revision == revision {
                    let status = match result {
                        Ok(Some(())) => t!("status.positions_reset").to_string(),
                        Ok(None) => t!("status.position_reset_replaced").to_string(),
                        Err(error) => t!("status.position_reset_failed", error = error.to_string())
                            .to_string(),
                    };
                    this.set_status(status, cx);
                }
            });
        });
        self.track_write_task(task);
    }

    fn preview_outfit(&mut self, runtime_id: String, display_name: String, cx: &mut Context<Self>) {
        self.active_outfit = Some(runtime_id.clone());
        cx.emit(SettingsEvent::PreviewExpression(runtime_id));
        self.set_status(
            t!("status.outfit_preview", name = display_name).to_string(),
            cx,
        );
        cx.notify();
    }

    fn preview_motion(&mut self, runtime_id: String, display_name: String, cx: &mut Context<Self>) {
        cx.emit(SettingsEvent::PreviewMotion(runtime_id));
        self.set_status(
            t!("status.motion_preview", name = display_name).to_string(),
            cx,
        );
    }

    fn preview_expression(
        &mut self,
        runtime_id: String,
        display_name: String,
        cx: &mut Context<Self>,
    ) {
        self.active_outfit = None;
        cx.emit(SettingsEvent::PreviewExpression(runtime_id));
        self.set_status(
            t!("status.expression_preview", name = display_name).to_string(),
            cx,
        );
    }

    fn capture_custom_theme(&mut self, cx: &mut Context<Self>) {
        if let Some(input) = &self.custom_accent_input {
            self.appearance.custom.accent = input.read(cx).value().to_string();
        }
        if let Some(input) = &self.custom_background_input {
            self.appearance.custom.background = input.read(cx).value().to_string();
        }
    }

    fn set_appearance(
        &mut self,
        appearance: AppearanceSettings,
        show_feedback: bool,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let appearance = match appearance.normalized() {
            Ok(appearance) => appearance,
            Err(error) => {
                self.set_status(error, cx);
                return;
            }
        };
        let published_before_request = CONFIG.appearance().as_ref().clone();
        self.appearance = appearance.clone();
        let requested = appearance.clone();
        self.revision = self.revision.wrapping_add(1);
        let revision = self.revision;
        cx.notify();
        let config_revision = CONFIG.reserve_appearance_revision();
        let background = cx.background_executor().clone();
        let task = cx.spawn(async move |this, cx| {
            let result = background
                .spawn(
                    async move { CONFIG.set_appearance_at_revision(appearance, config_revision) },
                )
                .await;
            let _ = this.update(cx, |this, cx| {
                let request_can_update_draft =
                    this.appearance == requested || this.appearance == published_before_request;
                match result {
                    Ok(Some(published)) => {
                        let published = published.as_ref().clone();
                        if request_can_update_draft {
                            this.appearance = published.clone();
                        }
                        apply_language(published.language);
                        apply(&published, None, cx);
                        cx.emit(SettingsEvent::AppearanceChanged(published));
                        if request_can_update_draft && this.revision == revision && show_feedback {
                            this.set_status(t!("status.appearance_saved").to_string(), cx);
                        } else {
                            cx.notify();
                        }
                    }
                    Ok(None) if request_can_update_draft => {
                        this.appearance = CONFIG.appearance().as_ref().clone();
                        cx.notify();
                    }
                    Ok(None) => {}
                    Err(error) if request_can_update_draft => {
                        this.appearance = CONFIG.appearance().as_ref().clone();
                        if this.revision == revision {
                            this.set_status(
                                t!("status.appearance_failed", error = error.to_string())
                                    .to_string(),
                                cx,
                            );
                        } else {
                            cx.notify();
                        }
                    }
                    Err(_) => {}
                }
            });
        });
        self.track_write_task(task);
    }

    fn set_theme(&mut self, theme: ThemePreset, window: &mut Window, cx: &mut Context<Self>) {
        if theme == ThemePreset::Custom {
            self.capture_custom_theme(cx);
        }
        let mut appearance = self.appearance.clone();
        appearance.theme = theme;
        self.set_appearance(appearance, false, window, cx);
    }

    fn apply_custom_theme(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.capture_custom_theme(cx);
        let mut appearance = self.appearance.clone();
        appearance.theme = ThemePreset::Custom;
        self.set_appearance(appearance, true, window, cx);
    }

    fn set_language(&mut self, language: AppLanguage, window: &mut Window, cx: &mut Context<Self>) {
        let mut appearance = self.appearance.clone();
        appearance.language = language;
        self.set_appearance(appearance, false, window, cx);
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

impl EventEmitter<SettingsEvent> for SettingsView {}

fn agent_context_limits(persona_id: &str) -> Option<lunamate_agent::ChatLimits> {
    let settings = CONFIG.llm_settings();
    CONFIG
        .persona_settings()
        .personas
        .iter()
        .find(|persona| persona.id == persona_id)
        .map(|persona| chat_limits(persona, &settings))
}

fn complete_agent_context_mutation(
    completion: Option<&ContextMutationCompletion>,
    result: Result<(), String>,
) {
    if let Some(completion) = completion {
        let _ = completion.try_send(result);
    }
}

pub(in crate::ui) fn parse_custom_frame_rate(value: &str) -> Option<FrameRate> {
    if value.is_empty() || !value.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    value
        .parse::<u16>()
        .ok()
        .and_then(|fps| FrameRate::custom(fps).ok())
}

pub(in crate::ui) fn custom_frame_rate_seed(frame_rate: FrameRate) -> u16 {
    frame_rate.limit().unwrap_or(60)
}
