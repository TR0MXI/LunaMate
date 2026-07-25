//! 管理桌宠根视图、模型 generation 生命周期及其渲染实现。

pub(in crate::ui) mod model_task;
mod render;

use std::{
    ffi::OsStr,
    path::{Path, PathBuf},
    sync::{Arc, mpsc::TrySendError},
    time::{Duration, Instant},
};

use gpui::{
    AnyWindowHandle, AppContext, Context, Entity, RenderImage, SharedString, Styled, Subscription,
    Task, Window, WindowBackgroundAppearance, WindowDecorations, WindowKind, WindowOptions, px,
    size, transparent_black,
};
use gpui_component::Root;
use parking_lot::{Condvar, Mutex};
use rust_i18n::t;

use crate::{
    agent::AgentView,
    config::{CONFIG, ConfigWindow, ModelWindowSize, ThemePreset},
    model::{
        FrameRateMeter, FrameWake, GpuUnderlay, GpuUnderlayEvent, GpuUnderlaySize, ModelCommand,
        ModelCommandSender, ModelLoadDiagnostics, ModelPreviewCapabilities, RenderCancellation,
        RenderedModelFrame,
    },
    platform::{WindowMover, WindowPositionController, configure_settings_window},
};

use super::{
    SettingsEvent, SettingsView, SettingsWindowView, apply, apply_language, cache_window_position,
    desktop_pet_window_size, gpu_underlay_size, gpu_underlay_size_for_window,
    raster_dimensions_for_window, restored_window_bounds, settings_window_sizes,
};

const FPS_REFRESH_INTERVAL: Duration = Duration::from_millis(250);

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

    fn wait(&self) {
        let mut completed = self.completed.lock();
        while !*completed {
            self.changed.wait(&mut completed);
        }
    }
}

/// 持有桌宠窗口的模型状态、交互实体和后台渲染任务。
pub(crate) struct DesktopPetView {
    frame: Option<Arc<RenderedModelFrame>>,
    current_rendered_image: Option<Arc<RenderImage>>,
    previous_rendered_image: Option<Arc<RenderImage>>,
    look_target: Arc<Mutex<[f32; 2]>>,
    eye_tracking_enabled: bool,
    show_fps: bool,
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
    close_after_gpu_shutdown: bool,
    gpu_shutdown_completion: Option<Arc<GpuShutdownCompletion>>,
    window_mover: WindowMover,
    config: Entity<SettingsView>,
    chat: Entity<AgentView>,
    chat_open: bool,
    position_controller: WindowPositionController,
    pending_model_window_size: Option<ModelWindowSize>,
    config_window: Option<AnyWindowHandle>,
    selected_model: Option<PathBuf>,
    model_state: ModelLoadState,
    raster_dimensions: [u32; 2],
    cpu_raster_dimensions: [u32; 2],
    gpu_underlay_size: GpuUnderlaySize,
    model_generation: u64,
    model_task: Option<Task<()>>,
    _config_subscription: Subscription,
    _bounds_subscription: Subscription,
    _appearance_subscription: Subscription,
}

