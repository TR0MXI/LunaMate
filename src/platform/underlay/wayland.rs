//! 使用父 surface 下方的 `wl_subsurface` 承载 Live2D Vulkan swapchain。

use std::{
    ffi::c_void,
    ptr::NonNull,
    time::{Duration, Instant},
};

use gpui::Window;
use gpui_wgpu::wgpu;
use raw_window_handle::{HasDisplayHandle, HasWindowHandle, RawDisplayHandle, RawWindowHandle};
use wayland_client::{
    Connection, Dispatch, EventQueue, Proxy, QueueHandle, delegate_noop,
    protocol::{
        wl_callback, wl_compositor, wl_region, wl_registry, wl_subcompositor, wl_subsurface,
        wl_surface,
    },
};
use wayland_protocols::wp::viewporter::client::{wp_viewport, wp_viewporter};

use super::{InitializationCancellation, SurfaceSeed, UnderlaySize};

const REGISTRY_SYNC_TIMEOUT: Duration = Duration::from_secs(5);
const REGISTRY_POLL_INTERVAL: Duration = Duration::from_millis(10);

/// Wayland 原生资源全部由 worker owner 持有，UI 侧只保留占位 attachment。
pub(crate) struct NativeAttachment;

/// 保存 GPUI foreign display 与 parent surface 的借用句柄。
pub(crate) struct SurfaceFactory {
    display: usize,
    parent: usize,
}

/// 取得当前 Wayland 窗口句柄；X11 会返回 `None`。
pub(super) fn attach(
    window: &Window,
) -> Result<Option<(SurfaceFactory, NativeAttachment)>, String> {
    let window_handle = HasWindowHandle::window_handle(window)
        .map_err(|error| format!("无法取得 GPUI Linux 窗口句柄：{error}"))?;
    let RawWindowHandle::Wayland(window_handle) = window_handle.as_raw() else {
        return Ok(None);
    };
    let display_handle = HasDisplayHandle::display_handle(window)
        .map_err(|error| format!("无法取得 GPUI Wayland display 句柄：{error}"))?;
    let RawDisplayHandle::Wayland(display_handle) = display_handle.as_raw() else {
        return Ok(None);
    };

    Ok(Some((
        SurfaceFactory {
            display: display_handle.display.as_ptr() as usize,
            parent: window_handle.surface.as_ptr() as usize,
        },
        NativeAttachment,
    )))
}

impl SurfaceFactory {
    /// 在 worker 中建立 guest queue、child subsurface 与 Vulkan surface。
    pub(crate) fn create(
        self,
        cancellation: &dyn InitializationCancellation,
    ) -> Result<SurfaceSeed, String> {
        let (owner, child_ptr) = SurfaceOwner::new(self.display, self.parent, cancellation)?;
        let mut descriptor = wgpu::InstanceDescriptor::new_without_display_handle();
        descriptor.backends = wgpu::Backends::VULKAN;
        let instance = wgpu::Instance::new(descriptor);
        let display = NonNull::new(self.display as *mut c_void)
            .ok_or_else(|| "GPUI Wayland display 指针为空".to_owned())?;
        let child = NonNull::new(child_ptr)
            .ok_or_else(|| "Live2D Wayland child surface 指针为空".to_owned())?;
        let raw_display_handle =
            RawDisplayHandle::Wayland(raw_window_handle::WaylandDisplayHandle::new(display));
        let raw_window_handle =
            RawWindowHandle::Wayland(raw_window_handle::WaylandWindowHandle::new(child));
        // SAFETY: child wl_surface 与 display 由 SurfaceOwner 在同一 worker 中持有；字段
        // 析构顺序保证 WGPU surface 先释放，之后才销毁 subsurface 和 child surface。
        let surface = unsafe {
            instance.create_surface_unsafe(wgpu::SurfaceTargetUnsafe::RawHandle {
                raw_display_handle: Some(raw_display_handle),
                raw_window_handle,
            })
        }
        .map_err(|error| format!("无法创建 Wayland Live2D Vulkan surface：{error}"))?;
        Ok(SurfaceSeed::new(instance, surface, owner))
    }
}

