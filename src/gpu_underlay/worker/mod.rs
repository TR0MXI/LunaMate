//! 独占 WGPU surface、Live2D GPU 资源并驱动模型帧循环。

use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

use async_channel::{Sender as AsyncSender, TrySendError};
use futures::executor::block_on;
use gpui_wgpu::wgpu;
use parking_lot::Mutex;
use rust_i18n::t;

use crate::{
    config::CONFIG,
    frame_scheduler::FramePacer,
    interaction::MAX_COMMANDS_PER_FRAME,
    live2d_image::{AnimatedModel, GpuModelRenderer, SurfaceAlphaMode},
};

use super::{
    GpuUnderlayEvent, GpuUnderlaySize, LatestFrameSlot, LoadRequest, PresentedFrame, SurfaceSeed,
    WorkerMailbox, platform,
};

const SURFACE_RETRY_DELAY: Duration = Duration::from_millis(16);

struct GpuSurface {
    surface: wgpu::Surface<'static>,
    _owner: platform::SurfaceOwner,
    _instance: wgpu::Instance,
    _adapter: wgpu::Adapter,
    device: wgpu::Device,
    queue: wgpu::Queue,
    config: wgpu::SurfaceConfiguration,
    supported_present_modes: Vec<wgpu::PresentMode>,
    alpha_mode: SurfaceAlphaMode,
    device_lost: Arc<AtomicBool>,
    device_error: Arc<Mutex<Option<String>>>,
    size: GpuUnderlaySize,
}

enum GpuFrameError {
    Cancelled,
    Model(String),
    Surface(String),
}

#[derive(Clone, Copy)]
enum ModelFailureStage {
    Load,
    Gpu,
}

fn model_failure_event(
    stage: ModelFailureStage,
    generation: u64,
    error: String,
) -> GpuUnderlayEvent {
    match stage {
        ModelFailureStage::Load => GpuUnderlayEvent::ModelLoadFailed { generation, error },
        ModelFailureStage::Gpu => GpuUnderlayEvent::ModelGpuFailed { generation, error },
    }
}

enum ClearSurfaceResult {
    Cleared,
    Replaced(LoadRequest),
    Shutdown,
}

/// 按帧率设置选择呈现模式；无限制模式优先避免 FIFO 的垂直同步节流。
fn present_mode_for_frame_rate(
    frame_rate_limit: Option<u16>,
    supported_modes: &[wgpu::PresentMode],
) -> wgpu::PresentMode {
    if frame_rate_limit.is_some() {
        return wgpu::PresentMode::Fifo;
    }

    [wgpu::PresentMode::Immediate, wgpu::PresentMode::Mailbox]
        .into_iter()
        .find(|mode| supported_modes.contains(mode))
        .unwrap_or(wgpu::PresentMode::Fifo)
}

