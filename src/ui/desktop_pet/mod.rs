//! 管理桌宠根视图、模型 generation 生命周期及其渲染实现。

mod gpu_lifecycle;
pub(in crate::ui) mod model_task;
mod outfit;
mod render;
mod shortcut;
mod tracking;
mod voice;
mod window_chat;

use std::{
    cell::RefCell,
    ffi::OsStr,
    path::Path,
    rc::Rc,
    sync::Arc,
    time::{Duration, Instant},
};

use gpui::{
    AnyWindowHandle, Context, Entity, Pixels, Point, RenderImage, SharedString, Size, Subscription,
    Task, Window,
};
use parking_lot::{Condvar, Mutex};
use rust_i18n::t;

use crate::{
    config::{AppearanceSettings, CONFIG, ConfigWindow, ModelWindowSize, ThemePreset, VoiceMode},
    model::{
        FrameRateMeter, FrameWake, GpuUnderlay, GpuUnderlaySize, ModelCommand, ModelCommandSender,
        ModelLoadDiagnostics, ModelManifest, RenderCancellation, RenderedModelFrame,
    },
    platform::{GlobalCursorTracker, SystemTray, WindowMover, WindowPositionController},
    shortcut::ShortcutManager,
    voice::{SpeechPlayback, VoiceActivitySnapshot, VoiceController},
};

use super::{
    AgentView, AgentViewEvent, SettingsEvent, SettingsView, ThinkingFeedback, apply,
    apply_language, cache_window_position, gpu_underlay_size_for_window,
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
    chat_thinking_feedback: Option<ThinkingFeedback>,
    position_controller: WindowPositionController,
    pending_model_window_size: Option<ModelWindowSize>,
    pending_model_window_size_attempts: u32,
    config_window: Option<AnyWindowHandle>,
    tray_menu_window: Option<AnyWindowHandle>,
    selected_model: Option<ModelManifest>,
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
        initial_model: Option<ModelManifest>,
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
                log::info!("event=gpu_underlay_attached");
                (Some(underlay), false)
            }
            Ok(None) => {
                log::info!("event=gpu_underlay_unavailable reason=unsupported fallback=cpu");
                (None, false)
            }
            Err(_) => {
                log::warn!("event=gpu_underlay_init_failed fallback=cpu");
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
                log::warn!("event=shortcut_runtime_unavailable");
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
        let chat_thinking_feedback = chat.read(cx).thinking_feedback();
        let chat_subscription = cx.observe(&chat, |this, chat, cx| {
            let visible = chat.read(cx).reply_visible();
            let thinking_feedback = chat.read(cx).thinking_feedback();
            if this.chat_overlay_visible != visible
                || this.chat_thinking_feedback != thinking_feedback
            {
                this.chat_overlay_visible = visible;
                this.chat_thinking_feedback = thinking_feedback;
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
                window_chat::sync_system_tray_appearance(tray_for_appearance.as_deref(), cx);
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
            chat_thinking_feedback,
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
}

fn model_display_name(path: &Path) -> String {
    path.file_name()
        .and_then(OsStr::to_str)
        .and_then(|name| name.strip_suffix(".model3.json"))
        .filter(|name| !name.is_empty())
        .map(str::to_owned)
        .unwrap_or_else(|| t!("model_state.unnamed").to_string())
}
