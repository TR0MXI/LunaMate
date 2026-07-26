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
    config::{CONFIG, FrameRate},
    platform::{InitializationCancellation, SurfaceFactory, SurfaceOwner, SurfaceSeed},
};

use super::super::{
    frame_scheduler::FramePacer,
    interaction::MAX_COMMANDS_PER_FRAME,
    live2d::{AnimatedModel, GpuModelRenderer, SurfaceAlphaMode},
};

use super::{
    GpuUnderlayEvent, GpuUnderlaySize, LatestFrameSlot, LoadRequest, PresentedFrame, WorkerMailbox,
};

const SURFACE_RETRY_INITIAL_DELAY: Duration = Duration::from_millis(16);
const CLEAR_RETRY_INITIAL_DELAY: Duration = Duration::from_millis(50);
const SURFACE_RETRY_MAX_DELAY: Duration = Duration::from_secs(1);

struct GpuSurface {
    surface: wgpu::Surface<'static>,
    _owner: SurfaceOwner,
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
pub(in crate::model) enum ModelFailureStage {
    Load,
    Gpu,
}

pub(in crate::model) fn model_failure_event(
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
    Paused,
    Shutdown,
}

pub(in crate::model) enum PauseWaitResult {
    Running,
    Replaced(LoadRequest),
    Shutdown,
}

pub(in crate::model) enum RetryWaitResult {
    Ready,
    Replaced(LoadRequest),
    Paused,
    Shutdown,
}

/// 对遮挡或暂时不可用的 surface 使用有界指数退避，避免固定频率空轮询。
pub(in crate::model) struct SurfaceRetryBackoff {
    initial: Duration,
    next: Duration,
}

impl SurfaceRetryBackoff {
    pub(in crate::model) fn new(initial: Duration) -> Self {
        Self {
            initial,
            next: initial,
        }
    }

    pub(in crate::model) fn next_delay(&mut self) -> Duration {
        let delay = self.next;
        self.next = self.next.saturating_mul(2).min(SURFACE_RETRY_MAX_DELAY);
        delay
    }

