//! 管理桌宠根视图、模型 generation 生命周期及其渲染实现。

pub(in crate::ui) mod model_task;
mod render;

use std::{
    cell::RefCell,
    ffi::OsStr,
    path::{Path, PathBuf},
    rc::Rc,
    sync::Arc,
    time::{Duration, Instant},
};

use gpui::{
    AnyWindowHandle, App, AppContext, Context, Entity, Hsla, Pixels, Point, RenderImage,
    SharedString, Size, Styled, Subscription, Task, Window, WindowBackgroundAppearance,
    WindowDecorations, WindowKind, WindowOptions, px, size, transparent_black,
};
use gpui_component::Root;
use lunamate_agent::tools::AgentOutfitRequest;
use parking_lot::{Condvar, Mutex};
use rust_i18n::t;

use crate::{
    config::{
        AppearanceSettings, CONFIG, ConfigWindow, ModelWindowSize, ThemePreset, VoiceMode,
        VoiceSettings,
    },
    model::{
        FrameRateMeter, FrameWake, GpuUnderlay, GpuUnderlayEvent, GpuUnderlaySize, ModelCommand,
        ModelCommandSender, ModelLoadDiagnostics, ModelPreviewCapabilities, RenderCancellation,
        RenderedModelFrame,
    },
    platform::{
        APPLICATION_ID, GlobalCursorTracker, NativeTrayMenuWindow, SystemTray, TrayIconStyle,
        TrayMenuAnchor, WindowMover, WindowPositionController, configure_settings_window,
        configure_tray_menu_window, set_desktop_pet_window_visible,
    },
    shortcut::{ShortcutEvent, ShortcutManager},
    voice::{SpeechPlayback, VoiceActivitySnapshot, VoiceController, VoiceEvent, VoicePhase},
};

use super::{
    AgentOutfitAction, AgentView, AgentViewEvent, SettingsEvent, SettingsView, SettingsWindowView,
    TrayMenuView, UiPalette, apply, apply_language, cache_window_position, desktop_pet_window_size,
    gpu_underlay_size, gpu_underlay_size_for_window, raster_dimensions_for_window,
    restored_window_bounds, settings_window_sizes, tray_menu_window_options,
};

const FPS_REFRESH_INTERVAL: Duration = Duration::from_millis(250);
const VOICE_LEVEL_REFRESH_INTERVAL: Duration = Duration::from_millis(50);
const VOICE_SHORTCUT_RELEASE_TIMEOUT: Duration = Duration::from_secs(31);
const CURSOR_TRACKING_INTERVAL: Duration = Duration::from_millis(16);
/// 合成器钳制窗口尺寸时放弃重试并接受实际 viewport，避免每帧重复请求 resize。
const MAX_WINDOW_RESIZE_ATTEMPTS: u32 = 8;
/// 退出时同步等待 GPU worker 的上限；驱动卡死时不应无限阻塞主线程。
const GPU_SHUTDOWN_WAIT_TIMEOUT: Duration = Duration::from_secs(5);

pub(in crate::ui) enum ModelLoadState {
    NoModel,
    Loading(String),
    Ready {
        diagnostics: ModelLoadDiagnostics,
        warning: Option<SharedString>,
    },
    Failed(String),
}

impl ModelLoadState {
    pub(in crate::ui) fn ready(diagnostics: ModelLoadDiagnostics) -> Self {
        let warning = localized_diagnostics_warning(&diagnostics);
        Self::Ready {
            diagnostics,
            warning,
        }
    }

    pub(in crate::ui) fn message(&self) -> Option<String> {
        match self {
            Self::NoModel | Self::Ready { .. } => None,
            Self::Loading(name) => Some(t!("model_state.loading", name = name).to_string()),
            Self::Failed(error) => Some(error.clone()),
        }
    }

    pub(in crate::ui) fn diagnostics_message(&self) -> Option<SharedString> {
        let Self::Ready { warning, .. } = self else {
            return None;
        };
        warning.clone()
    }

    fn refresh_localized_warning(&mut self) {
        let Self::Ready {
            diagnostics,
            warning,
        } = self
        else {
            return;
        };
        *warning = localized_diagnostics_warning(diagnostics);
    }
}

fn localized_diagnostics_warning(diagnostics: &ModelLoadDiagnostics) -> Option<SharedString> {
    diagnostics.summary().map(|summary| {
        SharedString::from(t!("model_state.loaded_with_warnings", summary = summary).to_string())
    })
}

pub(in crate::ui) fn look_target_for_position(
    position: Point<Pixels>,
    viewport: Size<Pixels>,
) -> [f32; 2] {
    let width = f32::from(viewport.width).max(1.0);
    let height = f32::from(viewport.height).max(1.0);
    [
        (f32::from(position.x) / width * 2.0 - 1.0).clamp(-1.0, 1.0),
        (1.0 - f32::from(position.y) / height * 2.0).clamp(-1.0, 1.0),
    ]
}

#[derive(Default)]
struct GpuShutdownCompletion {
    completed: Mutex<bool>,
    changed: Condvar,
}

impl GpuShutdownCompletion {
    fn complete(&self) {
        *self.completed.lock() = true;
        self.changed.notify_all();
    }

    /// 阻塞等待 GPU worker 退出；返回是否在超时前完成。
    fn wait_with_timeout(&self, timeout: Duration) -> bool {
        let deadline = Instant::now() + timeout;
        let mut completed = self.completed.lock();
        while !*completed {
            if self
                .changed
                .wait_until(&mut completed, deadline)
                .timed_out()
            {
                return *completed;
            }
        }
        true
    }
}

/// 持有桌宠窗口的模型状态、交互实体和后台渲染任务。
pub(crate) struct DesktopPetView {
    frame: Option<Arc<RenderedModelFrame>>,
    current_rendered_image: Option<Arc<RenderImage>>,
    previous_rendered_image: Option<Arc<RenderImage>>,
    look_target: Arc<Mutex<[f32; 2]>>,
    global_cursor_tracker: Option<GlobalCursorTracker>,
    cursor_tracking_task: Option<Task<()>>,
    eye_tracking_enabled: bool,
    show_fps: bool,
    desktop_pet_visible: bool,
    visibility_revision: u64,
    actual_fps: f32,
    frame_rate_meter: FrameRateMeter,
    fps_task: Option<Task<()>>,
    model_commands: Option<ModelCommandSender>,
    model_wake: Option<FrameWake>,
    frame_rate_wake: Option<FrameWake>,
    model_cancellation: Option<RenderCancellation>,
    gpu_underlay: Option<GpuUnderlay>,
    gpu_event_task: Option<Task<()>>,
    gpu_shutdown_pending: bool,
    gpu_shutdown_restart_cpu: bool,
    gpu_released_for_hidden: bool,
    gpu_restore_pending: bool,
    cpu_fallback_pending: bool,
    close_after_gpu_shutdown: bool,
    quitting: bool,
    gpu_shutdown_completion: Option<Arc<GpuShutdownCompletion>>,
    window_mover: WindowMover,
    system_tray: Option<Rc<SystemTray>>,
    appearance: Rc<RefCell<AppearanceSettings>>,
    config: Entity<SettingsView>,
    chat: Entity<AgentView>,
    voice: Option<VoiceController>,
    speech_playback: Option<SpeechPlayback>,
    voice_mode: VoiceMode,
    voice_revision: u64,
    voice_activity: VoiceActivitySnapshot,
    voice_event_task: Option<Task<()>>,
    voice_level_task: Option<Task<()>>,
    voice_shortcut_release_task: Option<Task<()>>,
    shortcut_manager: Option<ShortcutManager>,
    shortcut_runtime_errors: Vec<String>,
    shortcut_event_task: Option<Task<()>>,
    chat_input_open: bool,
    chat_overlay_visible: bool,
    position_controller: WindowPositionController,
    pending_model_window_size: Option<ModelWindowSize>,
    pending_model_window_size_attempts: u32,
    config_window: Option<AnyWindowHandle>,
    tray_menu_window: Option<AnyWindowHandle>,
    selected_model: Option<PathBuf>,
    model_state: ModelLoadState,
    raster_dimensions: [u32; 2],
    cpu_raster_dimensions: [u32; 2],
    gpu_underlay_size: GpuUnderlaySize,
    model_generation: u64,
    model_task: Option<Task<()>>,
    _config_subscription: Subscription,
    _chat_subscription: Subscription,
    _agent_event_subscription: Subscription,
    _bounds_subscription: Subscription,
    _activation_subscription: Subscription,
    _appearance_subscription: Subscription,
}

