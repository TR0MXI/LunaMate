//! 保存设置视图状态，处理用户动作，并向桌宠主视图发布热更新事件。

mod components;
mod model_page;
mod render;
mod shortcut_page;
mod system_page;
mod tool_page;
mod voice_page;
mod window;

use std::{
    collections::{HashMap, HashSet},
    path::{Path, PathBuf},
    time::Duration,
};

use gpui::{
    AppContext, Context, Entity, EventEmitter, FocusHandle, KeyDownEvent, KeybindingKeystroke,
    PathPromptOptions, Subscription, Task, Window,
};
use gpui_component::input::{InputEvent, InputState, MaskPattern};
use rust_i18n::t;

use crate::{
    agent::{
        AgentMemoryAccess, AgentSettingsDraft, AgentSettingsEvent, AgentSettingsView,
        PersonaSettingsDraft, PersonaSettingsEvent, PersonaSettingsView,
    },
    config::{
        AppLanguage, AppearanceSettings, CONFIG, CUSTOM_FRAME_RATE_MAX, CUSTOM_FRAME_RATE_MIN,
        ConfigWriteError, FrameRate, LOGGING_MAX_FILE_SIZE_MB, LOGGING_MAX_KEEP_FILES,
        LOGGING_MIN_FILE_SIZE_MB, LOGGING_MIN_KEEP_FILES, LoggingSettings, ModelWindowSize,
        ShortcutAction, ShortcutSettings, ThemePreset, VoiceMode, VoiceSettings,
    },
    model::{ModelCatalog, ModelPreviewCapabilities, ensure_model_directory},
    shortcut::{ShortcutRuntimeBinding, shortcut_from_keybinding},
};

use super::{apply, apply_language};

const CUSTOM_FRAME_RATE_SAVE_DELAY: Duration = Duration::from_millis(250);
const LOGGING_SAVE_DELAY: Duration = Duration::from_millis(250);

pub(crate) use window::SettingsWindowView;