    pub(in crate::model) fn reset(&mut self) {
        self.next = self.initial;
    }
}

/// 按帧率设置选择呈现模式；无限制模式优先避免 FIFO 的垂直同步节流。
pub(in crate::model) fn present_mode_for_frame_rate(
    frame_rate: FrameRate,
    supported_modes: &[wgpu::PresentMode],
) -> wgpu::PresentMode {
    if frame_rate.uses_vsync() {
        return wgpu::PresentMode::Fifo;
    }

    [wgpu::PresentMode::Immediate, wgpu::PresentMode::Mailbox]
        .into_iter()
        .find(|mode| supported_modes.contains(mode))
        .unwrap_or(wgpu::PresentMode::Fifo)
}

impl GpuSurface {
    fn new(
        factory: SurfaceFactory,
        size: GpuUnderlaySize,
        mailbox: Arc<WorkerMailbox>,
    ) -> Result<Option<Self>, String> {
        // SurfaceSeed 在全部可失败初始化完成前保持整体所有权，确保提前返回时先释放
        // WGPU surface，再释放 Wayland child/AppKit 关联资源，最后释放 Instance。
        let seed = factory.create(mailbox.as_ref())?;
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
            present_mode_for_frame_rate(CONFIG.frame_rate(), &capabilities.present_modes);
        seed.surface.configure(&device, &config);
        if let Some(error) = device_error.lock().take() {
            return Err(format!("配置 Live2D GPU surface 失败：{error}"));
        }
        if mailbox.is_shutdown() {
            return Ok(None);
        }
        let SurfaceSeed {
            surface,
            owner,
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
        // 与切换呈现模式一致：立即取出本次 configure 的校验错误，避免它被归因到后续帧。
        if let Some(error) = self.device_error.lock().take() {
            return Err(format!("配置 Live2D GPU surface 失败：{error}"));
        }
        Ok(())
    }

    /// 在运行时切换限帧设置时同步更新 swapchain 呈现模式。
    fn set_present_mode_for_frame_rate(&mut self, frame_rate: FrameRate) -> Result<(), String> {
        let present_mode = present_mode_for_frame_rate(frame_rate, &self.supported_present_modes);
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
    ) -> Result<Option<super::super::interaction::RenderedModelFrame>, GpuFrameError> {
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

/// 从配置同步限帧档位与 swapchain 呈现模式。
fn sync_frame_rate(surface: &mut GpuSurface, pacer: &mut FramePacer) -> Result<(), String> {
    let frame_rate = CONFIG.frame_rate();
    pacer.set_target_fps(
        frame_rate.limit(),
        frame_rate.allows_frame_rate_degradation(),
    );
    surface.set_present_mode_for_frame_rate(frame_rate)
}

pub(super) fn run(
    factory: SurfaceFactory,
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
        match wait_while_paused(&mailbox) {
            PauseWaitResult::Running => {}
            PauseWaitResult::Replaced(replacement) => {
                request = replacement;
                continue;
            }
            PauseWaitResult::Shutdown => return,
        }
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
            Ok(ClearSurfaceResult::Paused) => continue,
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
        match wait_while_paused(&mailbox) {
            PauseWaitResult::Running => {}
            PauseWaitResult::Replaced(replacement) => {
                request = replacement;
                continue;
            }
            PauseWaitResult::Shutdown => return,
        }
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
        let mut first_frame_retry = SurfaceRetryBackoff::new(SURFACE_RETRY_INITIAL_DELAY);
        let first_frame = loop {
            match wait_while_paused(&mailbox) {
                PauseWaitResult::Running => {}
                PauseWaitResult::Replaced(replacement) => {
                    request = replacement;
                    continue 'worker;
                }
                PauseWaitResult::Shutdown => return,
            }
            match surface.render_model(&mut model, &mut renderer, Duration::ZERO, [0.0, 0.0]) {
                Ok(Some(frame)) => {
                    first_frame_retry.reset();
                    if mailbox.is_paused() {
                        continue;
                    }
                    break frame;
                }
                Ok(None) => {
                    match wait_for_surface_retry(&mailbox, first_frame_retry.next_delay()) {
                        RetryWaitResult::Ready | RetryWaitResult::Paused => continue,
                        RetryWaitResult::Replaced(replacement) => {
                            request = replacement;
                            continue 'worker;
                        }
                        RetryWaitResult::Shutdown => return,
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
        let initial_frame_rate = CONFIG.frame_rate();
        let mut pacer = FramePacer::new(
            initial_frame_rate.limit(),
            initial_frame_rate.allows_frame_rate_degradation(),
        );
        let mut needs_next_frame = model.needs_continuous_frames();
        let mut render_requested = false;
        let mut reset_delta = false;
        let mut surface_retry = SurfaceRetryBackoff::new(SURFACE_RETRY_INITIAL_DELAY);
        loop {
            if mailbox.is_paused() {
                pacer.reset_after_idle();
                reset_delta = true;
                render_requested = true;
                match wait_while_paused(&mailbox) {
                    PauseWaitResult::Running => continue,
                    PauseWaitResult::Replaced(replacement) => {
                        request = replacement;
                        continue 'worker;
                    }
                    PauseWaitResult::Shutdown => break 'worker,
                }
            }
            if let Err(error) = sync_frame_rate(&mut surface, &mut pacer) {
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
                if update.woken {
                    mailbox.wake();
                }
                request = replacement;
                continue 'worker;
            }
            if update.pause_changed {
                pacer.reset_after_idle();
                reset_delta = true;
                render_requested = true;
                if update.paused {
                    continue;
                }
            }
            if let Err(error) = sync_frame_rate(&mut surface, &mut pacer) {
                let _ = events.send_blocking(GpuUnderlayEvent::Unavailable { error });
                break 'worker;
            }
            render_requested |= update.woken;
            if !needs_next_frame && !render_requested {
                continue;
            }
            if pacer.delay_until_next_frame(Instant::now()) > Duration::ZERO {
                continue;
            }
            if mailbox.is_paused() {
                continue;
            }
            if !needs_next_frame {
                pacer.reset_after_idle();
            }
            let frame_started = Instant::now();
            let delta = if reset_delta || !needs_next_frame {
                Duration::ZERO
            } else {
                frame_started.saturating_duration_since(previous_frame)
            };
            reset_delta = false;
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
                    surface_retry.reset();
                    if !mailbox.is_paused() {
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
                        Ok(ClearSurfaceResult::Paused) => {}
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
                pacer.postpone_next_frame(Instant::now(), surface_retry.next_delay());
            }
        }
    }
}

/// 重试透明清屏，直到成功 present、generation 被替换或 worker 被关闭。
fn clear_surface_until_ready(
    surface: &mut GpuSurface,
    mailbox: &WorkerMailbox,
) -> Result<ClearSurfaceResult, String> {
    let mut retry = SurfaceRetryBackoff::new(CLEAR_RETRY_INITIAL_DELAY);
    loop {
        match surface.clear()? {
            true => return Ok(ClearSurfaceResult::Cleared),
            false => match wait_for_surface_retry(mailbox, retry.next_delay()) {
                RetryWaitResult::Ready => {}
                RetryWaitResult::Replaced(replacement) => {
                    return Ok(ClearSurfaceResult::Replaced(replacement));
                }
                RetryWaitResult::Paused => return Ok(ClearSurfaceResult::Paused),
                RetryWaitResult::Shutdown => return Ok(ClearSurfaceResult::Shutdown),
            },
        }
    }
}

pub(in crate::model) fn wait_while_paused(mailbox: &WorkerMailbox) -> PauseWaitResult {
    let mut woken = false;
    while mailbox.is_paused() {
        let update = mailbox.wait(None);
        woken |= update.woken;
        if update.shutdown {
            return PauseWaitResult::Shutdown;
        }
        if let Some(replacement) = update.replacement {
            if woken {
                mailbox.wake();
            }
            return PauseWaitResult::Replaced(replacement);
        }
    }
    if woken {
        mailbox.wake();
    }
    PauseWaitResult::Running
}

pub(in crate::model) fn wait_for_surface_retry(
    mailbox: &WorkerMailbox,
    delay: Duration,
) -> RetryWaitResult {
    let deadline = Instant::now() + delay;
    let mut woken = false;
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        let update = mailbox.wait(Some(remaining));
        woken |= update.woken;
        if update.shutdown {
            return RetryWaitResult::Shutdown;
        }
        if let Some(replacement) = update.replacement {
            if woken {
                mailbox.wake();
            }
            return RetryWaitResult::Replaced(replacement);
        }
        if update.paused {
            if woken {
                mailbox.wake();
            }
            return RetryWaitResult::Paused;
        }
        if Instant::now() >= deadline {
            if woken {
                mailbox.wake();
            }
            return RetryWaitResult::Ready;
        }
    }
}

fn wait_for_replacement(mailbox: &WorkerMailbox) -> Option<LoadRequest> {
    let mut woken = false;
    loop {
        let update = mailbox.wait(None);
        woken |= update.woken;
        if update.shutdown {
            return None;
        }
        if let Some(replacement) = update.replacement {
            if woken {
                mailbox.wake();
            }
            return Some(replacement);
        }
    }
}
