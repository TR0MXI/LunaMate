//! 在 GPUI 原生绘制层下方管理独立 WGPU surface 与 Live2D worker。

use std::{
    panic::{AssertUnwindSafe, catch_unwind},
    path::PathBuf,
    sync::{Arc, mpsc::Receiver},
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

use async_channel::Receiver as AsyncReceiver;
use gpui_wgpu::wgpu;
use parking_lot::{Condvar, Mutex};

use crate::{
    capabilities::ModelLoadDiagnostics,
    interaction::{ModelCommand, RenderedModelFrame},
    live2d_image::{ModelPreviewCapabilities, RenderCancellation},
};

const EVENT_CHANNEL_CAPACITY: usize = 8;

#[cfg(target_os = "windows")]
#[path = "windows/mod.rs"]
mod platform;
#[cfg(target_os = "macos")]
#[path = "macos/mod.rs"]
mod platform;
#[cfg(target_os = "linux")]
#[path = "wayland/mod.rs"]
mod platform;
#[cfg(not(any(target_os = "windows", target_os = "macos", target_os = "linux")))]
#[path = "unsupported/mod.rs"]
mod platform;

mod worker;

/// 同时保存交换链物理像素和合成器逻辑尺寸。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct GpuUnderlaySize {
    pub(crate) physical: [u32; 2],
    pub(crate) logical: [u32; 2],
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
    Unavailable { error: String },
}

/// UI 线程持有的 GPU underlay 控制器。
pub(crate) struct GpuUnderlay {
    mailbox: Arc<WorkerMailbox>,
    events: AsyncReceiver<GpuUnderlayEvent>,
    worker: Option<JoinHandle<()>>,
    attachment: Option<platform::NativeAttachment>,
    active_cancellation: Option<RenderCancellation>,
    latest_frame: Arc<Mutex<LatestFrameSlot>>,
}

struct PresentedFrame {
    generation: u64,
    frame: RenderedModelFrame,
    presented_at: Instant,
    presented_frames: u64,
}

/// 保存最新成功呈现帧，并约束同一消费周期最多发布一个通知。
#[derive(Default)]
struct LatestFrameSlot {
    frame: Option<PresentedFrame>,
    notification_pending: bool,
    generation: Option<u64>,
}

impl LatestFrameSlot {
    fn begin_generation(&mut self, generation: u64) {
        self.frame = None;
        self.notification_pending = false;
        self.generation = Some(generation);
    }

    fn publish(&mut self, frame: PresentedFrame) -> bool {
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

    fn take(&mut self) -> Option<PresentedFrame> {
        self.notification_pending = false;
        self.frame.take()
    }

    /// 通知未进入事件队列时允许下一帧再次尝试发送。
    fn notification_failed(&mut self) {
        self.notification_pending = false;
    }
}

impl GpuUnderlay {
    /// 尝试为当前平台窗口建立 GPU underlay；X11 和不支持的平台返回 `None`。
    ///
    /// # Errors
    ///
    /// 原生句柄存在但无法安全建立平台 attachment 时返回错误。
    pub(crate) fn attach(window: &gpui::Window) -> Result<Option<Self>, String> {
        let Some((factory, attachment)) = platform::attach(window)? else {
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
                let result = catch_unwind(AssertUnwindSafe(|| {
                    worker::run(factory, worker_mailbox, event_sender, worker_latest_frame)
                }));
                if result.is_err() {
                    let _ = panic_sender.try_send(GpuUnderlayEvent::Unavailable {
                        error: "Live2D GPU worker 发生内部 panic".to_owned(),
                    });
                }
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
        path: Option<PathBuf>,
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
            log::error!("Live2D GPU worker 在退出时发生 panic");
        }
        self.attachment.take();
    }
}

impl Drop for GpuUnderlay {
    fn drop(&mut self) {
        self.shutdown();
    }
}

struct LoadRequest {
    generation: u64,
    path: Option<PathBuf>,
    size: GpuUnderlaySize,
    cancellation: RenderCancellation,
    commands: Receiver<ModelCommand>,
    look_target: Arc<Mutex<[f32; 2]>>,
}

#[derive(Default)]
struct WorkerMailbox {
    state: Mutex<MailboxState>,
    changed: Condvar,
}

#[derive(Default)]
struct MailboxState {
    replacement: Option<LoadRequest>,
    wake_pending: bool,
    shutdown: bool,
}

struct MailboxUpdate {
    replacement: Option<LoadRequest>,
    woken: bool,
    shutdown: bool,
}

impl WorkerMailbox {
    fn replace_model(&self, replacement: LoadRequest) {
        let mut state = self.state.lock();
        if state.shutdown {
            return;
        }
        state.replacement = Some(replacement);
        self.changed.notify_one();
    }