/// 设置界面向桌宠主视图发送的热更新事件。
#[derive(Clone, Debug)]
pub(crate) enum SettingsEvent {
    /// 当前模型或服装清单发生变化。
    ModelChanged(Option<PathBuf>),
    /// 当前模型路径未变，但重新扫描后的可用服装集合发生变化。
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
    /// 指定人格的短期上下文需要由持有会话的视图清除。
    PersonaContextCleared(String),
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
    name: String,
    target: AgentOutfitTarget,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ConfigSection {
    Model,
    Provider,
    Persona,
    Voice,
    Shortcut,
    Tool,
    System,
    Debug,
}

struct RetiredAgentSettingsEditor {
    view: Entity<AgentSettingsView>,
    _subscription: Subscription,
}

struct RetiredPersonaSettingsEditor {
    view: Entity<PersonaSettingsView>,
    _subscription: Subscription,
}

/// 独立设置窗口的主体状态。
pub(crate) struct SettingsView {
    catalog: ModelCatalog,
    memory: AgentMemoryAccess,
    agent_settings_view: Option<Entity<AgentSettingsView>>,
    agent_settings_draft: Option<AgentSettingsDraft>,
    persona_settings_view: Option<Entity<PersonaSettingsView>>,
    persona_settings_draft: Option<PersonaSettingsDraft>,
    custom_accent_input: Option<Entity<InputState>>,
    custom_background_input: Option<Entity<InputState>>,
    custom_frame_rate_input: Option<Entity<InputState>>,
    log_max_size_input: Option<Entity<InputState>>,
    log_keep_files_input: Option<Entity<InputState>>,
    voice_whisper_model_input: Option<Entity<InputState>>,
    voice_vad_model_input: Option<Entity<InputState>>,
    shortcut_focus: Option<FocusHandle>,
    preview_capabilities: ModelPreviewCapabilities,
    active_outfit: Option<String>,
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
    shortcut_runtime_bindings: HashMap<ShortcutAction, String>,
    is_refreshing: bool,
    revision: u64,
    model_revision: u64,
    refresh_task: Option<Task<()>>,
    write_tasks: Vec<Task<()>>,
    agent_settings_subscription: Option<Subscription>,
    persona_settings_subscription: Option<Subscription>,
    retired_agent_settings_editors: Vec<RetiredAgentSettingsEditor>,
    retired_persona_settings_editors: Vec<RetiredPersonaSettingsEditor>,
    custom_frame_rate_subscription: Option<Subscription>,
    custom_frame_rate_input_revision: u64,
    custom_frame_rate_save_task: Option<Task<()>>,
    logging_input_subscriptions: Vec<Subscription>,
    voice_input_subscriptions: Vec<Subscription>,
    shortcut_focus_subscription: Option<Subscription>,
    logging_input_revision: u64,
    logging_save_task: Option<Task<()>>,
    screenshot_permission_revision: u64,
    toast_revision: u64,
    toast_task: Option<Task<()>>,
    voice_save_revision: u64,
    shortcut_save_revision: u64,
    voice_picker_revision: u64,
    voice_picker_task: Option<Task<()>>,
}

impl SettingsView {
    /// 使用启动阶段得到的模型目录和配置诊断创建界面。
    pub(crate) fn new(
        catalog: ModelCatalog,
        memory: AgentMemoryAccess,
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
            memory,
            agent_settings_view: None,
            agent_settings_draft: None,
            persona_settings_view: None,
            persona_settings_draft: None,
            custom_accent_input: None,
            custom_background_input: None,
            custom_frame_rate_input: None,
            log_max_size_input: None,
            log_keep_files_input: None,
            voice_whisper_model_input: None,
            voice_vad_model_input: None,
            shortcut_focus: None,
            preview_capabilities: ModelPreviewCapabilities::default(),
            active_outfit: None,
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
            shortcut_runtime_bindings: HashMap::new(),
            is_refreshing: false,
            revision: 0,
            model_revision: 0,
            refresh_task: None,
            write_tasks: Vec::new(),
            agent_settings_subscription: None,
            persona_settings_subscription: None,
            retired_agent_settings_editors: Vec::new(),
            retired_persona_settings_editors: Vec::new(),
            custom_frame_rate_subscription: None,
            custom_frame_rate_input_revision: 0,
            custom_frame_rate_save_task: None,
            logging_input_subscriptions: Vec::new(),
            voice_input_subscriptions: Vec::new(),
            shortcut_focus_subscription: None,
            logging_input_revision: 0,
            logging_save_task: None,
            screenshot_permission_revision: 0,
            toast_revision: 0,
            toast_task: None,
            voice_save_revision: 0,
            shortcut_save_revision: 0,
            voice_picker_revision: 0,
            voice_picker_task: None,
        };
        if let Some(status) = status {
            view.set_status(status, cx);
        }
        view
    }

