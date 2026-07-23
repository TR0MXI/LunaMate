//! 保存配置视图状态，处理用户动作，并向桌宠主视图发布热更新事件。

mod components;
mod model_page;
mod render;
mod system_page;
mod window;

use std::path::{Path, PathBuf};

use gpui::{AppContext, Context, Entity, EventEmitter, Subscription, Task, Window};
use gpui_component::input::{InputEvent, InputState, MaskPattern};
use rust_i18n::t;

use crate::{
    live2d_image::ModelPreviewCapabilities,
    theme::{AppLanguage, AppearanceSettings, ThemePreset, apply_language},
};

use super::{
    CONFIG, ConfigWriteError, FrameRate, LOGGING_MAX_FILE_SIZE_MB, LOGGING_MAX_KEEP_FILES,
    LOGGING_MIN_FILE_SIZE_MB, LOGGING_MIN_KEEP_FILES, LoggingSettings, ModelCatalog,
    ModelWindowSize, SharedLlmSettings,
    llm_view::{LlmSettingsView, LlmViewEvent},
};

pub(crate) use window::ConfigWindowView;

/// 配置界面向桌宠主视图发送的热更新事件。
#[derive(Clone, Debug)]
pub(crate) enum ConfigEvent {
    /// 当前模型或服装清单发生变化。
    ModelChanged(Option<PathBuf>),
    /// 渲染帧率已更新，后台调度器应尽快重新读取原子配置。
    FrameRateChanged,
    /// 眼部跟随开关已更新。
    EyeTrackingChanged(bool),
    /// 主窗口帧率显示开关已更新。
    ShowFpsChanged(bool),
    /// 桌宠主窗口尺寸档位已更新。
    ModelWindowSizeChanged(ModelWindowSize),
    /// 请求主模型 generation 播放一个动作。
    PreviewMotion(String),
    /// 请求主模型 generation 应用一个表情或服装表达式。
    PreviewExpression(String),
    /// 请求主模型 generation 恢复模型清单中的默认表情。
    ResetExpression,
    /// 语言模型或系统提示词配置已经发布。
    LlmChanged(SharedLlmSettings),
    /// 外观设置已经发布，所有窗口应刷新主题和语言。
    AppearanceChanged(AppearanceSettings),
    /// 已清除持久化位置，所有现存窗口应立即返回默认位置。
    WindowPositionsReset,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ConfigSection {
    Model,
    Conversation,
    System,
    Debug,
}

/// 独立设置窗口的主体状态。
pub(crate) struct ConfigView {
    catalog: ModelCatalog,
    llm_view: Option<Entity<LlmSettingsView>>,
    llm_draft: Option<SharedLlmSettings>,
    custom_accent_input: Option<Entity<InputState>>,
    custom_background_input: Option<Entity<InputState>>,
    log_max_size_input: Option<Entity<InputState>>,
    log_keep_files_input: Option<Entity<InputState>>,
    preview_capabilities: ModelPreviewCapabilities,
    active_outfit: Option<String>,
    section: ConfigSection,
    status: Option<String>,
    frame_rate: FrameRate,
    model_window_size: ModelWindowSize,
    remember_window_positions: bool,
    eye_tracking: bool,
    show_fps: bool,
    logging: LoggingSettings,
    appearance: AppearanceSettings,
    is_refreshing: bool,
    revision: u64,
    model_revision: u64,
    refresh_task: Option<Task<()>>,
    write_tasks: Vec<Task<()>>,
    llm_subscription: Option<Subscription>,
    logging_input_subscriptions: Vec<Subscription>,
    toast_revision: u64,
    toast_task: Option<Task<()>>,
}

impl ConfigView {
    /// 使用启动阶段得到的模型目录和配置诊断创建界面。
    pub(crate) fn new(
        catalog: ModelCatalog,
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
            llm_view: None,
            llm_draft: None,
            custom_accent_input: None,
            custom_background_input: None,
            log_max_size_input: None,
            log_keep_files_input: None,
            preview_capabilities: ModelPreviewCapabilities::default(),
            active_outfit: None,
            section: ConfigSection::Model,
            status: None,
            frame_rate: CONFIG.frame_rate(),
            model_window_size: CONFIG.model_window_size(),
            remember_window_positions: CONFIG.remember_window_positions(),
            eye_tracking: CONFIG.eye_tracking(),
            show_fps: CONFIG.show_fps(),
            logging: *CONFIG.logging_settings(),
            appearance: CONFIG.appearance().as_ref().clone(),
            is_refreshing: false,
            revision: 0,
            model_revision: 0,
            refresh_task: None,
            write_tasks: Vec::new(),
            llm_subscription: None,
            logging_input_subscriptions: Vec::new(),
            toast_revision: 0,
            toast_task: None,
        };
        if let Some(status) = status {
            view.set_status(status, cx);
        }
        view
    }