/// 持有 guest connection 及 child surface 的完整协议生命周期。
pub(crate) struct SurfaceOwner {
    connection: Connection,
    event_queue: EventQueue<RegistryState>,
    state: RegistryState,
    _registry: wl_registry::WlRegistry,
    child: wl_surface::WlSurface,
    subsurface: wl_subsurface::WlSubsurface,
    viewport: Option<wp_viewport::WpViewport>,
    last_size: Option<UnderlaySize>,
}

impl SurfaceOwner {
    fn new(
        display: usize,
        parent: usize,
        cancellation: &dyn InitializationCancellation,
    ) -> Result<(Self, *mut c_void), String> {
        let display = display as *mut c_void;
        if display.is_null() || parent == 0 {
            return Err("GPUI Wayland 原生句柄为空".to_owned());
        }
        // SAFETY: display 来自当前存活的 GPUI Wayland connection；guest backend 不取得
        // connection 所有权，且 GpuUnderlay 在 GPUI 窗口释放前停止 worker。
        let backend =
            unsafe { wayland_client::backend::Backend::from_foreign_display(display.cast()) };
        let connection = Connection::from_backend(backend);
        let mut event_queue = connection.new_event_queue::<RegistryState>();
        let queue_handle = event_queue.handle();
        let registry = connection.display().get_registry(&queue_handle, ());
        let _initial_sync = connection.display().sync(&queue_handle, ());
        let mut state = RegistryState::default();
        flush_connection(&connection, "提交 Wayland registry 同步请求失败")?;
        let deadline = Instant::now() + REGISTRY_SYNC_TIMEOUT;
        while !state.initial_sync_done {
            event_queue
                .dispatch_pending(&mut state)
                .map_err(|error| format!("读取 Wayland 合成能力失败：{error}"))?;
            if state.initial_sync_done {
                break;
            }
            if cancellation.wait_for_shutdown(REGISTRY_POLL_INTERVAL) {
                return Err("Wayland underlay 初始化已取消".to_owned());
            }
            if Instant::now() >= deadline {
                return Err("等待 Wayland registry 同步超时".to_owned());
            }
            flush_connection(&connection, "重试 Wayland registry 同步请求失败")?;
        }
        let compositor = state
            .compositor
            .as_ref()
            .ok_or_else(|| "Wayland compositor 未提供 wl_compositor".to_owned())?;
        let subcompositor = state
            .subcompositor
            .as_ref()
            .ok_or_else(|| "Wayland compositor 未提供 wl_subcompositor".to_owned())?;
        // SAFETY: parent 指向 GPUI 已管理的 wl_surface，接口类型由 raw-window-handle
        // WaylandWindowHandle 契约保证；本模块只借用它，不接管或销毁该 proxy。
        let parent_id = unsafe {
            wayland_client::backend::ObjectId::from_ptr(
                wl_surface::WlSurface::interface(),
                (parent as *mut c_void).cast(),
            )
        }
        .map_err(|error| format!("无法包装 GPUI Wayland parent surface：{error}"))?;
        let parent = wl_surface::WlSurface::from_id(&connection, parent_id)
            .map_err(|error| format!("无法访问 GPUI Wayland parent surface：{error}"))?;
        let child = compositor.create_surface(&queue_handle, ());
        let empty_region = compositor.create_region(&queue_handle, ());
        child.set_input_region(Some(&empty_region));
        empty_region.destroy();
        let subsurface = subcompositor.get_subsurface(&child, &parent, &queue_handle, ());
        subsurface.place_below(&parent);
        subsurface.set_desync();
        let viewport = state
            .viewporter
            .as_ref()
            .map(|viewporter| viewporter.get_viewport(&child, &queue_handle, ()));
        flush_connection(&connection, "提交 Wayland underlay 初始化请求失败")?;
        let child_ptr = child.id().as_ptr().cast::<c_void>();

        Ok((
            Self {
                connection,
                event_queue,
                state,
                _registry: registry,
                child,
                subsurface,
                viewport,
                last_size: None,
            },
            child_ptr,
        ))
    }

