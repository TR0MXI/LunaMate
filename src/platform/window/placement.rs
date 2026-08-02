//! 协调窗口拖动、默认位置复位与原生位置更新。

use std::time::{Duration, Instant};

use gpui::{App, Pixels, Point, Window, WindowBounds};

const POSITION_RESET_SUPPRESSION: Duration = Duration::from_millis(250);

/// 在不支持 GPUI 标题栏拖拽区域的平台上协调一次原生窗口移动。
pub(crate) struct WindowMover {
    native_drag_pending: bool,
}

/// 统一管理窗口位置复位期间的 bounds 事件，避免依赖单次事件顺序。
#[derive(Default)]
pub(crate) struct WindowPositionController {
    state: PositionResetState,
}

#[derive(Default)]
enum PositionResetState {
    #[default]
    Idle,
    Requested,
    AwaitingBounds {
        expires_at: Instant,
    },
}

impl WindowPositionController {
    /// 请求在下一次视图更新中把窗口移动到默认位置。
    pub(crate) fn request_reset(&mut self) {
        self.state = PositionResetState::Requested;
    }

    /// 执行一次待处理复位；返回 `Some(false)` 表示当前平台拒绝移动。
    pub(crate) fn apply_pending_reset(&mut self, window: &Window, cx: &App) -> Option<bool> {
        self.expire_suppression();
        if !matches!(self.state, PositionResetState::Requested) {
            return None;
        }
        let moved = super::move_window_to_default(window, cx);
        self.state = if moved {
            PositionResetState::AwaitingBounds {
                expires_at: Instant::now() + POSITION_RESET_SUPPRESSION,
            }
        } else {
            PositionResetState::Idle
        };
        Some(moved)
    }

    /// 返回当前 bounds 是否应写入位置缓存。
    pub(crate) fn observe_bounds(&mut self) -> bool {
        if matches!(self.state, PositionResetState::Requested) {
            return false;
        }
        if matches!(self.state, PositionResetState::AwaitingBounds { .. }) {
            if self
                .expiry()
                .is_some_and(|expires_at| Instant::now() < expires_at)
            {
                return false;
            }
            self.state = PositionResetState::Idle;
        }
        true
    }

    fn expiry(&self) -> Option<Instant> {
        match &self.state {
            PositionResetState::AwaitingBounds { expires_at } => Some(*expires_at),
            PositionResetState::Idle | PositionResetState::Requested => None,
        }
    }

    fn expire_suppression(&mut self) {
        if self
            .expiry()
            .is_some_and(|expires_at| Instant::now() >= expires_at)
        {
            self.state = PositionResetState::Idle;
        }
    }
}

impl WindowMover {
    /// 创建尚未等待原生拖拽的窗口移动状态。
    pub(crate) fn new() -> Self {
        Self {
            native_drag_pending: false,
        }
    }

    /// 标记下一次鼠标移动应启动原生窗口拖拽。
    pub(crate) fn mouse_down(&mut self) {
        self.native_drag_pending = true;
    }

    /// 在按下后的首个移动事件中启动原生窗口拖拽。
    pub(crate) fn mouse_move(&mut self, window: &Window) {
        if self.native_drag_pending {
            self.native_drag_pending = false;
            window.start_window_move();
        }
    }

    /// 清除尚未消费的拖拽请求。
    pub(crate) fn mouse_up(&mut self) {
        self.native_drag_pending = false;
    }
}

/// 将现有窗口移动到与首次启动一致的默认居中位置。
///
/// X11 和 Windows 允许客户端更新顶层窗口坐标；Wayland 由合成器独占位置控制，
/// 此时返回 `false`，但调用方仍可清除保存的位置供下次创建窗口使用。
pub(crate) fn move_window_to_default(window: &Window, cx: &App) -> bool {
    let size = window.window_bounds().get_bounds().size;
    let origin = WindowBounds::centered(size, cx).get_bounds().origin;
    move_window(window, origin)
}

#[cfg(target_os = "linux")]
fn move_window(window: &Window, origin: Point<Pixels>) -> bool {
    use raw_window_handle::{HasDisplayHandle, HasWindowHandle, RawDisplayHandle, RawWindowHandle};

    use super::xcb::{xcb_configure_window, xcb_flush};

    let Ok(window_handle) = HasWindowHandle::window_handle(window) else {
        return false;
    };
    let RawWindowHandle::Xcb(window_handle) = window_handle.as_raw() else {
        return false;
    };
    let Ok(display_handle) = HasDisplayHandle::display_handle(window) else {
        return false;
    };
    let RawDisplayHandle::Xcb(display_handle) = display_handle.as_raw() else {
        return false;
    };
    let Some(connection) = display_handle.connection else {
        return false;
    };
    let values = [
        window_coordinate(origin.x, window.scale_factor()) as u32,
        window_coordinate(origin.y, window.scale_factor()) as u32,
    ];
    const X_AND_Y: u16 = (1 << 0) | (1 << 1);

    // SAFETY: 连接和窗口 ID 均来自仍存活的当前 GPUI X11 窗口；`values` 在调用期间
    // 保持有效并按 XCB 协议提供与掩码顺序一致的两个 32 位坐标。调用发生在 UI 线程，
    // 不会与 GPUI 对同一连接的事件处理跨线程并发。
    unsafe {
        let _ = xcb_configure_window(
            connection.as_ptr(),
            window_handle.window.get(),
            X_AND_Y,
            values.as_ptr().cast(),
        );
        xcb_flush(connection.as_ptr()) > 0
    }
}

#[cfg(not(any(target_os = "linux", target_os = "windows")))]
fn move_window(_window: &Window, _origin: Point<Pixels>) -> bool {
    false
}

/// 将 GPUI 逻辑坐标换算为原生窗口系统使用的物理像素坐标。
#[cfg(any(target_os = "linux", target_os = "windows"))]
fn window_coordinate(value: Pixels, scale_factor: f32) -> i32 {
    (f32::from(value) * scale_factor)
        .round()
        .clamp(i32::MIN as f32, i32::MAX as f32) as i32
}

#[cfg(target_os = "windows")]
fn move_window(window: &Window, origin: Point<Pixels>) -> bool {
    use std::ffi::c_void;

    use raw_window_handle::{HasWindowHandle, RawWindowHandle};
    use windows::Win32::{
        Foundation::HWND,
        UI::WindowsAndMessaging::{SWP_NOACTIVATE, SWP_NOSIZE, SWP_NOZORDER, SetWindowPos},
    };

    let Ok(handle) = HasWindowHandle::window_handle(window) else {
        return false;
    };
    let RawWindowHandle::Win32(handle) = handle.as_raw() else {
        return false;
    };
    let hwnd = HWND(handle.hwnd.get() as *mut c_void);
    // 调用方已按 GPUI 的显示器布局算出目标原点；自行居中会把多显示器窗口拉回主屏。
    let scale_factor = window.scale_factor();
    let x = window_coordinate(origin.x, scale_factor);
    let y = window_coordinate(origin.y, scale_factor);

    // SAFETY: `hwnd` 来自当前 UI 线程中仍存活的 GPUI 窗口；调用只修改位置，
    // 不改变尺寸、Z 序或激活状态。
    unsafe {
        SetWindowPos(
            hwnd,
            None,
            x,
            y,
            0,
            0,
            SWP_NOACTIVATE | SWP_NOSIZE | SWP_NOZORDER,
        )
        .is_ok()
    }
}