    fn wake(&self) {
        let mut state = self.state.lock();
        if state.shutdown || state.wake_pending {
            return;
        }
        state.wake_pending = true;
        self.changed.notify_one();
    }

    fn shutdown(&self) {
        let mut state = self.state.lock();
        state.shutdown = true;
        state.replacement = None;
        state.wake_pending = true;
        self.changed.notify_one();
    }

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

    fn wait(&self, timeout: Option<Duration>) -> MailboxUpdate {
        let mut state = self.state.lock();
        if !state.shutdown && state.replacement.is_none() && !state.wake_pending {
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
            shutdown: state.shutdown,
        }
    }
}

/// 在设备初始化完成前整体保持 surface、原生 owner 与 instance 的析构顺序。
pub(super) struct SurfaceSeed {
    surface: wgpu::Surface<'static>,
    _owner: platform::SurfaceOwner,
    instance: wgpu::Instance,
}

impl SurfaceSeed {
    /// 按 surface、owner、instance 的字段顺序保存平台 surface 种子。
    fn new(
        instance: wgpu::Instance,
        surface: wgpu::Surface<'static>,
        owner: platform::SurfaceOwner,
    ) -> Self {
        Self {
            surface,
            _owner: owner,
            instance,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::interaction::command_channel;

    fn load_request(generation: u64) -> LoadRequest {
        let (_, commands) = command_channel();
        LoadRequest {
            generation,
            path: None,
            size: GpuUnderlaySize {
                physical: [200, 400],
                logical: [100, 200],
            },
            cancellation: RenderCancellation::default(),
            commands,
            look_target: Arc::new(Mutex::new([0.0, 0.0])),
        }
    }

    fn presented_frame(generation: u64, presented_frames: u64) -> PresentedFrame {
        PresentedFrame {
            generation,
            frame: RenderedModelFrame::gpu(Vec::new(), [200, 400]),
            presented_at: Instant::now(),
            presented_frames,
        }
    }

    #[test]
    fn worker_wake_is_coalesced() {
        let mailbox = WorkerMailbox::default();
        mailbox.wake();
        mailbox.wake();

        let update = mailbox.wait(Some(Duration::ZERO));
        assert!(update.woken);
        assert!(!mailbox.wait(Some(Duration::ZERO)).woken);
    }

    #[test]
    fn replacement_does_not_fabricate_a_worker_wake() {
        let mailbox = WorkerMailbox::default();
        mailbox.replace_model(load_request(7));

        let update = mailbox.wait(Some(Duration::ZERO));
        assert!(update.replacement.is_some());
        assert!(!update.woken);
    }

    #[test]
    fn real_wake_remains_pending_beside_a_replacement() {
        let mailbox = WorkerMailbox::default();
        mailbox.replace_model(load_request(7));
        mailbox.wake();

        let update = mailbox.wait(Some(Duration::ZERO));
        assert!(update.replacement.is_some());
        assert!(update.woken);
    }

    #[test]
    fn pending_model_replacement_keeps_only_the_latest_generation() {
        let mailbox = WorkerMailbox::default();
        mailbox.replace_model(load_request(7));
        mailbox.replace_model(load_request(8));

        let update = mailbox.wait(Some(Duration::ZERO));
        assert_eq!(
            update.replacement.expect("最新模型请求必须保留").generation,
            8
        );
    }

    #[test]
    fn latest_frame_notification_is_coalesced_until_consumed() {
        let mut slot = LatestFrameSlot::default();
        slot.begin_generation(7);
        assert!(slot.publish(presented_frame(7, 1)));
        assert!(!slot.publish(presented_frame(7, 2)));

        let latest = slot.take().expect("latest slot 必须保留最近一帧");
        assert_eq!(latest.generation, 7);
        assert_eq!(latest.presented_frames, 2);
        assert!(slot.publish(presented_frame(7, 3)));
    }

    #[test]
    fn failed_latest_frame_notification_can_be_retried() {
        let mut slot = LatestFrameSlot::default();
        slot.begin_generation(7);
        assert!(slot.publish(presented_frame(7, 1)));

        slot.notification_failed();

        assert!(slot.publish(presented_frame(7, 2)));
        let latest = slot.take().expect("重试前 latest slot 必须保留最近一帧");
        assert_eq!(latest.presented_frames, 2);
    }

    #[test]
    fn stale_generation_cannot_publish_after_replacement() {
        let mut slot = LatestFrameSlot::default();
        slot.begin_generation(8);

        assert!(!slot.publish(presented_frame(7, 1)));
        assert!(slot.take().is_none());
    }

    #[test]
    fn shutdown_interrupts_an_idle_worker() {
        let mailbox = WorkerMailbox::default();
        mailbox.shutdown();

        let update = mailbox.wait(None);
        assert!(update.shutdown);
        assert!(update.replacement.is_none());
    }
}