    /// 在 WGPU commit 前同步逻辑尺寸、buffer scale 和待处理协议事件。
    pub(crate) fn prepare_present(&mut self, size: UnderlaySize) -> Result<(), String> {
        if self.last_size != Some(size) {
            if let Some(viewport) = &self.viewport {
                let width = i32::try_from(size.logical[0])
                    .map_err(|_| "Wayland underlay 逻辑宽度超过协议上限".to_owned())?;
                let height = i32::try_from(size.logical[1])
                    .map_err(|_| "Wayland underlay 逻辑高度超过协议上限".to_owned())?;
                self.child.set_buffer_scale(1);
                viewport.set_destination(width, height);
            } else {
                let scale_x = exact_buffer_scale(size.physical[0], size.logical[0]);
                let scale_y = exact_buffer_scale(size.physical[1], size.logical[1]);
                let Some(scale) = scale_x.filter(|scale| Some(*scale) == scale_y) else {
                    return Err(
                        "Wayland compositor 缺少 wp_viewporter，无法匹配当前缩放比例".to_owned(),
                    );
                };
                self.child.set_buffer_scale(scale);
            }
            self.last_size = Some(size);
        }
        self.event_queue
            .dispatch_pending(&mut self.state)
            .map_err(|error| format!("处理 Wayland underlay 事件失败：{error}"))?;
        flush_connection(&self.connection, "提交 Wayland underlay 帧状态失败")?;
        Ok(())
    }
}

impl Drop for SurfaceOwner {
    fn drop(&mut self) {
        if let Some(viewport) = self.viewport.take() {
            viewport.destroy();
        }
        self.subsurface.destroy();
        self.child.destroy();
        if let Some(viewporter) = self.state.viewporter.take() {
            viewporter.destroy();
        }
        if let Some(subcompositor) = self.state.subcompositor.take() {
            subcompositor.destroy();
        }
        let _ = self.connection.flush();
    }
}

/// 尝试冲刷 guest connection；非阻塞 socket 暂时写满时保留请求供下一帧重试。
fn flush_connection(connection: &Connection, context: &str) -> Result<(), String> {
    match connection.flush() {
        Ok(()) => Ok(()),
        Err(wayland_client::backend::WaylandError::Io(error))
            if error.kind() == std::io::ErrorKind::WouldBlock =>
        {
            Ok(())
        }
        Err(error) => Err(format!("{context}：{error}")),
    }
}

pub(crate) fn exact_buffer_scale(physical: u32, logical: u32) -> Option<i32> {
    if logical == 0 || !physical.is_multiple_of(logical) {
        return None;
    }
    i32::try_from(physical / logical)
        .ok()
        .filter(|scale| *scale > 0)
}

#[derive(Default)]
struct RegistryState {
    compositor: Option<wl_compositor::WlCompositor>,
    subcompositor: Option<wl_subcompositor::WlSubcompositor>,
    viewporter: Option<wp_viewporter::WpViewporter>,
    initial_sync_done: bool,
}

impl Dispatch<wl_callback::WlCallback, ()> for RegistryState {
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

impl Dispatch<wl_registry::WlRegistry, ()> for RegistryState {
    fn event(
        state: &mut Self,
        registry: &wl_registry::WlRegistry,
        event: wl_registry::Event,
        _: &(),
        _: &Connection,
        queue_handle: &QueueHandle<Self>,
    ) {
        let wl_registry::Event::Global {
            name,
            interface,
            version,
        } = event
        else {
            return;
        };
        match interface.as_str() {
            "wl_compositor" if state.compositor.is_none() => {
                state.compositor = Some(registry.bind(name, version.min(6), queue_handle, ()));
            }
            "wl_subcompositor" if state.subcompositor.is_none() => {
                state.subcompositor = Some(registry.bind(name, 1, queue_handle, ()));
            }
            "wp_viewporter" if state.viewporter.is_none() => {
                state.viewporter = Some(registry.bind(name, 1, queue_handle, ()));
            }
            _ => {}
        }
    }
}

delegate_noop!(RegistryState: ignore wl_compositor::WlCompositor);
delegate_noop!(RegistryState: ignore wl_region::WlRegion);
delegate_noop!(RegistryState: ignore wl_subcompositor::WlSubcompositor);
delegate_noop!(RegistryState: ignore wl_subsurface::WlSubsurface);
delegate_noop!(RegistryState: ignore wl_surface::WlSurface);
delegate_noop!(RegistryState: ignore wp_viewporter::WpViewporter);
delegate_noop!(RegistryState: ignore wp_viewport::WpViewport);