    /// 设置窗口打开时创建输入组件，并把当前外观同步到全局主题。
    pub(crate) fn activate_window(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        apply_language(self.appearance.language);
        crate::theme::apply(&self.appearance, Some(window), cx);
        let settings = self
            .llm_draft
            .take()
            .unwrap_or_else(|| CONFIG.llm_settings());
        let llm_view = cx.new(|cx| LlmSettingsView::new(settings, window, cx));
        self.custom_accent_input = Some(cx.new(|cx| {
            InputState::new(window, cx).default_value(self.appearance.custom.accent.clone())
        }));
        self.custom_background_input = Some(cx.new(|cx| {
            InputState::new(window, cx).default_value(self.appearance.custom.background.clone())
        }));
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
                |this, input, event: &InputEvent, cx| {
                    if matches!(event, InputEvent::Change) {
                        this.set_log_max_size_from_input(&input, cx);
                    }
                },
            ),
            cx.subscribe(
                &log_keep_files_input,
                |this, input, event: &InputEvent, cx| {
                    if matches!(event, InputEvent::Change) {
                        this.set_log_keep_files_from_input(&input, cx);
                    }
                },
            ),
        ];
        self.log_max_size_input = Some(log_max_size_input);
        self.log_keep_files_input = Some(log_keep_files_input);
        self.llm_subscription = Some(cx.subscribe(
            &llm_view,
            |this, _, event: &LlmViewEvent, cx| {
                this.llm_draft = Some(event.0.clone());
                cx.emit(ConfigEvent::LlmChanged(event.0.clone()));
            },
        ));
        self.llm_view = Some(llm_view);
        cx.notify();
    }

    /// 设置窗口关闭时丢弃绑定到旧窗口的输入状态。
    pub(crate) fn deactivate_window(&mut self, cx: &mut Context<Self>) {
        if let Some(llm_view) = self.llm_view.take() {
            let (draft, pending) =
                llm_view.update(cx, |llm_view, cx| llm_view.take_window_state(cx));
            self.llm_draft = Some(draft);
            self.write_tasks.extend(pending);
        }
        self.custom_accent_input = None;
        self.custom_background_input = None;
        self.log_max_size_input = None;
        self.log_keep_files_input = None;
        self.llm_subscription = None;
        self.logging_input_subscriptions.clear();
        cx.notify();
    }

    /// 取出配置主体和当前语言模型编辑器中尚未完成的写入任务。
    pub(crate) fn take_pending_write_tasks(&mut self, cx: &mut Context<Self>) -> Vec<Task<()>> {
        if let Some(llm_view) = &self.llm_view {
            let llm_view = llm_view.clone();
            let (draft, pending) =
                llm_view.update(cx, |llm_view, cx| llm_view.take_window_state(cx));
            self.llm_draft = Some(draft);
            self.write_tasks.extend(pending);
        }
        std::mem::take(&mut self.write_tasks)
    }

    fn track_write_task(&mut self, task: Task<()>) {
        self.write_tasks.retain(|task| !task.is_ready());
        self.write_tasks.push(task);
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

    /// 接收主模型 generation 的能力快照，供设置窗口显示可用控制项。
    pub(crate) fn set_preview_capabilities(
        &mut self,
        capabilities: ModelPreviewCapabilities,
        cx: &mut Context<Self>,
    ) {
        self.preview_capabilities = capabilities;
        cx.notify();
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
                cx.emit(ConfigEvent::ResetExpression);
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
        self.revision = self.revision.wrapping_add(1);
        let revision = self.revision;
        self.model_revision = self.model_revision.wrapping_add(1);
        self.active_outfit = None;
        let relative_path = self.catalog.selected_relative_path().map(Path::to_path_buf);
        cx.emit(ConfigEvent::ModelChanged(model_path));
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
                        }
                    }
                    Err(error) => {
                        this.set_status(t!("status.scan_failed", error = error).to_string(), cx)
                    }
                }
            });
        }));
    }

    fn set_frame_rate(&mut self, frame_rate: FrameRate, cx: &mut Context<Self>) {
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
                    cx.emit(ConfigEvent::FrameRateChanged);
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

    fn set_model_window_size(&mut self, size: ModelWindowSize, cx: &mut Context<Self>) {
        if self.model_window_size == size {
            return;
        }
        self.model_window_size = size;
        cx.emit(ConfigEvent::ModelWindowSizeChanged(size));
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
        cx.emit(ConfigEvent::EyeTrackingChanged(enabled));
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
        cx.emit(ConfigEvent::ShowFpsChanged(show));
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
                        .map_err(|error| error.to_string())?;
                    if persisted.is_some() {
                        crate::logging::apply_current_settings()?;
                    }
                    Ok::<Option<()>, String>(persisted)
                })
                .await;
            if let Err(error) = &result {
                log::error!("更新日志配置失败：{error}");
            }
            let _ = this.update(cx, |this, cx| {
                if this.revision != revision {
                    return;
                }
                if let Err(error) = result {
                    this.set_status(t!("status.setting_failed", error = error).to_string(), cx);
                } else {
                    cx.notify();
                }
            });
        });
        self.track_write_task(task);
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
                    cx.emit(ConfigEvent::WindowPositionsReset);
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
        cx.emit(ConfigEvent::PreviewExpression(name));
        cx.notify();
    }

    fn preview_motion(&mut self, name: String, cx: &mut Context<Self>) {
        cx.emit(ConfigEvent::PreviewMotion(name));
    }

    fn preview_expression(&mut self, name: String, cx: &mut Context<Self>) {
        cx.emit(ConfigEvent::PreviewExpression(name));
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
        crate::theme::apply(&appearance, Some(window), cx);
        cx.emit(ConfigEvent::AppearanceChanged(appearance.clone()));
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

impl EventEmitter<ConfigEvent> for ConfigView {}