impl DesktopPetView {
    /// 创建桌宠根视图并启动初始模型 generation。
    pub(crate) fn new(
        config: Entity<SettingsView>,
        chat: Entity<AgentView>,
        initial_model: Option<PathBuf>,
        raster_dimensions: [u32; 2],
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
            if let Some(mut underlay) = this.gpu_underlay.take() {
                underlay.shutdown();
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
        let gpu_underlay_size = gpu_underlay_size_for_window(window);
        let gpu_underlay = match GpuUnderlay::attach(window) {
            Ok(underlay) => underlay,
            Err(error) => {
                log::warn!("{}", t!("log.gpu_underlay_init_failed", error = error));
                None
            }
        };
        let gpu_events = gpu_underlay.as_ref().map(GpuUnderlay::events);
        let config_subscription =
            cx.subscribe(&config, |this, _, event: &SettingsEvent, cx| match event {
                SettingsEvent::ModelChanged(model_path) => {
                    this.load_model(model_path.clone(), cx);
                }
                SettingsEvent::FrameRateChanged => this.wake_frame_rate_scheduler(),
                SettingsEvent::EyeTrackingChanged(enabled) => {
                    this.eye_tracking_enabled = *enabled;
                    if !*enabled {
                        this.reset_look_target();
                    }
                    cx.notify();
                }
                SettingsEvent::ShowFpsChanged(show) => this.set_show_fps(*show, cx),
                SettingsEvent::ModelWindowSizeChanged(size) => {
                    this.pending_model_window_size = Some(*size);
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
                    if let Some(sender) = &this.model_commands
                        && sender.try_send(ModelCommand::ResetExpression).is_ok()
                    {
                        this.wake_model();
                    }
                }
                SettingsEvent::AgentChanged => {
                    this.chat.update(cx, |chat, cx| {
                        chat.refresh_settings(cx);
                    });
                }
                SettingsEvent::WindowPositionsReset => {
                    this.position_controller.request_reset();
                    cx.notify();
                }
                SettingsEvent::AppearanceChanged(settings) => {
                    apply_language(settings.language);
                    this.model_state.refresh_localized_warning();
                    apply(settings, None, cx);
                }
            });
        cache_window_position(window, ConfigWindow::DesktopPet);
        let bounds_subscription = cx.observe_window_bounds(window, |this, window, _| {
            if !this.position_controller.observe_bounds() {
                return;
            }
            cache_window_position(window, ConfigWindow::DesktopPet);
        });
        let appearance_subscription = window.observe_window_appearance(|window, cx| {
            let appearance = CONFIG.appearance();
            if appearance.theme == ThemePreset::System {
                apply(&appearance, Some(window), cx);
            }
        });
        let mut view = Self {
            frame: None,
            current_rendered_image: None,
            previous_rendered_image: None,
            look_target,
            eye_tracking_enabled: CONFIG.eye_tracking(),
            show_fps: CONFIG.show_fps(),
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
            close_after_gpu_shutdown: false,
            gpu_shutdown_completion: None,
            window_mover: WindowMover::new(),
            config,
            chat,
            chat_open: false,
            position_controller: WindowPositionController::default(),
            pending_model_window_size: None,
            config_window: None,
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
            _bounds_subscription: bounds_subscription,
            _appearance_subscription: appearance_subscription,
        };
        if view.show_fps {
            view.start_fps_task(cx);
        }
        if let Some(events) = gpu_events {
            view.gpu_event_task = Some(cx.spawn(async move |this, cx| {
                loop {
                    match events.recv().await {
                        Ok(event) => {
                            let keep_running = this
                                .update(cx, |this, cx| this.handle_gpu_event(event, cx))
                                .unwrap_or(false);
                            if !keep_running {
                                return;
                            }
                        }
                        Err(_) => {
                            let _ = this.update(cx, |this, cx| {
                                if this.gpu_underlay.is_some() {
                                    log::error!("{}", t!("log.gpu_worker_exited"));
                                    this.fallback_to_cpu(cx);
                                }
                            });
                            return;
                        }
                    }
                }
            }));
        }
        view.load_model(initial_model, cx);
        view
    }

    fn update_look_target(&self, position: gpui::Point<gpui::Pixels>, window: &Window) {
        if !self.eye_tracking_enabled || self.frame.is_none() || self.chat_open {
            return;
        }
        let viewport = window.viewport_size();
        let width = f32::from(viewport.width).max(1.0);
        let height = f32::from(viewport.height).max(1.0);
        let look = [
            (f32::from(position.x) / width * 2.0 - 1.0).clamp(-1.0, 1.0),
            (1.0 - f32::from(position.y) / height * 2.0).clamp(-1.0, 1.0),
        ];
        let mut target = self.look_target.lock();
        if *target == look {
            return;
        }
        *target = look;
        drop(target);
        self.wake_model();
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
                self.frame = Some(Arc::new(frame));
                self.model_state = ModelLoadState::ready(diagnostics);
                self.config.update(cx, |config, cx| {
                    config.set_preview_capabilities(capabilities, cx);
                });
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
                {
                    self.frame = Some(Arc::new(frame));
                    self.record_gpu_presented_frames(presented_at, presented_frames);
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
                self.model_state = ModelLoadState::Failed(
                    t!("model_state.load_failed", name = model_name, error = error).to_string(),
                );
                self.frame = None;
                self.config.update(cx, |config, cx| {
                    config.set_preview_capabilities(ModelPreviewCapabilities::default(), cx);
                });
                self.model_commands = None;
                cx.notify();
            }
            GpuUnderlayEvent::ModelGpuFailed { generation, error } => {
                if self.model_generation != generation || self.gpu_underlay.is_none() {
                    return true;
                }
                let model_name = self
                    .selected_model
                    .as_deref()
                    .map(model_display_name)
                    .unwrap_or_else(|| t!("model_state.unnamed").to_string());
                let message = t!(
                    "model_state.render_failed",
                    name = model_name,
                    error = error
                )
                .to_string();
                log::error!("{}", t!("log.gpu_model_cpu_fallback", message = message));
                self.fallback_to_cpu(cx);
                return false;
            }
            GpuUnderlayEvent::Unavailable { error } => {
                log::error!("{}", t!("log.gpu_underlay_cpu_fallback", error = error));
                self.fallback_to_cpu(cx);
                return false;
            }
        }
        true
    }