impl GpuSurface {
    fn new(
        factory: platform::SurfaceFactory,
        size: GpuUnderlaySize,
        mailbox: Arc<WorkerMailbox>,
    ) -> Result<Option<Self>, String> {
        // SurfaceSeed 在全部可失败初始化完成前保持整体所有权，确保提前返回时先释放
        // WGPU surface，再释放 Wayland child/AppKit 关联资源，最后释放 Instance。
        let seed = factory.create(&mailbox)?;
        if mailbox.is_shutdown() {
            return Ok(None);
        }
        let adapter = block_on(seed.instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::HighPerformance,
            force_fallback_adapter: false,
            compatible_surface: Some(&seed.surface),
        }))
        .map_err(|error| format!("找不到兼容 Live2D surface 的 GPU adapter：{error}"))?;
        if mailbox.is_shutdown() {
            return Ok(None);
        }
        let (device, queue) = block_on(adapter.request_device(&wgpu::DeviceDescriptor {
            label: Some("lunamate.live2d.device"),
            ..Default::default()
        }))
        .map_err(|error| format!("无法创建 Live2D GPU device：{error}"))?;
        let device_lost = Arc::new(AtomicBool::new(false));
        let lost_flag = device_lost.clone();
        let lost_wake = mailbox.clone();
        device.set_device_lost_callback(move |reason, message| {
            lost_flag.store(true, Ordering::Release);
            lost_wake.wake();
            log::error!(
                "{}",
                t!(
                    "log.gpu_device_lost",
                    reason = format!("{reason:?}"),
                    message = message
                )
            );
        });
        let device_error = Arc::new(Mutex::new(None));
        let uncaptured_error = device_error.clone();
        let error_wake = mailbox.clone();
        device.on_uncaptured_error(Arc::new(move |error| {
            *uncaptured_error.lock() = Some(error.to_string());
            error_wake.wake();
        }));

        let capabilities = seed.surface.get_capabilities(&adapter);
        let format = mocari::render::wgpu::preferred_surface_format(&capabilities.formats)
            .ok_or_else(|| "Live2D surface 没有可用颜色格式".to_owned())?;
        let (composite_alpha, alpha_mode) = if capabilities
            .alpha_modes
            .contains(&wgpu::CompositeAlphaMode::PreMultiplied)
        {
            (
                wgpu::CompositeAlphaMode::PreMultiplied,
                SurfaceAlphaMode::Premultiplied,
            )
        } else if capabilities
            .alpha_modes
            .contains(&wgpu::CompositeAlphaMode::PostMultiplied)
        {
            (
                wgpu::CompositeAlphaMode::PostMultiplied,
                SurfaceAlphaMode::Postmultiplied,
            )
        } else {
            return Err("Live2D surface 不支持透明 Alpha 合成".to_owned());
        };
        let [width, height] = size.physical;
        let mut config = seed
            .surface
            .get_default_config(&adapter, width, height)
            .ok_or_else(|| "GPU adapter 无法配置 Live2D surface".to_owned())?;
        config.format = format;
        config.alpha_mode = composite_alpha;
        config.present_mode =
            present_mode_for_frame_rate(CONFIG.frame_rate().limit(), &capabilities.present_modes);
        seed.surface.configure(&device, &config);
        if let Some(error) = device_error.lock().take() {
            return Err(format!("配置 Live2D GPU surface 失败：{error}"));
        }
        if mailbox.is_shutdown() {
            return Ok(None);
        }
        let SurfaceSeed {
            surface,
            _owner: owner,
            instance,
        } = seed;

        Ok(Some(Self {
            surface,
            _owner: owner,
            _instance: instance,
            _adapter: adapter,
            device,
            queue,
            config,
            supported_present_modes: capabilities.present_modes,
            alpha_mode,
            device_lost,
            device_error,
            size,
        }))
    }

    fn resize(&mut self, size: GpuUnderlaySize) -> Result<(), String> {
        let [width, height] = size.physical;
        if width == 0 || height == 0 {
            return Err("Live2D surface 尺寸必须非零".to_owned());
        }
        if self.size == size {
            return Ok(());
        }
        self.size = size;
        self.config.width = width;
        self.config.height = height;
        self.surface.configure(&self.device, &self.config);
        Ok(())
    }

    /// 在运行时切换限帧设置时同步更新 swapchain 呈现模式。
    fn set_present_mode_for_frame_rate(
        &mut self,
        frame_rate_limit: Option<u16>,
    ) -> Result<(), String> {
        let present_mode =
            present_mode_for_frame_rate(frame_rate_limit, &self.supported_present_modes);
        if self.config.present_mode == present_mode {
            return Ok(());
        }

        self.config.present_mode = present_mode;
        self.surface.configure(&self.device, &self.config);
        if let Some(error) = self.device_error.lock().take() {
            return Err(format!("切换 Live2D GPU 呈现模式失败：{error}"));
        }
        Ok(())
    }

    fn clear(&mut self) -> Result<bool, String> {
        match self.render_surface(|device, queue, target| {
            let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("lunamate.live2d.clear-encoder"),
            });
            {
                let _pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                    label: Some("lunamate.live2d.clear-pass"),
                    color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                        view: target,
                        depth_slice: None,
                        resolve_target: None,
                        ops: wgpu::Operations {
                            load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                            store: wgpu::StoreOp::Store,
                        },
                    })],
                    depth_stencil_attachment: None,
                    timestamp_writes: None,
                    occlusion_query_set: None,
                    multiview_mask: None,
                });
            }
            queue.submit([encoder.finish()]);
            Ok(())
        }) {
            Ok(rendered) => Ok(rendered),
            Err(GpuFrameError::Surface(error)) => Err(error),
            Err(GpuFrameError::Cancelled | GpuFrameError::Model(_)) => {
                Err("清空 Live2D surface 时发生内部状态错误".to_owned())
            }
        }
    }

    fn render_model(
        &mut self,
        model: &mut AnimatedModel,
        renderer: &mut GpuModelRenderer,
        delta: Duration,
        look: [f32; 2],
    ) -> Result<Option<crate::interaction::RenderedModelFrame>, GpuFrameError> {
        let mut frame = None;
        let rendered = self.render_surface(|device, queue, target| {
            let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("lunamate.live2d.frame-encoder"),
            });
            let encoded = renderer
                .encode_frame(model, delta, look, (device, queue), &mut encoder, target)
                .map_err(|error| {
                    if error.is_cancelled() {
                        GpuFrameError::Cancelled
                    } else {
                        GpuFrameError::Model(error.to_string())
                    }
                })?;
            queue.submit([encoder.finish()]);
            renderer.recall_vertex_uploads();
            frame = Some(encoded);
            Ok(())
        })?;
        Ok(rendered.then_some(frame).flatten())
    }

    fn render_surface(
        &mut self,
        mut encode: impl FnMut(
            &wgpu::Device,
            &wgpu::Queue,
            &wgpu::TextureView,
        ) -> Result<(), GpuFrameError>,
    ) -> Result<bool, GpuFrameError> {
        if self.device_lost.load(Ordering::Acquire) {
            return Err(GpuFrameError::Surface(
                "Live2D GPU device 已丢失".to_owned(),
            ));
        }
        if let Some(error) = self.device_error.lock().take() {
            return Err(GpuFrameError::Surface(format!(
                "Live2D GPU 操作失败：{error}"
            )));
        }
        self._owner
            .prepare_present(self.size)
            .map_err(GpuFrameError::Surface)?;
        let (surface_texture, suboptimal) = match self.surface.get_current_texture() {
            wgpu::CurrentSurfaceTexture::Success(texture) => (texture, false),
            wgpu::CurrentSurfaceTexture::Suboptimal(texture) => (texture, true),
            wgpu::CurrentSurfaceTexture::Timeout | wgpu::CurrentSurfaceTexture::Occluded => {
                return Ok(false);
            }
            wgpu::CurrentSurfaceTexture::Outdated => {
                self.surface.configure(&self.device, &self.config);
                return Ok(false);
            }
            wgpu::CurrentSurfaceTexture::Lost => {
                return Err(GpuFrameError::Surface(
                    "Live2D GPU surface 已丢失".to_owned(),
                ));
            }
            wgpu::CurrentSurfaceTexture::Validation => {
                return Err(GpuFrameError::Surface(
                    "获取 Live2D GPU surface texture 时发生校验错误".to_owned(),
                ));
            }
        };
        let view = surface_texture
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());
        encode(&self.device, &self.queue, &view)?;
        surface_texture.present();
        if suboptimal {
            self.surface.configure(&self.device, &self.config);
        }
        if let Some(error) = self.device_error.lock().take() {
            return Err(GpuFrameError::Surface(format!(
                "Live2D GPU 提交失败：{error}"
            )));
        }
        Ok(true)
    }
}

