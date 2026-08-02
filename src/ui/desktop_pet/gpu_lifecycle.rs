//! 管理 GPU underlay、CPU 回退、窗口显隐及渲染尺寸生命周期。

use std::{sync::Arc, time::Instant};

use gpui::{Context, RenderImage, Window, px, size};
use rust_i18n::t;

use super::{
    DesktopPetView, FPS_REFRESH_INTERVAL, GPU_SHUTDOWN_WAIT_TIMEOUT, GpuShutdownCompletion,
    MAX_WINDOW_RESIZE_ATTEMPTS, ModelLoadState, model_display_name,
};
use crate::{
    config::CONFIG,
    model::{GpuUnderlay, GpuUnderlayEvent, ModelManifest, ModelPreviewCapabilities},
    platform::set_desktop_pet_window_visible,
    ui::{desktop_pet_window_size, gpu_underlay_size, raster_dimensions_for_window},
    voice::VoiceActivitySnapshot,
};

impl DesktopPetView {
    pub(super) fn start_gpu_event_task(&mut self, cx: &mut Context<Self>) {
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
                                log::warn!(
                                    "event=gpu_underlay_unavailable reason=event_channel_closed fallback=cpu"
                                );
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

    pub(super) fn wake_model(&self) {
        if let Some(underlay) = &self.gpu_underlay {
            underlay.wake();
        }
        if let Some(wake) = &self.model_wake {
            wake.wake();
        }
    }

    pub(super) fn wake_frame_rate_scheduler(&self) {
        if let Some(underlay) = &self.gpu_underlay {
            underlay.wake();
        }
        if let Some(wake) = &self.frame_rate_wake {
            wake.wake();
        }
    }

    pub(super) fn handle_gpu_event(
        &mut self,
        event: GpuUnderlayEvent,
        cx: &mut Context<Self>,
    ) -> bool {
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
                    "event=model_ready generation={generation} renderer=gpu movable_expressions={movable_expression_count} motions={motion_count} expressions={expression_count} diagnostics={diagnostic_count}"
                );
                if diagnostic_count > 0 {
                    log::warn!(
                        "event=model_capability_warning generation={generation} diagnostics={diagnostic_count}"
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
                    "event=model_load_failed generation={generation} renderer=gpu stage=model_load"
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
                    "event=model_render_failed generation={generation} renderer=gpu stage=model_render fallback=cpu"
                );
                self.fallback_to_cpu(cx);
                return false;
            }
            GpuUnderlayEvent::Unavailable { kind } => {
                log::warn!(
                    "event=gpu_underlay_unavailable generation={} stage=underlay_runtime reason={} fallback=cpu",
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
            "event=desktop_pet_window_close_requested generation={} renderer={}",
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
                log::error!("event=gpu_worker_shutdown_timeout");
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
        model_path: Option<ModelManifest>,
        cx: &mut Context<Self>,
    ) {
        let Some(mut underlay) = self.gpu_underlay.take() else {
            if self.gpu_shutdown_restart_cpu && self.model_generation == generation {
                self.selected_model = None;
                self.load_model(model_path, cx);
            }
            return;
        };
        log::debug!("event=gpu_worker_shutdown_started generation={generation}");
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
                log::error!("event=gpu_worker_exit_failed reason=panic");
            }
            let _ = this.update_in(cx, |this, window, cx| {
                this.gpu_shutdown_pending = false;
                this.gpu_shutdown_completion = None;
                log::debug!("event=gpu_worker_released generation={generation}");
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

    pub(super) fn set_show_fps(&mut self, show: bool, cx: &mut Context<Self>) {
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

    pub(super) fn start_fps_task(&mut self, cx: &mut Context<Self>) {
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
                log::info!(
                    "event=gpu_underlay_restore_unavailable reason=unsupported fallback=cpu"
                );
                self.restore_cpu_after_hidden(cx);
                return;
            }
            Err(_) => {
                log::warn!("event=gpu_underlay_restore_failed fallback=cpu");
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
            "event=desktop_pet_visibility_changed visible={visible} generation={}",
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
            && window.drop_image(previous).is_err()
        {
            log::warn!("event=render_image_release_failed slot=previous");
        }
        if let Some(current) = current
            && window.drop_image(current).is_err()
        {
            log::warn!("event=render_image_release_failed slot=current");
        }
    }

    pub(super) fn apply_pending_model_window_size(
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
                    "event=desktop_pet_resize_not_applied attempts={MAX_WINDOW_RESIZE_ATTEMPTS}"
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

    pub(super) fn synchronize_render_dimensions(
        &mut self,
        window: &Window,
        cx: &mut Context<Self>,
    ) {
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

    /// 记录本次实际提交绘制的图像，并回收已经离开两代场景的 atlas 资源。
    pub(super) fn track_rendered_image(
        &mut self,
        next: Option<Arc<RenderImage>>,
        window: &mut Window,
    ) {
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
            && window.drop_image(previous).is_err()
        {
            log::warn!("event=render_image_release_failed slot=previous");
        }
        self.previous_rendered_image = current;
        self.current_rendered_image = next;
        if image_changed {
            self.record_presented_frame();
        }
    }
}
