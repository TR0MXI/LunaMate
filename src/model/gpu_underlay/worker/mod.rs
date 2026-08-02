//! 独占 WGPU surface、Live2D GPU 资源并驱动模型帧循环。

use std::{
    sync::{Arc, atomic::AtomicBool},
    time::Duration,
};

use gpui_wgpu::wgpu;
use parking_lot::Mutex;

use crate::{config::FrameRate, platform::SurfaceOwner};

use super::super::live2d::SurfaceAlphaMode;
use super::{GpuUnderlayEvent, GpuUnderlaySize, LoadRequest};

mod run;
mod surface;
mod wait;

pub(super) use run::run;
pub(in crate::model) use wait::{wait_for_surface_retry, wait_while_paused};

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