    /// 设置窗口打开时创建输入组件，并把当前外观同步到全局主题。
    pub(crate) fn activate_window(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.allow_agent_screenshot = CONFIG.requested_allow_agent_screenshot();
        self.screenshot_permission_retry_required =
            CONFIG.agent_screenshot_permission_retry_required();
        apply_language(self.appearance.language);
        apply(&self.appearance, Some(window), cx);
        let shortcut_focus = cx.focus_handle();
        self.shortcut_focus_subscription =
            Some(cx.on_blur(&shortcut_focus, window, |this, _, cx| {
                this.stop_shortcut_recording(cx)
            }));
        self.shortcut_focus = Some(shortcut_focus);
        let draft = self
            .agent_settings_draft
            .take()
            .unwrap_or_else(AgentSettingsDraft::current);
        let agent_settings_view = cx.new(|cx| AgentSettingsView::new(draft, window, cx));
        self.activate_persona_settings(window, cx);
        self.custom_accent_input = Some(cx.new(|cx| {
            InputState::new(window, cx).default_value(self.appearance.custom.accent.clone())
        }));
        self.custom_background_input = Some(cx.new(|cx| {
            InputState::new(window, cx).default_value(self.appearance.custom.background.clone())
        }));
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
                InputEvent::PressEnter { .. } | InputEvent::Blur => {
                    this.commit_custom_frame_rate_input(input, window, cx);
                }
                InputEvent::Focus => {}
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
            cx.subscribe(
                &log_max_size_input,
                |this, input, event: &InputEvent, cx| match event {
                    InputEvent::Change => {
                        this.schedule_logging_save(&input, Self::set_log_max_size_from_input, cx);
                    }
                    InputEvent::PressEnter { .. } | InputEvent::Blur => {
                        this.commit_logging_input(&input, Self::set_log_max_size_from_input, cx);
                    }
                    InputEvent::Focus => {}
                },
            ),
            cx.subscribe(
                &log_keep_files_input,
                |this, input, event: &InputEvent, cx| match event {
                    InputEvent::Change => {
                        this.schedule_logging_save(&input, Self::set_log_keep_files_from_input, cx);
                    }
                    InputEvent::PressEnter { .. } | InputEvent::Blur => {
                        this.commit_logging_input(&input, Self::set_log_keep_files_from_input, cx);
                    }
                    InputEvent::Focus => {}
                },
            ),
        ];
        self.log_max_size_input = Some(log_max_size_input);
        self.log_keep_files_input = Some(log_keep_files_input);
        let voice_whisper_model_input = cx.new(|cx| {
            InputState::new(window, cx).default_value(
                self.voice
                    .whisper_model
                    .as_deref()
                    .map(|path| path.to_string_lossy().into_owned())
                    .unwrap_or_default(),
            )
        });
        let voice_vad_model_input = cx.new(|cx| {
            InputState::new(window, cx).default_value(
                self.voice
                    .vad_model
                    .as_deref()
                    .map(|path| path.to_string_lossy().into_owned())
                    .unwrap_or_default(),
            )
        });
        self.voice_whisper_model_input = Some(voice_whisper_model_input.clone());
        self.voice_vad_model_input = Some(voice_vad_model_input.clone());
        self.voice_input_subscriptions = vec![
            cx.subscribe(
                &voice_whisper_model_input,
                |this, _, event: &InputEvent, cx| {
                    if matches!(event, InputEvent::Change) {
                        this.capture_voice_draft(cx);
                    }
                },
            ),
            cx.subscribe(&voice_vad_model_input, |this, _, event: &InputEvent, cx| {
                if matches!(event, InputEvent::Change) {
                    this.capture_voice_draft(cx);
                }
            }),
        ];
        // 供应商目录变化会改变人格可绑定的候选项，两个编辑器必须保持同步。
        self.agent_settings_subscription = Some(cx.subscribe(
            &agent_settings_view,
            |this, editor, event: &AgentSettingsEvent, cx| {
                let editor_id = editor.entity_id();
                this.retired_agent_settings_editors
                    .retain(|retired| retired.view.entity_id() != editor_id);
                if matches!(event, AgentSettingsEvent::Saved) {
                    cx.emit(SettingsEvent::AgentChanged);
                }
            },
        ));
        self.agent_settings_view = Some(agent_settings_view);
        cx.notify();
    }

    fn activate_persona_settings(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let draft = self
            .persona_settings_draft
            .take()
            .unwrap_or_else(PersonaSettingsDraft::current);
        let memory = self.memory.clone();
        let view = cx.new(|cx| PersonaSettingsView::new(draft, memory, window, cx));
        self.persona_settings_subscription = Some(cx.subscribe(
            &view,
            |this, editor, event: &PersonaSettingsEvent, cx| match event {
                PersonaSettingsEvent::Saved => {
                    let editor_id = editor.entity_id();
                    this.retired_persona_settings_editors
                        .retain(|retired| retired.view.entity_id() != editor_id);
                    cx.emit(SettingsEvent::AgentChanged);
                }
                PersonaSettingsEvent::SaveFinished => {
                    let editor_id = editor.entity_id();
                    this.retired_persona_settings_editors
                        .retain(|retired| retired.view.entity_id() != editor_id);
                }
                PersonaSettingsEvent::ClearContext(persona) => {
                    cx.emit(SettingsEvent::PersonaContextCleared(persona.clone()));
                }
            },
        ));
        self.persona_settings_view = Some(view);
    }

    /// 设置窗口关闭时丢弃绑定到旧窗口的输入状态。
    pub(crate) fn deactivate_window(&mut self, cx: &mut Context<Self>) {
        self.stop_shortcut_recording(cx);
        if let Some(agent_settings_view) = self.agent_settings_view.take() {
            let (draft, pending) =
                agent_settings_view.update(cx, |view, cx| view.take_window_state(cx));
            self.agent_settings_draft = Some(draft);
            let has_pending = pending.iter().any(|task| !task.is_ready());
            if has_pending && let Some(subscription) = self.agent_settings_subscription.take() {
                self.retired_agent_settings_editors
                    .push(RetiredAgentSettingsEditor {
                        view: agent_settings_view,
                        _subscription: subscription,
                    });
            }
            self.write_tasks.extend(pending);
        }
        if let Some(persona_settings_view) = self.persona_settings_view.take() {
            let (draft, pending) =
                persona_settings_view.update(cx, |view, cx| view.take_window_state(cx));
            self.persona_settings_draft = Some(draft);
            let has_pending = pending.iter().any(|task| !task.is_ready());
            if has_pending && let Some(subscription) = self.persona_settings_subscription.take() {
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
        self.capture_voice_draft(cx);
        self.custom_accent_input = None;
        self.custom_background_input = None;
        self.custom_frame_rate_input = None;
        self.custom_frame_rate_save_task = None;
        self.log_max_size_input = None;
        self.log_keep_files_input = None;
        self.voice_input_subscriptions.clear();
        self.voice_whisper_model_input = None;
        self.voice_vad_model_input = None;
        self.shortcut_focus = None;
        self.shortcut_focus_subscription = None;
        self.voice_picker_revision = self.voice_picker_revision.wrapping_add(1).max(1);
        self.voice_picker_task = None;
        self.agent_settings_subscription = None;
        self.persona_settings_subscription = None;
        self.custom_frame_rate_subscription = None;
        self.logging_input_subscriptions.clear();
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

    /// 取出设置主体和 Agent 编辑器中尚未完成的写入任务。
    pub(crate) fn take_pending_write_tasks(&mut self, cx: &mut Context<Self>) -> Vec<Task<()>> {
        self.flush_custom_frame_rate_input(cx);
        self.flush_logging_inputs(cx);
        if let Some(agent_settings_view) = &self.agent_settings_view {
            let agent_settings_view = agent_settings_view.clone();
            let (draft, pending) =
                agent_settings_view.update(cx, |view, cx| view.take_window_state(cx));
            self.agent_settings_draft = Some(draft);
            self.write_tasks.extend(pending);
        }
        if let Some(persona_settings_view) = &self.persona_settings_view {
            let persona_settings_view = persona_settings_view.clone();
            let (draft, pending) =
                persona_settings_view.update(cx, |view, cx| view.take_window_state(cx));
            self.persona_settings_draft = Some(draft);
            self.write_tasks.extend(pending);
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

    /// 返回已发现的模型家族与服装总数。
    #[cfg(test)]
    pub(in crate::ui) fn catalog_counts_for_test(&self) -> (usize, usize) {
        self.catalog.counts()
    }

    /// 只切换测试实体中的换装工具状态，不写入用户配置。
    #[cfg(test)]
    pub(in crate::ui) fn set_agent_outfit_tool_enabled_for_test(&mut self, enabled: bool) {
        self.allow_agent_outfit_change = enabled;
    }

    /// 返回设置窗口是否已经创建输入组件。
    #[cfg(test)]
    pub(in crate::ui) fn window_is_active_for_test(&self) -> bool {
        self.agent_settings_view.is_some()
            && self.persona_settings_view.is_some()
            && self.custom_frame_rate_input.is_some()
            && self.voice_whisper_model_input.is_some()
            && self.voice_vad_model_input.is_some()
            && self.shortcut_focus.is_some()
    }

    /// 返回后台模型扫描是否仍在进行。
    #[cfg(test)]
    pub(in crate::ui) fn is_refreshing_for_test(&self) -> bool {
        self.is_refreshing
    }

    /// 返回当前主模型 generation 上报的可预览能力。
    #[cfg(test)]
    pub(in crate::ui) fn preview_capabilities_for_test(&self) -> &ModelPreviewCapabilities {
        &self.preview_capabilities
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
            3 => ConfigSection::Voice,
            4 => ConfigSection::Shortcut,
            5 => ConfigSection::Tool,
            6 => ConfigSection::System,
            _ => ConfigSection::Debug,
        };
        self.set_section(section, cx);
    }

    /// 返回配置分区总数，供测试遍历全部页面。
    #[cfg(test)]
    pub(in crate::ui) const fn section_count_for_test() -> usize {
        8
    }

    /// 接收主模型 generation 的能力快照，供设置窗口显示可用控制项。
    pub(crate) fn set_preview_capabilities(
        &mut self,
        capabilities: ModelPreviewCapabilities,
        cx: &mut Context<Self>,
    ) {
        self.preview_capabilities = capabilities;
        cx.notify();
    }

    /// 返回当前已加载模型可交给 Agent 选择的本地化服装名称。
    pub(in crate::ui) fn available_agent_outfits(&self) -> Vec<String> {
        self.agent_outfit_candidates()
            .into_iter()
            .map(|candidate| candidate.name)
            .collect()
    }

    /// 将 Agent 传回的枚举名称解析为当前目录和 generation 下的语义动作。
    pub(in crate::ui) fn resolve_agent_outfit(&self, requested: &str) -> Option<AgentOutfitAction> {
        let candidate = self
            .agent_outfit_candidates()
            .into_iter()
            .find(|candidate| candidate.name == requested)?;
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

    /// 在桌宠已经受理对应模型命令后提交换装 UI 状态，并在清单变体切换时持久化选择。
    pub(in crate::ui) fn commit_agent_outfit(
        &mut self,
        action: AgentOutfitAction,
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
                self.commit_model_selection(cx);
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
        if !self.allow_agent_outfit_change {
            return Vec::new();
        }
        let Some(family) = self.catalog.selected_family() else {
            return Vec::new();
        };
        let variants = family.variants();
        let default_outfit = variants.len() == 1;
        let mut candidates = variants
            .iter()
            .map(|variant| AgentOutfitCandidate {
                name: if default_outfit {
                    t!("model.default_outfit").to_string()
                } else {
                    variant.display_name().to_owned()
                },
                target: AgentOutfitTarget::Variant(variant.relative_path().to_path_buf()),
            })
            .chain(
                family
                    .outfits()
                    .iter()
                    .filter(|outfit| {
                        self.preview_capabilities
                            .outfits()
                            .iter()
                            .any(|name| name == outfit.expression_name())
                    })
                    .map(|outfit| AgentOutfitCandidate {
                        name: outfit.display_name().to_owned(),
                        target: AgentOutfitTarget::Expression(outfit.expression_name().to_owned()),
                    }),
            )
            .collect::<Vec<_>>();
        let mut used_names = HashSet::with_capacity(candidates.len());
        for candidate in &mut candidates {
            candidate.name = unique_outfit_name(&candidate.name, &mut used_names);
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
        let model_path = match self.catalog.select_family(index) {
            Ok(path) => path,
            Err(error) => {
                self.set_status(
                    t!("status.model_action_failed", error = error.to_string()).to_string(),
                    cx,
                );
                return;
            }
        };
        self.publish_model_selection(Some(model_path), cx);
    }

    fn select_variant(&mut self, relative_path: PathBuf, cx: &mut Context<Self>) {
        if self.catalog.selected_relative_path() == Some(relative_path.as_path()) {
            if self.active_outfit.take().is_some() {
                cx.emit(SettingsEvent::ResetExpression);
                cx.notify();
            }
            return;
        }
        let model_path = match self.catalog.select_variant(&relative_path) {
            Ok(path) => path,
            Err(error) => {
                self.set_status(
                    t!("status.model_action_failed", error = error.to_string()).to_string(),
                    cx,
                );
                return;
            }
        };
        self.publish_model_selection(Some(model_path), cx);
    }

    /// 在首窗建立后启动初始模型扫描，避免目录 I/O 阻塞 GPUI 初始化。
    pub(crate) fn start_initial_scan(
        &mut self,
        configured_selection: Option<PathBuf>,
        cx: &mut Context<Self>,
    ) {
        self.refresh_models_with_selection(configured_selection, cx);
    }

    fn publish_model_selection(&mut self, model_path: Option<PathBuf>, cx: &mut Context<Self>) {
        self.commit_model_selection(cx);
        cx.emit(SettingsEvent::ModelChanged(model_path));
    }

    fn commit_model_selection(&mut self, cx: &mut Context<Self>) {
        self.revision = self.revision.wrapping_add(1);
        let revision = self.revision;
        self.model_revision = self.model_revision.wrapping_add(1);
        self.active_outfit = None;
        let relative_path = self.catalog.selected_relative_path().map(Path::to_path_buf);
        cx.notify();

        let config_revision = CONFIG.reserve_model_revision();
        let background = cx.background_executor().clone();
        let task = cx.spawn(async move |this, cx| {
            let result = background
                .spawn(async move {
                    CONFIG.set_selected_model_at_revision(relative_path.as_deref(), config_revision)
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                if this.revision == revision {
                    if let Err(error) = result {
                        this.set_status(
                            t!("status.model_save_failed", error = error.to_string()).to_string(),
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

    fn refresh_models(&mut self, cx: &mut Context<Self>) {
        let previous_selection = self.catalog.selected_relative_path().map(Path::to_path_buf);
        self.refresh_models_with_selection(previous_selection, cx);
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
        cx: &mut Context<Self>,
    ) {
        if self.is_refreshing {
            return;
        }
        self.is_refreshing = true;
        self.set_status(t!("status.scanning_models").to_string(), cx);
        let root = self.catalog.root().to_path_buf();
        let model_revision = self.model_revision;
        let background = cx.background_executor().clone();
        log::debug!("开始扫描 Live2D 模型目录：scan_revision={model_revision}");
        cx.notify();

        self.refresh_task = Some(cx.spawn(async move |this, cx| {
            let catalog = background
                .spawn(async move {
                    ModelCatalog::load(root, previous_selection.as_deref())
                        .map_err(|error| error.to_string())
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                this.is_refreshing = false;
                if this.model_revision != model_revision {
                    this.set_status(t!("status.scan_stale").to_string(), cx);
                    return;
                }
                match catalog {
                    Ok(catalog) => {
                        let old_path = this.catalog.selected_model_path();
                        let new_path = catalog.selected_model_path();
                        let (families, outfits) = catalog.counts();
                        let warning = catalog.warning().map(str::to_owned);
                        if warning.is_some() {
                            log::warn!(
                                "Live2D 模型扫描完成但存在可恢复问题：scan_revision={model_revision}, families={families}, outfits={outfits}"
                            );
                        } else {
                            log::info!(
                                "Live2D 模型扫描完成：scan_revision={model_revision}, families={families}, outfits={outfits}"
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
                            None => {
                                t!("status.scan_result", families = families, outfits = outfits)
                                    .to_string()
                            }
                        };
                        this.set_status(status, cx);
                        if new_path != old_path {
                            this.publish_model_selection(new_path, cx);
                        } else {
                            cx.emit(SettingsEvent::ModelCatalogChanged);
                        }
                    }
                    Err(error) => {
                        log::warn!(
                            "Live2D 模型扫描失败：scan_revision={model_revision}, stage=root_scan"
                        );
                        this.set_status(t!("status.scan_failed", error = error).to_string(), cx)
                    }
                }
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

    fn set_voice_mode_draft(&mut self, mode: VoiceMode, cx: &mut Context<Self>) {
        if self.voice.mode == mode {
            return;
        }
        self.voice.mode = mode;
        self.voice_save_revision = self.voice_save_revision.wrapping_add(1).max(1);
        cx.notify();
    }

    fn toggle_voice_gpu_draft(&mut self, cx: &mut Context<Self>) {
        self.voice.use_gpu = !self.voice.use_gpu;
        self.voice_save_revision = self.voice_save_revision.wrapping_add(1).max(1);
        cx.notify();
    }

    fn capture_voice_draft(&mut self, cx: &mut Context<Self>) {
        let whisper_model = self.voice_whisper_model_input.as_ref().and_then(|input| {
            let value = input.read(cx).value().to_string();
            let value = value.trim();
            (!value.is_empty()).then(|| PathBuf::from(value))
        });
        let vad_model = self.voice_vad_model_input.as_ref().and_then(|input| {
            let value = input.read(cx).value().to_string();
            let value = value.trim();
            (!value.is_empty()).then(|| PathBuf::from(value))
        });
        if self.voice.whisper_model != whisper_model || self.voice.vad_model != vad_model {
            self.voice.whisper_model = whisper_model;
            self.voice.vad_model = vad_model;
            self.voice_save_revision = self.voice_save_revision.wrapping_add(1).max(1);
        }
    }

    fn save_voice_settings(&mut self, cx: &mut Context<Self>) {
        self.capture_voice_draft(cx);
        let settings = self.voice.clone();
        self.voice_save_revision = self.voice_save_revision.wrapping_add(1).max(1);
        let ui_revision = self.voice_save_revision;
        let config_revision = CONFIG.reserve_voice_settings_revision();
        let background = cx.background_executor().clone();
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
                        this.set_status(t!("voice.saved").to_string(), cx);
                    }
                    Ok(None) => {}
                    Err(error) => this.set_status(
                        t!("status.setting_failed", error = error.to_string()).to_string(),
                        cx,
                    ),
                }
            });
        });
        self.track_write_task(task);
    }

    fn choose_voice_model(&mut self, whisper: bool, cx: &mut Context<Self>) {
        self.voice_picker_revision = self.voice_picker_revision.wrapping_add(1).max(1);
        let revision = self.voice_picker_revision;
        let paths = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: Some(t!("voice.select_model").to_string().into()),
        });
        self.voice_picker_task = Some(cx.spawn(async move |this, cx| {
            let path = match paths.await {
                Ok(Ok(Some(paths))) => paths.into_iter().next(),
                Ok(Ok(None)) => return,
                Ok(Err(_)) | Err(_) => {
                    log::warn!(
                        "语音模型文件选择器失败：kind={}",
                        if whisper { "whisper" } else { "vad" }
                    );
                    let _ = this.update(cx, |this, cx| {
                        if this.voice_picker_revision == revision {
                            this.set_status(t!("voice.picker_failed").to_string(), cx);
                        }
                    });
                    return;
                }
            };
            let Some(path) = path else {
                return;
            };
            let _ = this.update_in(cx, |this, window, cx| {
                if this.voice_picker_revision != revision {
                    return;
                }
                let input = if whisper {
                    this.voice_whisper_model_input.clone()
                } else {
                    this.voice_vad_model_input.clone()
                };
                if let Some(input) = input {
                    input.update(cx, |input, cx| {
                        input.set_value(path.to_string_lossy().into_owned(), window, cx);
                    });
                    this.capture_voice_draft(cx);
                }
            });
        }));
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

    fn preview_outfit(&mut self, name: String, cx: &mut Context<Self>) {
        self.active_outfit = Some(name.clone());
        cx.emit(SettingsEvent::PreviewExpression(name));
        cx.notify();
    }

    fn preview_motion(&mut self, name: String, cx: &mut Context<Self>) {
        cx.emit(SettingsEvent::PreviewMotion(name));
    }

    fn preview_expression(&mut self, name: String, cx: &mut Context<Self>) {
        cx.emit(SettingsEvent::PreviewExpression(name));
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
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let appearance = match appearance.normalized() {
            Ok(appearance) => appearance,
            Err(error) => {
                self.set_status(error, cx);
                return;
            }
        };
        self.appearance = appearance.clone();
        apply_language(appearance.language);
        apply(&appearance, Some(window), cx);
        cx.emit(SettingsEvent::AppearanceChanged(appearance.clone()));
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
                if this.revision == revision {
                    if let Err(error) = result {
                        this.set_status(
                            t!("status.appearance_failed", error = error.to_string()).to_string(),
                            cx,
                        );
                    } else if show_feedback {
                        this.set_status(t!("status.appearance_saved").to_string(), cx);
                    } else {
                        cx.notify();
                    }
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
