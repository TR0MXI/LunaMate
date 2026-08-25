//! 管理 Live2D GPU worker、mailbox、generation 事件与永久回退状态。

use std::{
    panic::{AssertUnwindSafe, catch_unwind},
    sync::{Arc, mpsc::Receiver},
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

use async_channel::Receiver as AsyncReceiver;
use parking_lot::{Condvar, Mutex};

use crate::platform::{InitializationCancellation, NativeAttachment, attach_underlay};

use super::{
    capabilities::ModelLoadDiagnostics,
    catalog::ModelManifest,
    interaction::{ModelCommand, RenderedModelFrame},
    live2d::{ModelPreviewCapabilities, RenderCancellation},
};

const EVENT_CHANNEL_CAPACITY: usize = 8;

pub(in crate::model) mod worker;

pub(crate) use crate::platform::UnderlaySize as GpuUnderlaySize;

/// 标识 GPU underlay 永久失效的阶段，供前台在不记录驱动自由文本时诊断回退原因。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum GpuUnavailableKind {
    WorkerPanic,
    Initialization,
    Resize,
    SurfaceClear,
    FrameRateSync,
    Surface,
}

impl GpuUnavailableKind {
    pub(crate) const fn id(self) -> &'static str {
        match self {
            Self::WorkerPanic => "worker_panic",
            Self::Initialization => "initialization",
            Self::Resize => "resize",
            Self::SurfaceClear => "surface_clear",
            Self::FrameRateSync => "frame_rate_sync",
            Self::Surface => "surface",
        }
    }
}

/// GPU worker 向 GPUI 发布的 generation 状态。
pub(crate) enum GpuUnderlayEvent {
    /// 首帧已经成功提交并呈现。
    ModelLoaded {
        generation: u64,
        frame: RenderedModelFrame,
        presented_at: Instant,
        presented_frames: u64,
        diagnostics: ModelLoadDiagnostics,
        capabilities: ModelPreviewCapabilities,
    },
    /// latest slot 中已有一帧成功呈现的动画状态。
    FrameAvailable { generation: u64 },
    /// 模型资源加载失败；worker 可继续服务后续 generation。
    ModelLoadFailed { generation: u64, error: String },
    /// 模型无法创建或继续使用 GPU renderer，调用方应永久回退 CPU。
    ModelGpuFailed { generation: u64, error: String },
    /// 原生 surface、adapter 或 device 不再可用，调用方应永久回退 CPU。
    Unavailable { kind: GpuUnavailableKind },
}

/// UI 线程持有的 GPU underlay 控制器。
pub(crate) struct GpuUnderlay {
    mailbox: Arc<WorkerMailbox>,
    events: AsyncReceiver<GpuUnderlayEvent>,
    worker: Option<JoinHandle<()>>,
    attachment: Option<NativeAttachment>,
    active_cancellation: Option<RenderCancellation>,
    latest_frame: Arc<Mutex<LatestFrameSlot>>,
}

pub(in crate::model) struct PresentedFrame {
    pub(in crate::model) generation: u64,
    pub(in crate::model) frame: RenderedModelFrame,
    pub(in crate::model) presented_at: Instant,
    pub(in crate::model) presented_frames: u64,
}

/// 保存最新成功呈现帧，并约束同一消费周期最多发布一个通知。
#[derive(Default)]
pub(in crate::model) struct LatestFrameSlot {
    frame: Option<PresentedFrame>,
    notification_pending: bool,
    generation: Option<u64>,
}

impl LatestFrameSlot {
    pub(in crate::model) fn begin_generation(&mut self, generation: u64) {
        self.frame = None;
        self.notification_pending = false;
        self.generation = Some(generation);
    }

    pub(in crate::model) fn publish(&mut self, frame: PresentedFrame) -> bool {
        if self.generation != Some(frame.generation) {
            return false;
        }
        self.frame = Some(frame);
        if self.notification_pending {
            false
        } else {
            self.notification_pending = true;
            true
        }
    }