impl DesktopPetView {
    /// 创建桌宠根视图并启动初始模型 generation。
    #[expect(
        clippy::too_many_arguments,
        reason = "根视图挂载时需要一次性交接独立实体、窗口服务和语音控制端"
    )]
    pub(crate) fn new(
        config: Entity<SettingsView>,
        chat: Entity<AgentView>,
        voice: Option<VoiceController>,
        speech_playback: Option<SpeechPlayback>,
        initial_model: Option<PathBuf>,
        raster_dimensions: [u32; 2],
        system_tray: Option<Rc<SystemTray>>,
        shortcut_runtime: &tokio::runtime::Handle,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        // GPUI 的动态图像不会随 Arc 释放自动退出 sprite atlas，实体销毁时必须兜底回收。
        cx.on_release(|this, cx| {
            if let Some(cancellation) = this.model_cancellation.take() {
                cancellation.cancel();
            }
            if let Some(wake) = this.model_wake.take() {
                wake.close();
            }
            if let Some(wake) = this.frame_rate_wake.take() {
                wake.close();
            }
            this.shortcut_manager.take();
            if let Some(mut underlay) = this.gpu_underlay.take() {
                underlay.shutdown();
            }
            if let Some(voice) = &this.voice {
                voice.request_shutdown();
            }
            if let Some(playback) = &this.speech_playback {
                playback.stop();
            }
            if let Some(handle) = this.tray_menu_window.take() {
                let _ = handle.update(cx, |_, window, _| window.remove_window());
            }
            let current = this.current_rendered_image.take();
            if let Some(previous) = this.previous_rendered_image.take()
                && current
                    .as_ref()
                    .is_none_or(|current| previous.id != current.id)
            {
                cx.drop_image(previous, None);
            }
            if let Some(current) = current {
                cx.drop_image(current, None);
            }
        })
        .detach();

        let look_target = Arc::new(Mutex::new([0.0, 0.0]));
        let global_cursor_tracker = GlobalCursorTracker::new(window);
        let gpu_underlay_size = gpu_underlay_size_for_window(window);
        let (gpu_underlay, cpu_fallback_pending) = match GpuUnderlay::attach(window) {
            Ok(Some(underlay)) => {
                log::info!("Live2D GPU underlay attachment 已建立");
                (Some(underlay), false)
            }
            Ok(None) => {
                log::info!("当前窗口后端不支持 Live2D GPU underlay，使用 CPU renderer");
                (None, false)
            }
            Err(error) => {
                log::warn!("{}", t!("log.gpu_underlay_init_failed", error = error));
                (None, true)
            }
        };
        let gpu_events = gpu_underlay.as_ref().map(GpuUnderlay::events);
        let appearance = Rc::new(RefCell::new(CONFIG.appearance().as_ref().clone()));
        let voice_events = voice.as_ref().map(VoiceController::events);
        let voice_activity = voice
            .as_ref()
            .map(VoiceController::activity)
            .unwrap_or_default();
        let voice_revision = voice.as_ref().map_or(0, VoiceController::current_revision);
        let shortcut_settings = CONFIG.shortcut_settings();
        let (shortcut_manager, shortcut_errors) = match ShortcutManager::new(
            shortcut_settings.as_ref().clone(),
            window,
            shortcut_runtime,
        ) {
            Ok((manager, errors)) => (Some(manager), errors),
            Err(error) => {
                log::warn!("全局快捷键运行时不可用：{error}");
                let errors = vec![error.clone()];
                (None, errors)
            }
        };
        let shortcut_runtime_errors = shortcut_errors.clone();
        if !shortcut_errors.is_empty() {
            config.update(cx, |config, cx| {
                config.report_shortcut_runtime_errors(shortcut_errors, cx);
            });
        }
        let shortcut_events = shortcut_manager.as_ref().map(ShortcutManager::events);
        let chat_overlay_visible = chat.read(cx).reply_visible();
        let chat_subscription = cx.observe(&chat, |this, chat, cx| {
            let visible = chat.read(cx).reply_visible();
            if this.chat_overlay_visible != visible {
                this.chat_overlay_visible = visible;
                cx.notify();
            }
        });
        let agent_event_subscription =
            cx.subscribe(&chat, |this, _, event: &AgentViewEvent, cx| match event {
                AgentViewEvent::ChangeOutfit(request) => {
                    let request = request.clone();
                    cx.spawn(async move |this, cx| {
                        let _ = this.update(cx, |this, cx| {
                            this.apply_agent_outfit_request(&request, cx);
                        });
                    })
                    .detach();
                }
                AgentViewEvent::StopSpeech => {
                    if let Some(playback) = &this.speech_playback {
                        playback.stop();
                    }
                }
                AgentViewEvent::SpeechAudio {
                    samples,
                    sample_rate,
                } => {
                    if this.desktop_pet_visible
                        && let Some(playback) = &this.speech_playback
                    {
                        playback.play(samples.clone(), *sample_rate);
                    }
                }
            });
        let config_subscription =
            cx.subscribe(&config, |this, _, event: &SettingsEvent, cx| match event {
                SettingsEvent::ModelChanged(model_path) => {
                    this.reload_model(model_path.clone(), cx);
                }
                SettingsEvent::ModelCatalogChanged => {}
                SettingsEvent::FrameRateChanged => this.wake_frame_rate_scheduler(),
                SettingsEvent::EyeTrackingChanged(enabled) => {
                    this.eye_tracking_enabled = *enabled;
                    if !*enabled {
                        this.reset_look_target();
                    }
                    this.sync_cursor_tracking_task(cx);
                    cx.notify();
                }
                SettingsEvent::ShowFpsChanged(show) => this.set_show_fps(*show, cx),
                SettingsEvent::NativeTrayMenuChanged(enabled) => {
                    if let Some(tray) = &this.system_tray {
                        tray.set_use_native_menu(*enabled);
                    }
                    if *enabled {
                        this.close_tray_menu(cx);
                    }
                }
                SettingsEvent::ModelWindowSizeChanged(size) => {
                    this.pending_model_window_size = Some(*size);
                    this.pending_model_window_size_attempts = 0;
                    cx.notify();
                }
                SettingsEvent::PreviewMotion(group) => {
                    if let Some(sender) = &this.model_commands
                        && sender
                            .try_send(ModelCommand::PreviewMotion(group.clone()))
                            .is_ok()
                    {
                        this.wake_model();
                    }
                }
                SettingsEvent::PreviewExpression(name) => {
                    if let Some(sender) = &this.model_commands
                        && sender
                            .try_send(ModelCommand::PreviewExpression(name.clone()))
                            .is_ok()
                    {
                        this.wake_model();
                    }
                }
                SettingsEvent::ResetExpression => {
                    let sent = this.model_commands.as_ref().is_some_and(|sender| {
                        sender.try_send(ModelCommand::ResetExpression).is_ok()
                    });
                    if sent {
                        this.wake_model();
                    } else {
                        this.reload_model(this.selected_model.clone(), cx);
                    }
                }
                SettingsEvent::AgentChanged => {
                    // Provider 或人格变化会改变转写文本的发送目标；正在录制的旧 generation
                    // 必须先失效，不能在刷新后重新绑定到新的 Agent 配置。
                    let voice_settings = CONFIG.voice_settings();
                    this.apply_voice_settings(&voice_settings, cx);
                    this.chat.update(cx, |chat, cx| {
                        chat.refresh_settings(CONFIG.agent_config_snapshot(), cx);
                    });
                }
                SettingsEvent::AgentOutfitToolChanged(enabled) => {
                    if *enabled {
                        this.sync_agent_outfits(cx);
                    } else {
                        this.clear_agent_outfits(cx);
                    }
                }
                SettingsEvent::ModelResourcesChanged => this.sync_agent_outfits(cx),
                SettingsEvent::WindowPositionsReset => {
                    this.position_controller.request_reset();
                    cx.notify();
                }
                SettingsEvent::AppearanceChanged(settings) => {
                    *this.appearance.borrow_mut() = settings.clone();
                    apply_language(settings.language);
                    this.model_state.refresh_localized_warning();
                    apply(settings, None, cx);
                    this.sync_system_tray_appearance(cx);
                    this.sync_agent_outfits(cx);
                    this.chat.update(cx, |chat, cx| {
                        chat.refresh_settings(CONFIG.agent_config_snapshot(), cx);
                    });
                }
                SettingsEvent::VoiceChanged(settings) => {
                    this.apply_voice_settings(settings, cx);
                }
                SettingsEvent::ShortcutsChanged(settings) => {
                    this.apply_shortcut_settings(settings, cx);
                }
                SettingsEvent::ShortcutRecordingChanged(recording) => {
                    this.set_shortcut_recording(*recording, cx);
                }
            });
        cache_window_position(window, ConfigWindow::DesktopPet);
        let bounds_subscription = cx.observe_window_bounds(window, |this, window, _| {
            if !this.position_controller.observe_bounds() {
                return;
            }
            cache_window_position(window, ConfigWindow::DesktopPet);
        });
        let activation_subscription = cx.observe_window_activation(window, |this, window, _| {
            if !window.is_window_active() {
                this.release_voice_shortcut();
            }
        });
        let tray_for_appearance = system_tray.clone();
        let appearance_for_observer = appearance.clone();
        let appearance_subscription = window.observe_window_appearance(move |window, cx| {
            let appearance = appearance_for_observer.borrow().clone();
            if appearance.theme == ThemePreset::System {
                apply(&appearance, Some(window), cx);
                sync_system_tray_appearance(tray_for_appearance.as_deref(), cx);
            }
        });
        let mut view = Self {
            frame: None,
            current_rendered_image: None,
            previous_rendered_image: None,
            look_target,
            global_cursor_tracker,
            cursor_tracking_task: None,
            eye_tracking_enabled: CONFIG.eye_tracking(),
            show_fps: CONFIG.show_fps(),
            desktop_pet_visible: true,
            visibility_revision: 0,
            actual_fps: 0.0,
            frame_rate_meter: FrameRateMeter::new(),
            fps_task: None,
            model_commands: None,
            model_wake: None,
            frame_rate_wake: None,
            model_cancellation: None,
            gpu_underlay,
            gpu_event_task: None,
            gpu_shutdown_pending: false,
            gpu_shutdown_restart_cpu: false,
            gpu_released_for_hidden: false,
            gpu_restore_pending: false,
            cpu_fallback_pending,
            close_after_gpu_shutdown: false,
            quitting: false,
            gpu_shutdown_completion: None,
            window_mover: WindowMover::new(),
            system_tray,
            appearance,
            config,
            chat,
            voice,
            speech_playback,
            voice_mode: CONFIG.voice_settings().mode,
            voice_revision,
            voice_activity,
            voice_event_task: None,
            voice_level_task: None,
            voice_shortcut_release_task: None,
            shortcut_manager,
            shortcut_runtime_errors,
            shortcut_event_task: None,
            chat_input_open: false,
            chat_overlay_visible,
            position_controller: WindowPositionController::default(),
            pending_model_window_size: None,
            pending_model_window_size_attempts: 0,
            config_window: None,
            tray_menu_window: None,
            selected_model: None,
            model_state: ModelLoadState::NoModel,
            raster_dimensions: if gpu_events.is_some() {
                gpu_underlay_size.physical
            } else {
                raster_dimensions
            },
            cpu_raster_dimensions: raster_dimensions,
            gpu_underlay_size,
            model_generation: 0,
            model_task: None,
            _config_subscription: config_subscription,
            _chat_subscription: chat_subscription,
            _agent_event_subscription: agent_event_subscription,
            _bounds_subscription: bounds_subscription,
            _activation_subscription: activation_subscription,
            _appearance_subscription: appearance_subscription,
        };
        view.sync_system_tray_appearance(cx);
        if view.show_fps {
            view.start_fps_task(cx);
        }
        view.start_gpu_event_task(cx);
        if let Some(events) = voice_events {
            view.voice_event_task = Some(cx.spawn(async move |this, cx| {
                while let Ok(event) = events.recv().await {
                    let keep_running = this
                        .update(cx, |this, cx| this.handle_voice_event(event, cx))
                        .unwrap_or(false);
                    if !keep_running {
                        break;
                    }
                }
            }));
        }
        if let Some(events) = shortcut_events {
            view.shortcut_event_task = Some(cx.spawn(async move |this, cx| {
                while let Ok(event) = events.recv().await {
                    let keep_running = this
                        .update_in(cx, |this, window, cx| {
                            this.handle_shortcut_event(event, window, cx)
                        })
                        .unwrap_or(false);
                    if !keep_running {
                        break;
                    }
                }
            }));
        }
        view.load_model(initial_model, cx);
        view.sync_cursor_tracking_task(cx);
        view
    }

    fn start_gpu_event_task(&mut self, cx: &mut Context<Self>) {
        if self.gpu_event_task.is_some() {
            return;
        }
        let Some(events) = self.gpu_underlay.as_ref().map(GpuUnderlay::events) else {
            return;
        };
        self.gpu_event_task = Some(cx.spawn(async move |this, cx| {
            loop {
                match events.recv().await {
                    Ok(event) => {
                        let keep_running = this
                            .update(cx, |this, cx| this.handle_gpu_event(event, cx))
                            .unwrap_or(false);
                        if !keep_running {
                            let _ = this.update(cx, |this, _| {
                                this.gpu_event_task = None;
                            });
                            return;
                        }
                    }
                    Err(_) => {
                        let _ = this.update(cx, |this, cx| {
                            if this.gpu_underlay.is_some() {
                                log::warn!("{}", t!("log.gpu_worker_exited"));
                                this.fallback_to_cpu(cx);
                            }
                            this.gpu_event_task = None;
                        });
                        return;
                    }
                }
            }
        }));
    }

    fn apply_voice_settings(&mut self, settings: &VoiceSettings, cx: &mut Context<Self>) {
        self.release_voice_shortcut();
        self.voice_mode = settings.mode;
        if let Some(voice) = &self.voice {
            let mut runtime_settings = settings.runtime(CONFIG.llm_settings().as_ref());
            if !self.desktop_pet_visible {
                runtime_settings.mode = VoiceMode::Off;
            }
            self.voice_revision = voice.configure(runtime_settings);
            self.voice_activity = VoiceActivitySnapshot::default();
        } else {
            self.voice_revision = 0;
            self.voice_activity = VoiceActivitySnapshot::default();
        }
        self.chat.update(cx, |chat, cx| {
            chat.cancel_pending_voice();
            chat.set_voice_indicator_visible(false, cx);
        });
        if let Some(playback) = &self.speech_playback {
            playback.stop();
        }
        self.sync_voice_level_task(cx);
        cx.notify();
    }

    fn apply_shortcut_settings(
        &mut self,
        settings: &crate::config::ShortcutSettings,
        cx: &mut Context<Self>,
    ) {
        self.release_voice_shortcut();
        let (errors, asynchronous) = if let Some(manager) = &mut self.shortcut_manager {
            let errors = manager.configure(settings.clone());
            (errors, manager.reports_status_asynchronously())
        } else {
            (self.shortcut_runtime_errors.clone(), false)
        };
        if !asynchronous || !errors.is_empty() {
            self.report_shortcut_runtime_errors(errors, cx);
        }
    }

    fn set_shortcut_recording(&mut self, recording: bool, cx: &mut Context<Self>) {
        if recording {
            self.release_voice_shortcut();
        }
        if let Some(manager) = &mut self.shortcut_manager {
            let errors = manager.set_suspended(recording);
            if !manager.reports_status_asynchronously() && (!errors.is_empty() || !recording) {
                self.report_shortcut_runtime_errors(errors, cx);
            }
        } else {
            self.report_shortcut_runtime_errors(self.shortcut_runtime_errors.clone(), cx);
        }
    }

    fn report_shortcut_runtime_errors(&mut self, errors: Vec<String>, cx: &mut Context<Self>) {
        if !errors.is_empty() {
            log::warn!("全局快捷键配置存在运行时错误：count={}", errors.len());
        }
        self.shortcut_runtime_errors = errors.clone();
        self.config.update(cx, |config, cx| {
            config.report_shortcut_runtime_errors(errors, cx);
        });
    }

    fn handle_shortcut_event(
        &mut self,
        event: ShortcutEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let runtime_bindings = self
            .shortcut_manager
            .as_ref()
            .and_then(|manager| manager.runtime_bindings(&event))
            .map(|bindings| bindings.to_vec());
        if let Some(bindings) = runtime_bindings {
            self.config.update(cx, |config, cx| {
                config.report_shortcut_runtime_bindings(bindings, cx);
            });
            return true;
        }
        let runtime_errors = self
            .shortcut_manager
            .as_ref()
            .and_then(|manager| manager.runtime_errors(&event))
            .map(|errors| errors.to_vec());
        if let Some(errors) = runtime_errors {
            self.report_shortcut_runtime_errors(errors, cx);
            return true;
        }
        let Some(event) = self
            .shortcut_manager
            .as_ref()
            .and_then(|manager| manager.resolve(&event))
        else {
            return true;
        };
        let action = event.action();
        let should_activate_main_window = event.is_pressed()
            && match action {
                crate::config::ShortcutAction::ToggleDesktopPet => !self.desktop_pet_visible,
                crate::config::ShortcutAction::ToggleChatInput => {
                    !self.desktop_pet_visible || !self.chat_input_open
                }
                crate::config::ShortcutAction::VoiceInput
                | crate::config::ShortcutAction::ToggleSettings => false,
            };
        if should_activate_main_window
            && let Some(token) = event.activation_token()
            && let Some(manager) = &self.shortcut_manager
            && let Err(error) = manager.activate_wayland(token.to_owned())
        {
            log::warn!("提交 Wayland 快捷键窗口激活失败：{error}");
        }
        if action == crate::config::ShortcutAction::VoiceInput {
            self.set_voice_shortcut_pressed(event.is_pressed(), cx);
            return true;
        }
        if !event.is_pressed() {
            return true;
        }
        match action {
            crate::config::ShortcutAction::VoiceInput => {}
            crate::config::ShortcutAction::ToggleDesktopPet => {
                if let Err(error) = self.toggle_desktop_pet_visibility(window, cx) {
                    log::warn!("全局快捷键切换桌宠显隐失败：{error}");
                }
            }
            crate::config::ShortcutAction::ToggleSettings => self.toggle_config_window(cx),
            crate::config::ShortcutAction::ToggleChatInput => {
                self.toggle_chat_input_from_shortcut(window, cx);
            }
        }
        true
    }

    fn handle_voice_event(&mut self, event: VoiceEvent, cx: &mut Context<Self>) -> bool {
        match event {
            VoiceEvent::ActivityChanged { revision } if revision == self.voice_revision => {
                if let Some(voice) = &self.voice {
                    self.voice_activity = voice.activity();
                    let recording = self.voice_activity.phase == VoicePhase::Recording;
                    self.chat.update(cx, |chat, cx| {
                        chat.set_voice_indicator_visible(recording, cx);
                    });
                    self.sync_voice_level_task(cx);
                    cx.notify();
                }
            }
            VoiceEvent::SpeechStarted {
                revision,
                utterance_id,
            } if revision == self.voice_revision => {
                if let Some(playback) = &self.speech_playback {
                    playback.stop();
                }
                let language = self.appearance.borrow().language;
                self.chat.update(cx, |chat, cx| {
                    chat.voice_speech_started(utterance_id, language, cx);
                });
            }
            VoiceEvent::UtteranceCancelled {
                revision,
                utterance_id,
            } if revision == self.voice_revision => {
                self.chat.update(cx, |chat, _| {
                    chat.voice_utterance_cancelled(utterance_id);
                });
            }
            VoiceEvent::TranscriptReady {
                revision,
                utterance_id,
                text,
            } if revision == self.voice_revision => {
                self.chat.update(cx, |chat, cx| {
                    chat.send_voice_transcript(utterance_id, text, cx);
                });
            }
            VoiceEvent::TranscriptionRequested {
                revision,
                utterance_id,
                model_id,
                samples,
            } if revision == self.voice_revision => {
                if let Some(voice) = self.voice.clone() {
                    self.chat.update(cx, |chat, cx| {
                        chat.transcribe_voice(revision, utterance_id, model_id, samples, voice, cx);
                    });
                }
            }
            VoiceEvent::Error { revision, message } if revision == self.voice_revision => {
                self.chat.update(cx, |chat, cx| {
                    chat.voice_failed(message, cx);
                });
            }
            _ => {}
        }
        true
    }

    fn sync_voice_level_task(&mut self, cx: &mut Context<Self>) {
        if self.voice_activity.phase != VoicePhase::Recording {
            self.voice_level_task = None;
            return;
        }
        if self.voice_level_task.is_some() {
            return;
        }
        let Some(voice) = self.voice.clone() else {
            return;
        };
        let background = cx.background_executor().clone();
        self.voice_level_task = Some(cx.spawn(async move |this, cx| {
            loop {
                background.timer(VOICE_LEVEL_REFRESH_INTERVAL).await;
                let keep_running = this
                    .update(cx, |this, cx| {
                        this.voice_activity = voice.activity();
                        if this.voice_activity.phase != VoicePhase::Recording {
                            return false;
                        }
                        cx.notify();
                        true
                    })
                    .unwrap_or(false);
                if !keep_running {
                    break;
                }
            }
        }));
    }

    pub(in crate::ui) fn set_push_to_talk(&self, pressed: bool) {
        if self.voice_mode.supports_push_to_talk()
            && let Some(voice) = &self.voice
        {
            voice.set_push_to_talk(pressed);
        }
    }

    fn release_voice_shortcut(&mut self) {
        self.voice_shortcut_release_task = None;
        self.set_push_to_talk(false);
    }

    fn set_voice_shortcut_pressed(&mut self, pressed: bool, cx: &mut Context<Self>) {
        self.release_voice_shortcut();
        if !pressed || !self.voice_mode.supports_push_to_talk() {
            return;
        }
        self.set_push_to_talk(true);
        let background = cx.background_executor().clone();
        self.voice_shortcut_release_task = Some(cx.spawn(async move |this, cx| {
            background.timer(VOICE_SHORTCUT_RELEASE_TIMEOUT).await;
            let _ = this.update(cx, |this, _| {
                log::warn!("语音输入快捷键超过按住时限，已强制结束录音");
                this.release_voice_shortcut();
            });
        }));
    }

    fn update_look_target(&self, position: Point<Pixels>, window: &Window) {
        if !self.eye_tracking_enabled || self.frame.is_none() || self.chat_input_open {
            return;
        }
        let look = look_target_for_position(position, window.viewport_size());
        let mut target = self.look_target.lock();
        if *target == look {
            return;
        }
        *target = look;
        drop(target);
        self.wake_model();
    }

    fn update_global_cursor_target(&self, window: &Window) {
        let position = self
            .global_cursor_tracker
            .as_ref()
            .and_then(|tracker| tracker.position(window));
        if let Some(position) = position {
            self.update_look_target(position, window);
        } else {
            self.reset_look_target();
        }
    }

    fn handle_mouse_exit(&self, window: &Window) {
        self.update_global_cursor_target(window);
    }

    fn should_track_global_cursor(&self) -> bool {
        self.global_cursor_tracker.is_some()
            && self.eye_tracking_enabled
            && self.desktop_pet_visible
            && !self.chat_input_open
            && self.frame.is_some()
            && !self.close_after_gpu_shutdown
            && !self.quitting
            && !self.gpu_shutdown_pending
    }

    fn sync_cursor_tracking_task(&mut self, cx: &mut Context<Self>) {
        if !self.should_track_global_cursor() {
            self.cursor_tracking_task = None;
            return;
        }
        if self.cursor_tracking_task.is_some() {
            return;
        }

        let background = cx.background_executor().clone();
        self.cursor_tracking_task = Some(cx.spawn(async move |this, cx| {
            loop {
                background.timer(CURSOR_TRACKING_INTERVAL).await;
                let keep_running = this
                    .update_in(cx, |this, window, _| {
                        if !this.should_track_global_cursor() {
                            return false;
                        }
                        this.update_global_cursor_target(window);
                        true
                    })
                    .unwrap_or(false);
                if !keep_running {
                    break;
                }
            }
        }));
    }

    fn reset_look_target(&self) {
        let mut target = self.look_target.lock();
        if *target == [0.0, 0.0] {
            return;
        }
        *target = [0.0, 0.0];
        drop(target);
        self.wake_model();
    }

    fn clear_agent_outfits(&self, cx: &mut Context<Self>) {
        self.chat.update(cx, |chat, _| {
            chat.set_available_outfits(Vec::new());
        });
    }

    fn sync_agent_outfits(&self, cx: &mut Context<Self>) {
        let outfits = if matches!(self.model_state, ModelLoadState::Ready { .. }) {
            self.config.read(cx).available_agent_outfits()
        } else {
            Vec::new()
        };
        self.chat.update(cx, |chat, _| {
            chat.set_available_outfits(outfits);
        });
    }

    fn apply_agent_outfit_request(&mut self, request: &AgentOutfitRequest, cx: &mut Context<Self>) {
        if !self.desktop_pet_visible
            || !CONFIG.allow_agent_outfit_change()
            || !self.chat.read(cx).outfit_request_is_current(request)
        {
            request.complete(false);
            return;
        }
        let Some(action) = self
            .config
            .read(cx)
            .resolve_agent_outfit(request.outfit_id())
        else {
            request.complete(false);
            return;
        };
        let applied = match action {
            AgentOutfitAction::Unchanged => true,
            action @ AgentOutfitAction::LoadVariant(_) => {
                match self
                    .config
                    .update(cx, |config, cx| config.commit_agent_outfit(action, cx))
                {
                    Ok(Some(model_path)) => {
                        self.load_model(Some(model_path), cx);
                        true
                    }
                    Ok(None) | Err(_) => false,
                }
            }
            AgentOutfitAction::PreviewExpression(name) => {
                let sent = self.model_commands.as_ref().is_some_and(|sender| {
                    sender
                        .try_send(ModelCommand::PreviewExpression(name.clone()))
                        .is_ok()
                });
                if sent {
                    self.wake_model();
                    self.config
                        .update(cx, |config, cx| {
                            config
                                .commit_agent_outfit(AgentOutfitAction::PreviewExpression(name), cx)
                        })
                        .is_ok()
                } else {
                    false
                }
            }
            AgentOutfitAction::ResetExpression => {
                let sent = self
                    .model_commands
                    .as_ref()
                    .is_some_and(|sender| sender.try_send(ModelCommand::ResetExpression).is_ok());
                if sent {
                    self.wake_model();
                    self.config
                        .update(cx, |config, cx| {
                            config.commit_agent_outfit(AgentOutfitAction::ResetExpression, cx)
                        })
                        .is_ok()
                } else {
                    false
                }
            }
        };
        request.complete(applied);
    }

    fn wake_model(&self) {
        if let Some(underlay) = &self.gpu_underlay {
            underlay.wake();
        }
        if let Some(wake) = &self.model_wake {
            wake.wake();
        }
    }

    fn wake_frame_rate_scheduler(&self) {
        if let Some(underlay) = &self.gpu_underlay {
            underlay.wake();
        }
        if let Some(wake) = &self.frame_rate_wake {
            wake.wake();
        }
    }

    fn handle_gpu_event(&mut self, event: GpuUnderlayEvent, cx: &mut Context<Self>) -> bool {
        match event {
            GpuUnderlayEvent::ModelLoaded {
                generation,
                frame,
                presented_at,
                presented_frames,
                diagnostics,
                capabilities,
            } => {
                if self.model_generation != generation || self.gpu_underlay.is_none() {
                    return true;
                }
                // ModelLoaded 自带首帧端点。即使 latest slot 已被后续帧覆盖，也先记录
                // 这条基线，累计差值才能恢复 UI 延迟期间已经合并的全部 present。
                self.record_gpu_presented_frames(presented_at, presented_frames);
                let presented = self
                    .gpu_underlay
                    .as_ref()
                    .and_then(GpuUnderlay::take_presented_frame)
                    .and_then(
                        |(
                            presented_generation,
                            presented_frame,
                            presented_at,
                            presented_frames,
                        )| {
                            (presented_generation == generation).then_some((
                                presented_frame,
                                presented_at,
                                presented_frames,
                            ))
                        },
                    );
                let frame = if let Some((frame, presented_at, presented_frames)) = presented {
                    self.record_gpu_presented_frames(presented_at, presented_frames);
                    frame
                } else {
                    frame
                };
                if self.desktop_pet_visible {
                    self.frame = Some(Arc::new(frame));
                }
                let diagnostic_count = diagnostics.entries().len();
                let movable_expression_count = capabilities
                    .expressions()
                    .iter()
                    .filter(|expression| expression.movable_to_outfit())
                    .count();
                let motion_count = capabilities
                    .motions()
                    .len()
                    .saturating_add(capabilities.idle_motions().len());
                let expression_count = capabilities.expressions().len();
                self.model_state = ModelLoadState::ready(diagnostics);
                log::info!(
                    "Live2D 模型已就绪：generation={generation}, renderer=gpu, movable_expressions={movable_expression_count}, motions={motion_count}, expressions={expression_count}, diagnostics={diagnostic_count}"
                );
                if diagnostic_count > 0 {
                    log::warn!(
                        "Live2D 模型存在非致命能力问题：generation={generation}, diagnostics={diagnostic_count}"
                    );
                }
                self.cpu_fallback_pending = false;
                self.sync_cursor_tracking_task(cx);
                self.config.update(cx, |config, cx| {
                    config.set_preview_capabilities(capabilities, cx);
                });
                self.sync_agent_outfits(cx);
                cx.notify();
            }
            GpuUnderlayEvent::FrameAvailable { generation } => {
                if self.model_generation != generation || self.gpu_underlay.is_none() {
                    return true;
                }
                let presented = self
                    .gpu_underlay
                    .as_ref()
                    .and_then(GpuUnderlay::take_presented_frame);
                if let Some((generation, frame, presented_at, presented_frames)) = presented
                    && self.model_generation == generation
                    && self.desktop_pet_visible
                {
                    let needs_notify = self.frame.is_none();
                    self.frame = Some(Arc::new(frame));
                    self.record_gpu_presented_frames(presented_at, presented_frames);
                    if needs_notify {
                        self.sync_cursor_tracking_task(cx);
                        cx.notify();
                    }
                }
            }
            GpuUnderlayEvent::ModelLoadFailed { generation, error } => {
                if self.model_generation != generation || self.gpu_underlay.is_none() {
                    return true;
                }
                let model_name = self
                    .selected_model
                    .as_deref()
                    .map(model_display_name)
                    .unwrap_or_else(|| t!("model_state.unnamed").to_string());
                log::warn!(
                    "Live2D 模型加载失败：generation={generation}, renderer=gpu, stage=model_load"
                );
                self.model_state = ModelLoadState::Failed(
                    t!("model_state.load_failed", name = model_name, error = error).to_string(),
                );
                self.frame = None;
                self.sync_cursor_tracking_task(cx);
                self.config.update(cx, |config, cx| {
                    config.set_preview_capabilities(ModelPreviewCapabilities::default(), cx);
                });
                self.clear_agent_outfits(cx);
                self.model_commands = None;
                cx.notify();
            }
            GpuUnderlayEvent::ModelGpuFailed { generation, error } => {
                if self.model_generation != generation || self.gpu_underlay.is_none() {
                    return true;
                }
                let _ = error;
                log::warn!(
                    "Live2D GPU 模型渲染失败，正在回退 CPU：generation={generation}, stage=model_render"
                );
                self.fallback_to_cpu(cx);
                return false;
            }
            GpuUnderlayEvent::Unavailable { kind } => {
                log::warn!(
                    "Live2D GPU underlay 不可用，正在回退 CPU：generation={}, stage=underlay_runtime, reason={}",
                    self.model_generation,
                    kind.id()
                );
                self.fallback_to_cpu(cx);
                return false;
            }
        }
        true
    }

    fn fallback_to_cpu(&mut self, cx: &mut Context<Self>) {
        self.cpu_fallback_pending = true;
        if let Some(cancellation) = self.model_cancellation.take() {
            cancellation.cancel();
        }
        self.raster_dimensions = self.cpu_raster_dimensions;
        // 退出过程中 GPU worker 失败无需重建 CPU generation，且不能再 spawn 前台任务。
        if self.quitting {
            self.shutdown_gpu_for_quit();
            return;
        }
        if self.gpu_shutdown_pending {
            self.gpu_shutdown_restart_cpu = true;
            return;
        }
        let model_path = self.selected_model.clone();
        let generation = self.model_generation;
        self.gpu_shutdown_restart_cpu = true;
        self.begin_gpu_shutdown(generation, model_path, cx);
    }

    /// 请求关闭窗口；存在 GPU worker 时先异步回收 surface，再移除原生窗口。
    pub(crate) fn request_window_close(&mut self, cx: &mut Context<Self>) -> bool {
        log::info!(
            "桌宠窗口收到关闭请求：generation={}, renderer={}",
            self.model_generation,
            if self.gpu_underlay.is_some() {
                "gpu"
            } else {
                "cpu"
            }
        );
        self.release_voice_shortcut();
        if let Some(voice) = &self.voice {
            voice.request_shutdown();
        }
        self.voice_activity = VoiceActivitySnapshot::default();
        self.voice_level_task = None;
        self.chat.update(cx, |chat, cx| {
            chat.cancel_pending_voice();
            chat.set_voice_indicator_visible(false, cx);
        });
        if let Some(playback) = &self.speech_playback {
            playback.stop();
        }
        self.gpu_shutdown_restart_cpu = false;
        self.close_after_gpu_shutdown = true;
        self.sync_cursor_tracking_task(cx);
        if let Some(cancellation) = self.model_cancellation.take() {
            cancellation.cancel();
        }
        if let Some(wake) = self.model_wake.take() {
            wake.close();
        }
        if let Some(wake) = self.frame_rate_wake.take() {
            wake.close();
        }
        if self.gpu_shutdown_pending {
            return false;
        }
        if self.gpu_underlay.is_none() {
            return true;
        }
        self.begin_gpu_shutdown(self.model_generation, None, cx);
        false
    }

    /// 在 GPUI 清空原生窗口前同步确认 GPU surface 已经释放。
    pub(crate) fn shutdown_gpu_for_quit(&mut self) {
        self.release_voice_shortcut();
        if let Some(voice) = &self.voice {
            voice.request_shutdown();
        }
        self.gpu_shutdown_restart_cpu = false;
        // 退出后 GPUI 禁止再向前台执行器投递任务；后续 GPU 事件必须同步收尾。
        self.quitting = true;
        self.cursor_tracking_task = None;
        if self.gpu_shutdown_pending {
            if let Some(completion) = &self.gpu_shutdown_completion
                && !completion.wait_with_timeout(GPU_SHUTDOWN_WAIT_TIMEOUT)
            {
                log::error!("{}", t!("log.gpu_shutdown_wait_timeout"));
            }
            return;
        }
        if let Some(cancellation) = self.model_cancellation.take() {
            cancellation.cancel();
        }
        if let Some(wake) = self.model_wake.take() {
            wake.close();
        }
        if let Some(wake) = self.frame_rate_wake.take() {
            wake.close();
        }
        if let Some(mut underlay) = self.gpu_underlay.take() {
            underlay.shutdown();
        }
    }

    fn begin_gpu_shutdown(
        &mut self,
        generation: u64,
        model_path: Option<PathBuf>,
        cx: &mut Context<Self>,
    ) {
        let Some(mut underlay) = self.gpu_underlay.take() else {
            if self.gpu_shutdown_restart_cpu && self.model_generation == generation {
                self.selected_model = None;
                self.load_model(model_path, cx);
            }
            return;
        };
        log::debug!("正在停止 Live2D GPU worker：generation={generation}");
        self.gpu_event_task = None;
        let worker = underlay.request_shutdown();
        let completion = Arc::new(GpuShutdownCompletion::default());
        self.gpu_shutdown_pending = true;
        self.sync_cursor_tracking_task(cx);
        self.gpu_shutdown_completion = Some(completion.clone());

        // 立即提交后台 join：退出时 quit observer 先于前台执行器运行，延迟提交会让
        // `shutdown_gpu_for_quit` 永远等不到 `complete()`。
        let join_worker = cx.background_executor().spawn(async move {
            let worker_panicked = worker.is_some_and(|worker| worker.join().is_err());
            completion.complete();
            worker_panicked
        });

        // attachment 留在前台 future 中；后台只等待线程，确保原生 view 晚于 surface 析构。
        cx.spawn(async move |this, cx| {
            let worker_panicked = join_worker.await;
            drop(underlay);
            if worker_panicked {
                log::error!("{}", t!("log.gpu_worker_panicked"));
            }
            let _ = this.update_in(cx, |this, window, cx| {
                this.gpu_shutdown_pending = false;
                this.gpu_shutdown_completion = None;
                log::debug!("Live2D GPU worker 已完成回收：generation={generation}");
                if this.close_after_gpu_shutdown {
                    window.remove_window();
                } else if this.gpu_restore_pending && this.desktop_pet_visible {
                    this.gpu_restore_pending = false;
                    this.restore_gpu_underlay(window, cx);
                } else if this.gpu_shutdown_restart_cpu && this.model_generation == generation {
                    this.selected_model = None;
                    this.load_model(model_path, cx);
                }
            });
        })
        .detach();
    }

    fn record_presented_frame(&mut self) {
        if !self.show_fps || !self.desktop_pet_visible {
            return;
        }
        let now = Instant::now();
        self.frame_rate_meter.record(now);
    }

    fn record_gpu_presented_frames(&mut self, presented_at: Instant, presented_frames: u64) {
        if !self.show_fps || !self.desktop_pet_visible {
            return;
        }
        self.frame_rate_meter
            .record_cumulative(presented_at, presented_frames);
    }

    fn set_show_fps(&mut self, show: bool, cx: &mut Context<Self>) {
        if self.show_fps == show {
            return;
        }
        self.show_fps = show;
        self.frame_rate_meter.reset();
        self.actual_fps = 0.0;
        self.fps_task = None;
        if show && self.desktop_pet_visible {
            self.start_fps_task(cx);
        }
        cx.notify();
    }

    fn start_fps_task(&mut self, cx: &mut Context<Self>) {
        if self.fps_task.is_some() || !self.desktop_pet_visible {
            return;
        }
        let background = cx.background_executor().clone();
        self.fps_task = Some(cx.spawn(async move |this, cx| {
            loop {
                background.timer(FPS_REFRESH_INTERVAL).await;
                let keep_running = this
                    .update(cx, |this, cx| {
                        if !this.show_fps || !this.desktop_pet_visible {
                            return false;
                        }
                        this.actual_fps = this.frame_rate_meter.sample(Instant::now());
                        cx.notify();
                        true
                    })
                    .unwrap_or(false);
                if !keep_running {
                    break;
                }
            }
        }));
    }

    fn restore_gpu_underlay(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.gpu_underlay.is_some() || !self.gpu_released_for_hidden || self.quitting {
            return;
        }
        let underlay = match GpuUnderlay::attach(window) {
            Ok(Some(underlay)) => underlay,
            Ok(None) => {
                log::info!("桌宠恢复显示时 GPU underlay 不可用，正在重建 CPU renderer");
                self.restore_cpu_after_hidden(cx);
                return;
            }
            Err(error) => {
                log::warn!("桌宠恢复显示时 GPU underlay 创建失败：{error}");
                self.restore_cpu_after_hidden(cx);
                return;
            }
        };
        self.gpu_underlay = Some(underlay);
        self.gpu_shutdown_restart_cpu = false;
        self.gpu_restore_pending = false;
        self.gpu_released_for_hidden = false;
        self.start_gpu_event_task(cx);
        self.reload_model(self.selected_model.clone(), cx);
    }

    fn restore_cpu_after_hidden(&mut self, cx: &mut Context<Self>) {
        self.gpu_shutdown_restart_cpu = false;
        self.gpu_restore_pending = false;
        self.gpu_released_for_hidden = false;
        if self.model_task.is_none() && self.selected_model.is_some() {
            self.reload_model(self.selected_model.clone(), cx);
        }
    }

    /// 在原生窗口显隐成功后同步模型、语音和 Agent 生命周期。
    pub(crate) fn set_desktop_pet_visible(
        &mut self,
        visible: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.desktop_pet_visible == visible {
            return;
        }

        self.desktop_pet_visible = visible;
        if let Some(tray) = &self.system_tray {
            tray.set_desktop_pet_hidden(!visible);
        }
        log::info!(
            "桌宠窗口显隐已更新：visible={visible}, generation={}",
            self.model_generation
        );
        self.visibility_revision = self.visibility_revision.wrapping_add(1);
        self.chat.update(cx, |chat, cx| {
            if visible {
                chat.resume_after_hidden();
            } else {
                chat.suspend_for_hidden(cx);
            }
        });
        let voice_settings = CONFIG.voice_settings();
        self.apply_voice_settings(&voice_settings, cx);
        self.frame_rate_meter.reset();
        self.actual_fps = 0.0;
        if let Some(wake) = &self.frame_rate_wake {
            wake.wake();
        }

        if visible {
            self.wake_model();
            if self.show_fps {
                self.start_fps_task(cx);
            }
            if self.gpu_released_for_hidden {
                if self.gpu_shutdown_pending {
                    self.gpu_restore_pending = true;
                    self.gpu_shutdown_restart_cpu = false;
                } else {
                    self.restore_gpu_underlay(window, cx);
                }
            }
        } else {
            self.fps_task = None;
            self.gpu_restore_pending = false;
            if self.gpu_shutdown_pending && self.gpu_released_for_hidden {
                self.gpu_shutdown_restart_cpu = true;
            }
            self.release_rendered_images(window);
            if self.gpu_underlay.is_some() && !self.gpu_shutdown_pending {
                if let Some(underlay) = &self.gpu_underlay {
                    underlay.set_paused(true);
                }
                self.gpu_released_for_hidden = true;
                self.gpu_shutdown_restart_cpu = true;
                self.begin_gpu_shutdown(self.model_generation, self.selected_model.clone(), cx);
            }
        }
        self.sync_cursor_tracking_task(cx);
        cx.notify();
    }

    /// 切换原生桌宠窗口显隐，并只在平台请求成功后更新运行时状态。
    pub(crate) fn toggle_desktop_pet_visibility(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Result<bool, String> {
        let visible = !self.desktop_pet_visible;
        set_desktop_pet_window_visible(window, visible)?;
        self.set_desktop_pet_visible(visible, window, cx);
        Ok(visible)
    }

    /// 显隐请求失败时把原生托盘勾选恢复为视图中的权威状态。
    pub(crate) fn sync_desktop_pet_visibility_to_tray(&self) {
        if let Some(tray) = &self.system_tray {
            tray.set_desktop_pet_hidden(!self.desktop_pet_visible);
        }
    }

    fn release_rendered_images(&mut self, window: &mut Window) {
        self.frame = None;
        let current = self.current_rendered_image.take();
        if let Some(previous) = self.previous_rendered_image.take()
            && current
                .as_ref()
                .is_none_or(|current| previous.id != current.id)
            && let Err(error) = window.drop_image(previous)
        {
            log::warn!("{}", t!("log.image_release_failed", error = error));
        }
        if let Some(current) = current
            && let Err(error) = window.drop_image(current)
        {
            log::warn!("{}", t!("log.image_release_failed", error = error));
        }
    }

    fn sync_system_tray_appearance(&self, cx: &App) {
        sync_system_tray_appearance(self.system_tray.as_deref(), cx);
    }

    fn apply_pending_model_window_size(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some(window_size) = self.pending_model_window_size.take() else {
            return false;
        };
        // 桌宠可能已被拖到副屏；用所在显示器的可用区域约束尺寸，主屏只作为兜底。
        let display_size = window
            .display(cx)
            .or_else(|| cx.primary_display())
            .map(|display| display.visible_bounds().size)
            .unwrap_or_else(|| size(px(1280.0), px(720.0)));
        let [width, height] = desktop_pet_window_size(
            f32::from(display_size.width),
            f32::from(display_size.height),
            window_size,
        );
        let viewport = window.viewport_size();
        let reached_target = (f32::from(viewport.width) - width).abs() < 0.5
            && (f32::from(viewport.height) - height).abs() < 0.5;
        // 合成器可能钳制或拒绝请求的尺寸；重试有上限，否则每帧都会重新调用 resize。
        if reached_target || self.pending_model_window_size_attempts >= MAX_WINDOW_RESIZE_ATTEMPTS {
            if !reached_target {
                log::warn!(
                    "窗口后端未采用请求的桌宠尺寸，使用实际 viewport：attempts={MAX_WINDOW_RESIZE_ATTEMPTS}"
                );
            }
            let (width, height) = if reached_target {
                (width, height)
            } else {
                (f32::from(viewport.width), f32::from(viewport.height))
            };
            self.pending_model_window_size_attempts = 0;
            self.update_render_dimensions(width, height, window.scale_factor());
            let model_path = self.selected_model.clone();
            self.selected_model = None;
            self.load_model(model_path, cx);
            return true;
        }

        window.resize(size(px(width), px(height)));
        // 后端可能异步应用、取整或钳制尺寸；等待实际 viewport 变化后再重建 generation。
        self.pending_model_window_size_attempts += 1;
        self.pending_model_window_size = Some(window_size);
        true
    }

    fn synchronize_render_dimensions(&mut self, window: &Window, cx: &mut Context<Self>) {
        let viewport = window.viewport_size();
        let width = f32::from(viewport.width).max(1.0);
        let height = f32::from(viewport.height).max(1.0);
        let next_gpu_size = gpu_underlay_size(width, height, window.scale_factor());
        let next_cpu_dimensions =
            raster_dimensions_for_window(width, height, window.scale_factor());
        // 两个尺寸都要更新：GPU 失败回退到 CPU 时会直接采用 cpu_raster_dimensions。
        let active_path_changed = if self.gpu_underlay.is_some() {
            self.gpu_underlay_size != next_gpu_size
        } else {
            self.cpu_raster_dimensions != next_cpu_dimensions
        };
        self.gpu_underlay_size = next_gpu_size;
        self.cpu_raster_dimensions = next_cpu_dimensions;
        if !active_path_changed {
            return;
        }
        self.raster_dimensions = if self.gpu_underlay.is_some() {
            next_gpu_size.physical
        } else {
            next_cpu_dimensions
        };
        let model_path = self.selected_model.clone();
        self.selected_model = None;
        self.load_model(model_path, cx);
    }

    fn update_render_dimensions(&mut self, width: f32, height: f32, scale_factor: f32) {
        self.gpu_underlay_size = gpu_underlay_size(width, height, scale_factor);
        self.cpu_raster_dimensions = raster_dimensions_for_window(width, height, scale_factor);
        self.raster_dimensions = if self.gpu_underlay.is_some() {
            self.gpu_underlay_size.physical
        } else {
            self.cpu_raster_dimensions
        };
    }

    fn set_chat_input_open(&mut self, open: bool, window: &mut Window, cx: &mut Context<Self>) {
        if self.chat_input_open == open {
            if open {
                self.chat.update(cx, |chat, cx| {
                    chat.set_input_visible(true, window, cx);
                });
            }
            return;
        }
        self.chat_input_open = open;
        if open {
            self.reset_look_target();
        }
        self.sync_cursor_tracking_task(cx);
        self.chat.update(cx, |chat, cx| {
            chat.set_input_visible(open, window, cx);
        });
        cx.notify();
    }

    fn toggle_chat_input(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.set_chat_input_open(!self.chat_input_open, window, cx);
    }

    fn open_chat_input(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.desktop_pet_visible {
            if let Err(error) = set_desktop_pet_window_visible(window, true) {
                log::warn!("全局快捷键恢复桌宠窗口失败：{error}");
                return;
            }
            self.set_desktop_pet_visible(true, window, cx);
        }
        window.activate_window();
        self.set_chat_input_open(true, window, cx);
    }

    fn toggle_chat_input_from_shortcut(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.desktop_pet_visible && self.chat_input_open {
            self.set_chat_input_open(false, window, cx);
        } else {
            self.open_chat_input(window, cx);
        }
    }

    pub(crate) fn open_config_window(&mut self, cx: &mut Context<Self>) {
        if let Some(handle) = self.config_window
            && handle
                .update(cx, |_, window, _| window.activate_window())
                .is_ok()
        {
            return;
        }

        let (window_size, window_min_size) = settings_window_sizes(cx);
        let window_bounds = restored_window_bounds(ConfigWindow::Settings, window_size, cx);
        let config = self.config.clone();
        let result = cx.open_window(
            WindowOptions {
                window_bounds: Some(window_bounds),
                window_min_size: Some(window_min_size),
                titlebar: None,
                focus: true,
                show: true,
                kind: WindowKind::Normal,
                window_background: WindowBackgroundAppearance::Transparent,
                window_decorations: Some(WindowDecorations::Client),
                is_resizable: true,
                is_minimizable: true,
                is_movable: true,
                app_owns_titlebar_drag: true,
                app_id: Some(APPLICATION_ID.to_owned()),
                ..Default::default()
            },
            move |window, cx| {
                if let Err(error) = configure_settings_window(window) {
                    log::warn!("{}", t!("log.settings_window_config_failed", error = error));
                }
                window.set_window_title("LunaMate");
                let view = cx.new(|cx| SettingsWindowView::new(config, window, cx));
                cx.new(|cx| {
                    Root::new(view, window, cx)
                        .bordered(false)
                        .bg(transparent_black())
                })
            },
        );
        match result {
            Ok(handle) => {
                self.config_window = Some(handle.into());
                log::info!("设置窗口已打开");
            }
            Err(error) => log::error!("{}", t!("log.settings_window_create_failed", error = error)),
        }
    }

    fn toggle_config_window(&mut self, cx: &mut Context<Self>) {
        if let Some(handle) = self.config_window.take()
            && handle
                .update(cx, |_, window, _| window.remove_window())
                .is_ok()
        {
            log::info!("设置窗口已关闭");
            return;
        }
        self.open_config_window(cx);
    }

    pub(crate) fn toggle_tray_menu(&mut self, anchor: TrayMenuAnchor, cx: &mut Context<Self>) {
        let Some(tray) = self.system_tray.clone() else {
            return;
        };
        if tray.uses_native_menu() {
            self.close_tray_menu(cx);
            tray.show_native_menu();
            return;
        }
        if let Some(handle) = self.tray_menu_window.take()
            && handle
                .update(cx, |_, window, _| window.remove_window())
                .is_ok()
        {
            return;
        }
        let desktop_pet_hidden = !self.desktop_pet_visible;
        let (options, menu_bounds) = tray_menu_window_options(anchor, cx);
        let tray_for_window = tray.clone();
        let result = cx.open_window(options, move |window, cx| {
            if let Err(error) = configure_tray_menu_window(window) {
                log::warn!(
                    "{}",
                    t!("log.tray_menu_window_config_failed", error = error)
                );
            }
            let view =
                cx.new(|cx| TrayMenuView::new(tray_for_window, desktop_pet_hidden, window, cx));
            cx.new(|cx| {
                Root::new(view, window, cx)
                    .bordered(false)
                    .bg(transparent_black())
            })
        });
        match result {
            Ok(handle) => {
                let handle: AnyWindowHandle = handle.into();
                let native_window = handle.update(cx, |_, window, _| {
                    NativeTrayMenuWindow::prepare(window, menu_bounds, anchor.scale_factor)
                });
                let native_window = native_window
                    .map_err(|error| error.to_string())
                    .and_then(|result| result);
                let native_window = match native_window {
                    Ok(native_window) => native_window,
                    Err(error) => {
                        log::warn!(
                            "{}",
                            t!("log.tray_menu_window_config_failed", error = error)
                        );
                        let _ = handle.update(cx, |_, window, _| window.remove_window());
                        tray.show_native_menu();
                        return;
                    }
                };
                self.tray_menu_window = Some(handle);
                let tray_for_fallback = tray.clone();
                // 原生 SetWindowPos 会同步派发 WM_MOVE、WM_SIZE 与 WM_DPICHANGED，必须等
                // 当前 App borrow 结束后执行，避免重入 GPUI。显示前再校验当前窗口 generation。
                cx.spawn(async move |this, cx| {
                    let current = this
                        .update(cx, |this, _| this.tray_menu_window == Some(handle))
                        .unwrap_or(false);
                    if !current {
                        return;
                    }
                    if let Err(error) = native_window.show() {
                        log::warn!(
                            "{}",
                            t!("log.tray_menu_window_config_failed", error = error)
                        );
                        let _ = this.update(cx, |this, cx| {
                            if this.tray_menu_window == Some(handle) {
                                this.close_tray_menu(cx);
                                tray_for_fallback.show_native_menu();
                            }
                        });
                    }
                })
                .detach();
            }
            Err(error) => {
                log::warn!("{}", t!("log.tray_menu_create_failed", error = error));
                tray.show_native_menu();
            }
        }
    }

    fn close_tray_menu(&mut self, cx: &mut Context<Self>) {
        if let Some(handle) = self.tray_menu_window.take() {
            let _ = handle.update(cx, |_, window, _| window.remove_window());
        }
    }

    /// 记录本次实际提交绘制的图像，并回收已经离开两代场景的 atlas 资源。
    fn track_rendered_image(&mut self, next: Option<Arc<RenderImage>>, window: &mut Window) {
        let image_changed = match (&self.current_rendered_image, &next) {
            (Some(current), Some(next)) => current.id != next.id,
            (None, Some(_)) => true,
            _ => false,
        };
        let current = self.current_rendered_image.take();
        if let Some(previous) = self.previous_rendered_image.take()
            && current
                .as_ref()
                .is_none_or(|current| previous.id != current.id)
            && let Err(error) = window.drop_image(previous)
        {
            log::warn!("{}", t!("log.image_release_failed", error = error));
        }
        self.previous_rendered_image = current;
        self.current_rendered_image = next;
        if image_changed {
            self.record_presented_frame();
        }
    }

    fn send_hit_area_event_at(
        &mut self,
        frame: &RenderedModelFrame,
        generation: u64,
        position: gpui::Point<gpui::Pixels>,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> bool {
        if self.chat_input_open || self.model_generation != generation {
            return false;
        }

        let viewport = window.viewport_size();
        let Some(hit_area) = frame.hit_area_at_window_point(
            [f32::from(position.x), f32::from(position.y)],
            [f32::from(viewport.width), f32::from(viewport.height)],
        ) else {
            return false;
        };
        let part_name = hit_area.name().to_owned();
        let language = self.appearance.borrow().language;
        self.chat.update(cx, |chat, cx| {
            chat.send_model_click_event(&part_name, language, cx)
        })
    }
}

fn sync_system_tray_appearance(tray: Option<&SystemTray>, cx: &App) {
    let Some(tray) = tray else {
        return;
    };
    let palette = UiPalette::from_app(cx);
    let style = TrayIconStyle::new(rgb8_over(palette.primary, palette.background));
    if let Err(error) = tray.sync_appearance(style) {
        log::warn!("{}", t!("log.tray_appearance_failed", error = error));
    }
}

fn rgb8_over(color: Hsla, background: Hsla) -> [u8; 3] {
    let color = background.to_rgb().blend(color.to_rgb());
    [color.r, color.g, color.b].map(|channel| (channel.clamp(0.0, 1.0) * 255.0).round() as u8)
}

fn model_display_name(path: &Path) -> String {
    path.file_name()
        .and_then(OsStr::to_str)
        .and_then(|name| name.strip_suffix(".model3.json"))
        .filter(|name| !name.is_empty())
        .map(str::to_owned)
        .unwrap_or_else(|| t!("model_state.unnamed").to_string())
}
