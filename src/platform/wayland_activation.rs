//! 在有界专用线程中消费 portal 快捷键携带的 Wayland 激活令牌。

use std::{
    ffi::c_void,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc::{Receiver, RecvTimeoutError, SyncSender, TryRecvError, TrySendError, sync_channel},
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

use gpui::Window;
use raw_window_handle::{HasDisplayHandle, HasWindowHandle, RawDisplayHandle, RawWindowHandle};
use wayland_client::{
    Connection, Dispatch, EventQueue, Proxy, QueueHandle,
    protocol::{wl_callback, wl_registry, wl_surface},
};
use wayland_protocols::xdg::activation::v1::client::xdg_activation_v1;

const COMMAND_CAPACITY: usize = 8;
const REGISTRY_SYNC_TIMEOUT: Duration = Duration::from_secs(3);
const POLL_INTERVAL: Duration = Duration::from_millis(10);

/// 只交给受控线程使用的 GPUI foreign display 与 surface 借用地址。
#[derive(Clone, Copy, Debug)]
pub(crate) struct WaylandActivationTarget {
    display: usize,
    surface: usize,
}

/// 在 GPUI 窗口释放前关闭并 join 的 Wayland 激活控制端。
pub(crate) struct WaylandActivationController {
    commands: SyncSender<ActivationCommand>,
    shutdown: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

enum ActivationCommand {
    Activate(String),
}

struct WaylandActivator {
    connection: Connection,
    event_queue: EventQueue<ActivationState>,
    state: ActivationState,
    _registry: wl_registry::WlRegistry,
    surface: wl_surface::WlSurface,
}

/// 从当前窗口取得 Wayland 原生句柄；X11 窗口返回 `None`。
pub(crate) fn wayland_activation_target(
    window: &Window,
) -> Result<Option<WaylandActivationTarget>, String> {
    let window_handle = HasWindowHandle::window_handle(window)
        .map_err(|error| format!("无法取得快捷键 Wayland 窗口句柄：{error}"))?;
    let RawWindowHandle::Wayland(window_handle) = window_handle.as_raw() else {
        return Ok(None);
    };
    let display_handle = HasDisplayHandle::display_handle(window)
        .map_err(|error| format!("无法取得快捷键 Wayland display 句柄：{error}"))?;
    let RawDisplayHandle::Wayland(display_handle) = display_handle.as_raw() else {
        return Err("快捷键窗口与 display 的 Wayland 类型不一致".to_owned());
    };
    Ok(Some(WaylandActivationTarget {
        display: display_handle.display.as_ptr() as usize,
        surface: window_handle.surface.as_ptr() as usize,
    }))
}

impl WaylandActivationController {
    pub(crate) fn start(target: WaylandActivationTarget) -> Self {
        let (commands, receiver) = sync_channel(COMMAND_CAPACITY);
        let shutdown = Arc::new(AtomicBool::new(false));
        let thread_shutdown = shutdown.clone();
        let thread = thread::Builder::new()
            .name("lunamate-wayland-activation".to_owned())
            .spawn(move || run_activation_thread(target, receiver, thread_shutdown));
        let thread = match thread {
            Ok(thread) => Some(thread),
            Err(_) => {
                log::warn!("event=wayland_activation_thread_start_failed");
                None
            }
        };
        Self {
            commands,
            shutdown,
            thread,
        }
    }

    /// 异步提交单次激活；队列已满时拒绝，避免快捷键突发阻塞 UI 线程。
    pub(crate) fn activate(&self, token: String) -> Result<(), String> {
        if token.is_empty() {
            return Err("Wayland 激活令牌为空".to_owned());
        }
        match self.commands.try_send(ActivationCommand::Activate(token)) {
            Ok(()) => Ok(()),
            Err(TrySendError::Full(_)) => Err("Wayland 激活请求队列已满".to_owned()),
            Err(TrySendError::Disconnected(_)) => Err("Wayland 激活线程已结束".to_owned()),
        }
    }
}

impl Drop for WaylandActivationController {
    fn drop(&mut self) {
        self.shutdown.store(true, Ordering::Release);
        if let Some(thread) = self.thread.take()
            && thread.join().is_err()
        {
            log::warn!("event=wayland_activation_thread_exit_failed reason=panic");
        }
    }
}

fn run_activation_thread(
    target: WaylandActivationTarget,
    receiver: Receiver<ActivationCommand>,
    shutdown: Arc<AtomicBool>,
) {
    let mut pending = None;
    let mut activator = match WaylandActivator::new(target, &receiver, &shutdown, &mut pending) {
        Ok(activator) => activator,
        Err(_) => {
            if !shutdown.load(Ordering::Acquire) {
                log::warn!("event=wayland_activation_unavailable stage=initialize");
            }
            return;
        }
    };
    if shutdown.load(Ordering::Acquire) {
        return;
    }
    if let Some(token) = pending
        && activator.activate(&token).is_err()
    {
        log::warn!("event=wayland_activation_failed stage=pending_request");
    }
    while !shutdown.load(Ordering::Acquire) {
        match receiver.recv_timeout(POLL_INTERVAL) {
            Ok(ActivationCommand::Activate(token)) => {
                if activator.activate(&token).is_err() {
                    log::warn!("event=wayland_activation_failed stage=request");
                }
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => break,
        }
    }
}

impl WaylandActivator {
    fn new(
        target: WaylandActivationTarget,
        commands: &Receiver<ActivationCommand>,
        shutdown: &AtomicBool,
        pending: &mut Option<String>,
    ) -> Result<Self, String> {
        let display = target.display as *mut c_void;
        if display.is_null() || target.surface == 0 {
            return Err("快捷键 Wayland 原生句柄为空".to_owned());
        }
        // SAFETY: display 由仍存活的 GPUI connection 拥有；本控制器在窗口释放回调中
        // 同步 join，因此 guest backend 与所有 proxy 都先于 foreign display 失效。
        let backend =
            unsafe { wayland_client::backend::Backend::from_foreign_display(display.cast()) };
        let connection = Connection::from_backend(backend);
        let mut event_queue = connection.new_event_queue::<ActivationState>();
        let queue_handle = event_queue.handle();
        let registry = connection.display().get_registry(&queue_handle, ());
        let _sync = connection.display().sync(&queue_handle, ());
        let mut state = ActivationState::default();
        flush_connection(&connection)?;
        let deadline = Instant::now() + REGISTRY_SYNC_TIMEOUT;
        while !state.initial_sync_done {
            event_queue
                .dispatch_pending(&mut state)
                .map_err(|error| format!("读取 Wayland 激活协议失败：{error}"))?;
            collect_pending_commands(commands, shutdown, pending)?;
            if state.initial_sync_done {
                break;
            }
            if Instant::now() >= deadline {
                return Err("等待 Wayland 激活协议超过 3 秒".to_owned());
            }
            thread::sleep(POLL_INTERVAL);
            flush_connection(&connection)?;
        }
        if state.activation.is_none() {
            return Err("Wayland 合成器未提供 xdg_activation_v1".to_owned());
        }
        // SAFETY: surface 来自与 connection 匹配的 GPUI WaylandWindowHandle；这里只
        // 构造借用 proxy 发送 activate 请求，不销毁或提交该 surface。
        let surface_id = unsafe {
            wayland_client::backend::ObjectId::from_ptr(
                wl_surface::WlSurface::interface(),
                (target.surface as *mut c_void).cast(),
            )
        }
        .map_err(|error| format!("包装快捷键 Wayland surface 失败：{error}"))?;
        let surface = wl_surface::WlSurface::from_id(&connection, surface_id)
            .map_err(|error| format!("访问快捷键 Wayland surface 失败：{error}"))?;

        Ok(Self {
            connection,
            event_queue,
            state,
            _registry: registry,
            surface,
        })
    }

    fn activate(&mut self, token: &str) -> Result<(), String> {
        let activation = self
            .state
            .activation
            .as_ref()
            .ok_or_else(|| "Wayland 激活协议已不可用".to_owned())?;
        activation.activate(token.to_owned(), &self.surface);
        self.event_queue
            .dispatch_pending(&mut self.state)
            .map_err(|error| format!("处理 Wayland 激活事件失败：{error}"))?;
        flush_connection(&self.connection)
    }
}

impl Drop for WaylandActivator {
    fn drop(&mut self) {
        if let Some(activation) = self.state.activation.take() {
            activation.destroy();
        }
        let _ = self.connection.flush();
    }
}

fn collect_pending_commands(
    commands: &Receiver<ActivationCommand>,
    shutdown: &AtomicBool,
    pending: &mut Option<String>,
) -> Result<(), String> {
    if shutdown.load(Ordering::Acquire) {
        return Err("Wayland 激活初始化已取消".to_owned());
    }
    loop {
        match commands.try_recv() {
            Ok(ActivationCommand::Activate(token)) => *pending = Some(token),
            Err(TryRecvError::Empty) => return Ok(()),
            Err(TryRecvError::Disconnected) => {
                return Err("Wayland 激活控制端已释放".to_owned());
            }
        }
    }
}

fn flush_connection(connection: &Connection) -> Result<(), String> {
    match connection.flush() {
        Ok(()) => Ok(()),
        Err(wayland_client::backend::WaylandError::Io(error))
            if error.kind() == std::io::ErrorKind::WouldBlock =>
        {
            Ok(())
        }
        Err(error) => Err(format!("提交 Wayland 激活请求失败：{error}")),
    }
}

#[derive(Default)]
struct ActivationState {
    activation: Option<xdg_activation_v1::XdgActivationV1>,
    initial_sync_done: bool,
}

impl Dispatch<wl_callback::WlCallback, ()> for ActivationState {
    fn event(
        state: &mut Self,
        _: &wl_callback::WlCallback,
        event: wl_callback::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if matches!(event, wl_callback::Event::Done { .. }) {
            state.initial_sync_done = true;
        }
    }
}

impl Dispatch<wl_registry::WlRegistry, ()> for ActivationState {
    fn event(
        state: &mut Self,
        registry: &wl_registry::WlRegistry,
        event: wl_registry::Event,
        _: &(),
        _: &Connection,
        queue_handle: &QueueHandle<Self>,
    ) {
        if let wl_registry::Event::Global {
            name,
            interface,
            version,
        } = event
            && interface == "xdg_activation_v1"
        {
            state.activation = Some(registry.bind::<xdg_activation_v1::XdgActivationV1, _, _>(
                name,
                version.min(1),
                queue_handle,
                (),
            ));
        }
    }
}

impl Dispatch<xdg_activation_v1::XdgActivationV1, ()> for ActivationState {
    fn event(
        _: &mut Self,
        _: &xdg_activation_v1::XdgActivationV1,
        _: xdg_activation_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}