    pub(in crate::model) fn take(&mut self) -> Option<PresentedFrame> {
        self.notification_pending = false;
        self.frame.take()
    }

    /// 通知未进入事件队列时允许下一帧再次尝试发送。
    pub(in crate::model) fn notification_failed(&mut self) {
        self.notification_pending = false;
    }
}

impl GpuUnderlay {
    /// 尝试为当前平台窗口建立 GPU underlay；不支持的平台返回 `None`。
    ///
    /// # Errors
    ///
    /// 原生句柄存在但无法安全建立平台 attachment 时返回错误。
    pub(crate) fn attach(window: &gpui::Window) -> Result<Option<Self>, String> {
        let Some((factory, attachment)) = attach_underlay(window)? else {
            return Ok(None);
        };
        let mailbox = Arc::new(WorkerMailbox::default());
        let latest_frame = Arc::new(Mutex::new(LatestFrameSlot::default()));
        let (event_sender, events) = async_channel::bounded(EVENT_CHANNEL_CAPACITY);
        let worker_mailbox = mailbox.clone();
        let worker_latest_frame = latest_frame.clone();
        let panic_sender = event_sender.clone();
        let worker = thread::Builder::new()
            .name("lunamate-live2d-gpu".to_owned())
            .spawn(move || {
                log::info!("event=gpu_worker_started");
                let result = catch_unwind(AssertUnwindSafe(|| {
                    worker::run(factory, worker_mailbox, event_sender, worker_latest_frame)
                }));
                if result.is_err() {
                    log::error!("event=gpu_worker_failed reason=panic");
                    let _ = panic_sender.try_send(GpuUnderlayEvent::Unavailable {
                        kind: GpuUnavailableKind::WorkerPanic,
                    });
                }
                log::info!("event=gpu_worker_stopped");
            })
            .map_err(|error| format!("无法启动 Live2D GPU worker：{error}"))?;

        Ok(Some(Self {
            mailbox,
            events,
            worker: Some(worker),
            attachment: Some(attachment),
            active_cancellation: None,
            latest_frame,
        }))
    }

    /// 返回异步事件接收端；同一控制器只应由一个 GPUI task 消费。
    pub(crate) fn events(&self) -> AsyncReceiver<GpuUnderlayEvent> {
        self.events.clone()
    }

    /// 取出 latest slot 中最近成功 present 的帧，并允许 worker 发布下一次通知。
    pub(crate) fn take_presented_frame(&self) -> Option<(u64, RenderedModelFrame, Instant, u64)> {
        self.latest_frame.lock().take().map(|frame| {
            (
                frame.generation,
                frame.frame,
                frame.presented_at,
                frame.presented_frames,
            )
        })
    }

    /// 用新的 generation 替换 worker 中尚未完成或正在显示的模型。
    pub(crate) fn load(
        &mut self,
        generation: u64,
        path: Option<ModelManifest>,
        size: GpuUnderlaySize,
        cancellation: RenderCancellation,
        commands: Receiver<ModelCommand>,
        look_target: Arc<Mutex<[f32; 2]>>,
    ) {
        if let Some(previous) = self.active_cancellation.replace(cancellation.clone()) {
            previous.cancel();
        }
        self.latest_frame.lock().begin_generation(generation);
        self.mailbox.replace_model(LoadRequest {
            generation,
            path,
            size,
            cancellation,
            commands,
            look_target,
        });
    }

    /// 合并一次输入或配置唤醒，避免鼠标移动扩张队列。
    pub(crate) fn wake(&self) {
        self.mailbox.wake();
    }

    /// 暂停或恢复 surface 获取与模型帧推进；模型和 GPU 资源继续驻留。
    pub(crate) fn set_paused(&self, paused: bool) {
        self.mailbox.set_paused(paused);
    }

    /// 请求 worker 停止并转移线程句柄；调用方必须等待句柄后再释放 attachment。
    pub(crate) fn request_shutdown(&mut self) -> Option<JoinHandle<()>> {
        if let Some(cancellation) = self.active_cancellation.take() {
            cancellation.cancel();
        }
        self.events.close();
        self.mailbox.shutdown();
        self.worker.take()
    }