impl Drop for GpuSurface {
    fn drop(&mut self) {
        let _ = self.device.poll(wgpu::PollType::Wait {
            submission_index: None,
            timeout: Some(Duration::from_secs(1)),
        });
    }
}

pub(super) fn run(
    factory: platform::SurfaceFactory,
    mailbox: Arc<WorkerMailbox>,
    events: AsyncSender<GpuUnderlayEvent>,
    latest_frame: Arc<Mutex<LatestFrameSlot>>,
) {
    let Some(mut request) = wait_for_replacement(&mailbox) else {
        return;
    };
    let mut surface = match GpuSurface::new(factory, request.size, mailbox.clone()) {
        Ok(Some(surface)) => surface,
        Ok(None) => return,
        Err(error) => {
            let _ = events.send_blocking(GpuUnderlayEvent::Unavailable { error });
            return;
        }
    };

    'worker: loop {
        if let Err(error) = surface.resize(request.size) {
            let _ = events.send_blocking(GpuUnderlayEvent::Unavailable { error });
            return;
        }
        match clear_surface_until_ready(&mut surface, &mailbox) {
            Ok(ClearSurfaceResult::Cleared) => {}
            Ok(ClearSurfaceResult::Replaced(replacement)) => {
                request = replacement;
                continue;
            }
            Ok(ClearSurfaceResult::Shutdown) => return,
            Err(error) => {
                let _ = events.send_blocking(GpuUnderlayEvent::Unavailable { error });
                return;
            }
        }
        let Some(path) = request.path.clone() else {
            let Some(replacement) = wait_for_replacement(&mailbox) else {
                return;
            };
            request = replacement;
            continue;
        };

        let mut model = match AnimatedModel::load_for_gpu(
            &path,
            request.size.physical[0],
            request.size.physical[1],
            request.cancellation.clone(),
        ) {
            Ok(model) => model,
            Err(error) if error.is_cancelled() => {
                let Some(replacement) = wait_for_replacement(&mailbox) else {
                    return;
                };
                request = replacement;
                continue;
            }
            Err(error) => {
                let _ = events.send_blocking(model_failure_event(
                    ModelFailureStage::Load,
                    request.generation,
                    error.to_string(),
                ));
                let Some(replacement) = wait_for_replacement(&mailbox) else {
                    return;
                };
                request = replacement;
                continue;
            }
        };
        let diagnostics = model.diagnostics().clone();
        let capabilities = model.preview_capabilities();
        let mut renderer = match GpuModelRenderer::new(
            &surface.device,
            &surface.queue,
            &model,
            surface.config.format,
            surface.alpha_mode,
        ) {
            Ok(renderer) => renderer,
            Err(error) => {
                let _ = events.send_blocking(model_failure_event(
                    ModelFailureStage::Gpu,
                    request.generation,
                    error.to_string(),
                ));
                let Some(replacement) = wait_for_replacement(&mailbox) else {
                    return;
                };
                request = replacement;
                continue;
            }
        };
        let first_frame = loop {
            match surface.render_model(&mut model, &mut renderer, Duration::ZERO, [0.0, 0.0]) {
                Ok(Some(frame)) => break frame,
                Ok(None) => {
                    let update = mailbox.wait(Some(Duration::from_millis(16)));
                    if update.shutdown {
                        return;
                    }
                    if let Some(replacement) = update.replacement {
                        request = replacement;
                        continue 'worker;
                    }
                }
                Err(GpuFrameError::Cancelled) => {
                    let Some(replacement) = wait_for_replacement(&mailbox) else {
                        return;
                    };
                    request = replacement;
                    continue 'worker;
                }
                Err(GpuFrameError::Model(error)) => {
                    let _ = events.send_blocking(model_failure_event(
                        ModelFailureStage::Gpu,
                        request.generation,
                        error,
                    ));
                    let Some(replacement) = wait_for_replacement(&mailbox) else {
                        return;
                    };
                    request = replacement;
                    continue 'worker;
                }
                Err(GpuFrameError::Surface(error)) => {
                    let _ = events.send_blocking(GpuUnderlayEvent::Unavailable { error });
                    return;
                }
            }
        };
        let first_presented_at = Instant::now();
        let mut presented_frames = 1_u64;
        if events
            .send_blocking(GpuUnderlayEvent::ModelLoaded {
                generation: request.generation,
                frame: first_frame,
                presented_at: first_presented_at,
                presented_frames,
                diagnostics,
                capabilities,
            })
            .is_err()
        {
            return;
        }

        let mut previous_frame = Instant::now();
        let mut pacer = FramePacer::new(CONFIG.frame_rate().limit());
        let mut needs_next_frame = model.needs_continuous_frames();
        let mut render_requested = false;
        loop {
            let frame_rate_limit = CONFIG.frame_rate().limit();
            pacer.set_target_fps(frame_rate_limit);
            if let Err(error) = surface.set_present_mode_for_frame_rate(frame_rate_limit) {
                let _ = events.send_blocking(GpuUnderlayEvent::Unavailable { error });
                break 'worker;
            }
            let should_render = needs_next_frame || render_requested;
            let timeout = should_render.then(|| pacer.delay_until_next_frame(Instant::now()));
            let update = mailbox.wait(timeout);
            if update.shutdown {
                break 'worker;
            }
            if let Some(replacement) = update.replacement {
                request = replacement;
                continue 'worker;
            }
            render_requested |= update.woken;
            if !needs_next_frame && !render_requested {
                continue;
            }
            if pacer.delay_until_next_frame(Instant::now()) > Duration::ZERO {
                continue;
            }
            if !needs_next_frame {
                pacer.reset_after_idle();
            }
            let frame_started = Instant::now();
            let delta = if !needs_next_frame {
                Duration::ZERO
            } else {
                frame_started.saturating_duration_since(previous_frame)
            };
            previous_frame = frame_started;
            render_requested = false;
            let mut command_count = 0;
            for command in request.commands.try_iter().take(MAX_COMMANDS_PER_FRAME) {
                command_count += 1;
                model.handle_command(command);
            }
            let command_batch_full = command_count == MAX_COMMANDS_PER_FRAME;
            let look = *request.look_target.lock();
            match surface.render_model(&mut model, &mut renderer, delta, look) {
                Ok(Some(frame)) => {
                    presented_frames = presented_frames.saturating_add(1);
                    let should_notify = latest_frame.lock().publish(PresentedFrame {
                        generation: request.generation,
                        frame,
                        presented_at: Instant::now(),
                        presented_frames,
                    });
                    if should_notify {
                        match events.try_send(GpuUnderlayEvent::FrameAvailable {
                            generation: request.generation,
                        }) {
                            Ok(()) => {}
                            Err(TrySendError::Full(_)) => {
                                latest_frame.lock().notification_failed();
                            }
                            Err(TrySendError::Closed(_)) => break 'worker,
                        }
                    }
                }
                Ok(None) => render_requested = true,
                Err(GpuFrameError::Cancelled) => {
                    let Some(replacement) = wait_for_replacement(&mailbox) else {
                        break 'worker;
                    };
                    request = replacement;
                    continue 'worker;
                }
                Err(GpuFrameError::Model(error)) => {
                    match clear_surface_until_ready(&mut surface, &mailbox) {
                        Ok(ClearSurfaceResult::Cleared) => {}
                        Ok(ClearSurfaceResult::Replaced(replacement)) => {
                            request = replacement;
                            continue 'worker;
                        }
                        Ok(ClearSurfaceResult::Shutdown) => break 'worker,
                        Err(surface_error) => {
                            let _ = events.send_blocking(GpuUnderlayEvent::Unavailable {
                                error: surface_error,
                            });
                            break 'worker;
                        }
                    }
                    let _ = events.send_blocking(model_failure_event(
                        ModelFailureStage::Gpu,
                        request.generation,
                        error,
                    ));
                    let Some(replacement) = wait_for_replacement(&mailbox) else {
                        break 'worker;
                    };
                    request = replacement;
                    continue 'worker;
                }
                Err(GpuFrameError::Surface(error)) => {
                    let _ = events.send_blocking(GpuUnderlayEvent::Unavailable { error });
                    break 'worker;
                }
            }
            let frame_completed = Instant::now();
            needs_next_frame = model.needs_continuous_frames() || command_batch_full;
            pacer.complete_frame(frame_started, frame_completed);
            if render_requested {
                pacer.postpone_next_frame(Instant::now(), SURFACE_RETRY_DELAY);
            }
        }
    }
}