    fn fallback_to_cpu(&mut self, cx: &mut Context<Self>) {
        if let Some(cancellation) = self.model_cancellation.take() {
            cancellation.cancel();
        }
        self.raster_dimensions = self.cpu_raster_dimensions;
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
        self.gpu_shutdown_restart_cpu = false;
        self.close_after_gpu_shutdown = true;
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
        self.gpu_shutdown_restart_cpu = false;
        if self.gpu_shutdown_pending {
            if let Some(completion) = &self.gpu_shutdown_completion {
                completion.wait();
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
        let worker = underlay.request_shutdown();
        let completion = Arc::new(GpuShutdownCompletion::default());
        self.gpu_shutdown_pending = true;
        self.gpu_shutdown_completion = Some(completion.clone());
        let background = cx.background_executor().clone();

        // attachment 留在前台 future 中；后台只等待线程，确保原生 view 晚于 surface 析构。
        cx.spawn(async move |this, cx| {
            let worker_panicked = background
                .spawn(async move {
                    let worker_panicked = worker.is_some_and(|worker| worker.join().is_err());
                    completion.complete();
                    worker_panicked
                })
                .await;
            drop(underlay);
            if worker_panicked {
                log::error!("{}", t!("log.gpu_worker_panicked"));
            }
            let _ = this.update_in(cx, |this, window, cx| {
                this.gpu_shutdown_pending = false;
                this.gpu_shutdown_completion = None;
                if this.close_after_gpu_shutdown {
                    window.remove_window();
                } else if this.gpu_shutdown_restart_cpu && this.model_generation == generation {
                    this.selected_model = None;
                    this.load_model(model_path, cx);
                }
            });
        })
        .detach();
    }

    fn record_presented_frame(&mut self) {
        if !self.show_fps {
            return;
        }
        let now = Instant::now();
        self.frame_rate_meter.record(now);
    }

    fn record_gpu_presented_frames(&mut self, presented_at: Instant, presented_frames: u64) {
        if !self.show_fps {
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
        if show {
            self.start_fps_task(cx);
        }
        cx.notify();
    }

    fn start_fps_task(&mut self, cx: &mut Context<Self>) {
        let background = cx.background_executor().clone();
        self.fps_task = Some(cx.spawn(async move |this, cx| {
            loop {
                background.timer(FPS_REFRESH_INTERVAL).await;
                let keep_running = this
                    .update(cx, |this, cx| {
                        if !this.show_fps {
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

    fn apply_pending_model_window_size(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some(window_size) = self.pending_model_window_size.take() else {
            return false;
        };
        let display_size = cx
            .primary_display()
            .map(|display| display.visible_bounds().size)
            .unwrap_or_else(|| size(px(1280.0), px(720.0)));
        let [width, height] = desktop_pet_window_size(
            f32::from(display_size.width),
            f32::from(display_size.height),
            window_size,
        );
        let viewport = window.viewport_size();
        if (f32::from(viewport.width) - width).abs() < 0.5
            && (f32::from(viewport.height) - height).abs() < 0.5
        {
            self.update_render_dimensions(width, height, window.scale_factor());
            let model_path = self.selected_model.clone();
            self.selected_model = None;
            self.load_model(model_path, cx);
            return true;
        }

        window.resize(size(px(width), px(height)));
        // 后端可能异步应用、取整或钳制尺寸；等待实际 viewport 变化后再重建 generation。
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
        let changed = if self.gpu_underlay.is_some() {
            self.gpu_underlay_size != next_gpu_size
        } else {
            self.cpu_raster_dimensions != next_cpu_dimensions
        };
        if !changed {
            return;
        }
        self.gpu_underlay_size = next_gpu_size;
        self.cpu_raster_dimensions = next_cpu_dimensions;
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

    fn toggle_chat(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.chat_open = !self.chat_open;
        if self.chat_open {
            self.reset_look_target();
            self.chat.update(cx, |chat, cx| {
                chat.refresh_settings(cx);
                chat.focus_input(window, cx);
            });
        }
        cx.notify();
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
                app_id: Some("lunamate-settings".to_owned()),
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
            Ok(handle) => self.config_window = Some(handle.into()),
            Err(error) => log::error!("{}", t!("log.settings_window_create_failed", error = error)),
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

    fn activate_hit_area_at(
        &mut self,
        frame: &RenderedModelFrame,
        generation: u64,
        position: gpui::Point<gpui::Pixels>,
        window: &Window,
    ) -> bool {
        if self.chat_open || self.model_generation != generation {
            return false;
        }

        let viewport = window.viewport_size();
        let Some(hit_area) = frame.hit_area_at_window_point(
            [f32::from(position.x), f32::from(position.y)],
            [f32::from(viewport.width), f32::from(viewport.height)],
        ) else {
            return false;
        };
        let Some(sender) = &self.model_commands else {
            return false;
        };

        match sender.try_send(ModelCommand::ActivateHitArea(hit_area.activation())) {
            Ok(()) => {
                self.wake_model();
                true
            }
            Err(TrySendError::Full(_)) => false,
            Err(TrySendError::Disconnected(_)) => {
                self.model_commands = None;
                false
            }
        }
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