    /// 同步停止 worker；仅用于无法再调度异步收尾的析构兜底。
    pub(crate) fn shutdown(&mut self) {
        if let Some(worker) = self.request_shutdown()
            && worker.join().is_err()
        {
            log::error!("event=gpu_worker_exit_failed reason=panic");
        }
        self.attachment.take();
    }
}

impl Drop for GpuUnderlay {
    fn drop(&mut self) {
        self.shutdown();
    }
}

pub(in crate::model) struct LoadRequest {
    pub(in crate::model) generation: u64,
    pub(in crate::model) path: Option<ModelManifest>,
    pub(in crate::model) size: GpuUnderlaySize,
    pub(in crate::model) cancellation: RenderCancellation,
    pub(in crate::model) commands: Receiver<ModelCommand>,
    pub(in crate::model) look_target: Arc<Mutex<[f32; 2]>>,
}

#[derive(Default)]
pub(in crate::model) struct WorkerMailbox {
    state: Mutex<MailboxState>,
    changed: Condvar,
}

#[derive(Default)]
struct MailboxState {
    replacement: Option<LoadRequest>,
    wake_pending: bool,
    paused: bool,
    pause_changed: bool,
    shutdown: bool,
}

pub(in crate::model) struct MailboxUpdate {
    pub(in crate::model) replacement: Option<LoadRequest>,
    pub(in crate::model) woken: bool,
    pub(in crate::model) paused: bool,
    pub(in crate::model) pause_changed: bool,
    pub(in crate::model) shutdown: bool,
}

impl WorkerMailbox {
    pub(in crate::model) fn replace_model(&self, replacement: LoadRequest) {
        let mut state = self.state.lock();
        if state.shutdown {
            return;
        }
        state.replacement = Some(replacement);
        self.changed.notify_one();
    }

    pub(in crate::model) fn wake(&self) {
        let mut state = self.state.lock();
        if state.shutdown || state.wake_pending {
            return;
        }
        state.wake_pending = true;
        self.changed.notify_one();
    }

    pub(in crate::model) fn set_paused(&self, paused: bool) {
        let mut state = self.state.lock();
        if state.shutdown || state.paused == paused {
            return;
        }
        state.paused = paused;
        state.pause_changed = true;
        self.changed.notify_one();
    }

    pub(in crate::model) fn is_paused(&self) -> bool {
        self.state.lock().paused
    }

    #[cfg(test)]
    pub(in crate::model) fn has_pending_wake(&self) -> bool {
        self.state.lock().wake_pending
    }

    pub(in crate::model) fn shutdown(&self) {
        let mut state = self.state.lock();
        state.shutdown = true;
        state.replacement = None;
        state.wake_pending = true;
        self.changed.notify_one();
    }

    pub(in crate::model) fn wait(&self, timeout: Option<Duration>) -> MailboxUpdate {
        let mut state = self.state.lock();
        if !state.shutdown
            && state.replacement.is_none()
            && !state.wake_pending
            && !state.pause_changed
        {
            match timeout {
                Some(timeout) => {
                    self.changed.wait_for(&mut state, timeout);
                }
                None => self.changed.wait(&mut state),
            }
        }
        MailboxUpdate {
            replacement: state.replacement.take(),
            woken: std::mem::take(&mut state.wake_pending),
            paused: state.paused,
            pause_changed: std::mem::take(&mut state.pause_changed),
            shutdown: state.shutdown,
        }
    }
}

impl InitializationCancellation for WorkerMailbox {
    fn is_shutdown(&self) -> bool {
        self.state.lock().shutdown
    }

    #[cfg(target_os = "linux")]
    fn wait_for_shutdown(&self, timeout: Duration) -> bool {
        let mut state = self.state.lock();
        if !state.shutdown {
            self.changed.wait_for(&mut state, timeout);
        }
        state.shutdown
    }
}