/// 重试透明清屏，直到成功 present、generation 被替换或 worker 被关闭。
fn clear_surface_until_ready(
    surface: &mut GpuSurface,
    mailbox: &WorkerMailbox,
) -> Result<ClearSurfaceResult, String> {
    loop {
        match surface.clear()? {
            true => return Ok(ClearSurfaceResult::Cleared),
            false => {
                let update = mailbox.wait(Some(Duration::from_millis(50)));
                if update.shutdown {
                    return Ok(ClearSurfaceResult::Shutdown);
                }
                if let Some(replacement) = update.replacement {
                    return Ok(ClearSurfaceResult::Replaced(replacement));
                }
            }
        }
    }
}

fn wait_for_replacement(mailbox: &WorkerMailbox) -> Option<LoadRequest> {
    loop {
        let update = mailbox.wait(None);
        if update.shutdown {
            return None;
        }
        if let Some(replacement) = update.replacement {
            return Some(replacement);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn model_failure_stages_map_to_distinct_events() {
        let load = model_failure_event(ModelFailureStage::Load, 7, "load".to_owned());
        assert!(matches!(
            load,
            GpuUnderlayEvent::ModelLoadFailed {
                generation: 7,
                error
            } if error == "load"
        ));

        let gpu = model_failure_event(ModelFailureStage::Gpu, 8, "gpu".to_owned());
        assert!(matches!(
            gpu,
            GpuUnderlayEvent::ModelGpuFailed {
                generation: 8,
                error
            } if error == "gpu"
        ));
    }

    #[test]
    fn finite_frame_rates_keep_fifo_presentation() {
        let modes = [
            wgpu::PresentMode::Fifo,
            wgpu::PresentMode::Immediate,
            wgpu::PresentMode::Mailbox,
        ];

        assert_eq!(
            present_mode_for_frame_rate(Some(120), &modes),
            wgpu::PresentMode::Fifo
        );
    }

    #[test]
    fn unlimited_presentation_prefers_immediate_when_available() {
        let modes = [
            wgpu::PresentMode::Fifo,
            wgpu::PresentMode::Mailbox,
            wgpu::PresentMode::Immediate,
        ];

        assert_eq!(
            present_mode_for_frame_rate(None, &modes),
            wgpu::PresentMode::Immediate
        );
    }

    #[test]
    fn unlimited_presentation_uses_mailbox_before_fifo_fallback() {
        assert_eq!(
            present_mode_for_frame_rate(
                None,
                &[wgpu::PresentMode::Fifo, wgpu::PresentMode::Mailbox]
            ),
            wgpu::PresentMode::Mailbox
        );
        assert_eq!(
            present_mode_for_frame_rate(None, &[wgpu::PresentMode::Fifo]),
            wgpu::PresentMode::Fifo
        );
    }
}
