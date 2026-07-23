//! 封装桌宠与设置窗口的原生样式、拖动和位置复位行为。

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
        let moved = move_window_to_default(window, cx);
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

/// 非 Windows 平台暂不追加原生样式；具体置顶语义由当前窗口后端决定。
#[cfg(not(target_os = "windows"))]
pub(crate) fn configure_desktop_pet_window(_window: &Window) -> Result<(), String> {
    Ok(())
}

/// 非 Windows 平台不需要额外关闭 DWM 原生边框。
#[cfg(not(target_os = "windows"))]
pub(crate) fn configure_settings_window(_window: &Window) -> Result<(), String> {
    Ok(())
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
#[repr(C)]
struct XcbVoidCookie {
    sequence: u32,
}

#[cfg(target_os = "linux")]
#[link(name = "xcb")]
unsafe extern "C" {
    fn xcb_configure_window(
        connection: *mut std::ffi::c_void,
        window: u32,
        value_mask: u16,
        value_list: *const std::ffi::c_void,
    ) -> XcbVoidCookie;
    fn xcb_flush(connection: *mut std::ffi::c_void) -> i32;
}

#[cfg(target_os = "linux")]
fn move_window(window: &Window, origin: Point<Pixels>) -> bool {
    use raw_window_handle::{HasDisplayHandle, HasWindowHandle, RawDisplayHandle, RawWindowHandle};

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

#[cfg(target_os = "linux")]
fn window_coordinate(value: Pixels, scale_factor: f32) -> i32 {
    (f32::from(value) * scale_factor)
        .round()
        .clamp(i32::MIN as f32, i32::MAX as f32) as i32
}

/// 将 Windows 桌宠窗口修正为无边框、无圆角并保持在最上层。
#[cfg(target_os = "windows")]
pub(crate) fn configure_desktop_pet_window(window: &Window) -> Result<(), String> {
    use std::{ffi::c_void, mem::size_of_val};

    use raw_window_handle::{HasWindowHandle, RawWindowHandle};
    use windows::Win32::{
        Foundation::{GetLastError, HWND, SetLastError, WIN32_ERROR},
        Graphics::Dwm::{
            DWMNCRP_DISABLED, DWMWA_BORDER_COLOR, DWMWA_COLOR_NONE, DWMWA_NCRENDERING_POLICY,
            DWMWA_WINDOW_CORNER_PREFERENCE, DWMWCP_DONOTROUND, DwmSetWindowAttribute,
        },
        UI::WindowsAndMessaging::{
            GWL_STYLE, GetWindowLongPtrW, HWND_TOPMOST, SWP_FRAMECHANGED, SWP_NOACTIVATE,
            SWP_NOMOVE, SWP_NOSIZE, SetWindowLongPtrW, SetWindowPos, WS_CAPTION, WS_MAXIMIZEBOX,
            WS_MINIMIZEBOX, WS_POPUP, WS_SYSMENU, WS_THICKFRAME,
        },
    };

    let handle = HasWindowHandle::window_handle(window)
        .map_err(|error| format!("无法取得桌宠 Win32 窗口句柄：{error}"))?;
    let RawWindowHandle::Win32(handle) = handle.as_raw() else {
        return Err("桌宠窗口没有 Win32 原生句柄".to_owned());
    };
    let hwnd = HWND(handle.hwnd.get() as *mut c_void);

    // SAFETY: `hwnd` 来自当前 UI 线程中仍然存活的 GPUI 窗口，样式与层级操作均在
    // 该线程完成。传给 DWM 的每个指针只在调用期间引用局部值，并使用精确字节长度。
    unsafe {
        // 先清理标题栏、系统菜单和可调整边框，再保留其余状态位并设置弹出式样式。
        SetLastError(WIN32_ERROR(0));
        let existing_style = GetWindowLongPtrW(hwnd, GWL_STYLE);
        let get_error = GetLastError();
        if existing_style == 0 && get_error.0 != 0 {
            return Err(format!(
                "读取桌宠窗口样式失败（Win32 错误 {}）",
                get_error.0
            ));
        }
        let conflicting =
            WS_CAPTION.0 | WS_THICKFRAME.0 | WS_SYSMENU.0 | WS_MINIMIZEBOX.0 | WS_MAXIMIZEBOX.0;
        let style = (existing_style as u32 & !conflicting) | WS_POPUP.0;
        SetLastError(WIN32_ERROR(0));
        let previous_style = SetWindowLongPtrW(hwnd, GWL_STYLE, style as isize);
        let set_style_error = GetLastError();
        if previous_style == 0 && set_style_error.0 != 0 {
            return Err(format!(
                "设置桌宠窗口样式失败（Win32 错误 {}）",
                set_style_error.0
            ));
        }
        SetWindowPos(
            hwnd,
            Some(HWND_TOPMOST),
            0,
            0,
            0,
            0,
            SWP_FRAMECHANGED | SWP_NOACTIVATE | SWP_NOMOVE | SWP_NOSIZE,
        )
        .map_err(|error| format!("设置桌宠窗口置顶失败：{error}"))?;

        let border_color = DWMWA_COLOR_NONE;
        DwmSetWindowAttribute(
            hwnd,
            DWMWA_BORDER_COLOR,
            &border_color as *const _ as *const c_void,
            size_of_val(&border_color) as u32,
        )
        .map_err(|error| format!("关闭桌宠 DWM 边框失败：{error}"))?;

        let corner_preference = DWMWCP_DONOTROUND;
        DwmSetWindowAttribute(
            hwnd,
            DWMWA_WINDOW_CORNER_PREFERENCE,
            &corner_preference as *const _ as *const c_void,
            size_of_val(&corner_preference) as u32,
        )
        .map_err(|error| format!("关闭桌宠 DWM 圆角失败：{error}"))?;

        let rendering_policy = DWMNCRP_DISABLED;
        DwmSetWindowAttribute(
            hwnd,
            DWMWA_NCRENDERING_POLICY,
            &rendering_policy as *const _ as *const c_void,
            size_of_val(&rendering_policy) as u32,
        )
        .map_err(|error| format!("关闭桌宠 DWM 非客户区渲染失败：{error}"))?;
    }
    Ok(())
}

/// 关闭 Windows DWM 为自绘设置窗口追加的原生边框颜色。
#[cfg(target_os = "windows")]
pub(crate) fn configure_settings_window(window: &Window) -> Result<(), String> {
    use std::{ffi::c_void, mem::size_of_val};

    use raw_window_handle::{HasWindowHandle, RawWindowHandle};
    use windows::Win32::Graphics::Dwm::{
        DWMWA_BORDER_COLOR, DWMWA_COLOR_NONE, DwmSetWindowAttribute,
    };

    let handle = HasWindowHandle::window_handle(window)
        .map_err(|error| format!("无法取得设置窗口 Win32 句柄：{error}"))?;
    let RawWindowHandle::Win32(handle) = handle.as_raw() else {
        return Err("设置窗口没有 Win32 原生句柄".to_owned());
    };
    let hwnd = windows::Win32::Foundation::HWND(handle.hwnd.get() as *mut c_void);
    let border_color = DWMWA_COLOR_NONE;

    // SAFETY: `hwnd` 来自当前 UI 线程中仍存活的 GPUI 窗口；属性指针只在同步调用
    // 期间引用具有准确字节长度的局部颜色值。
    unsafe {
        DwmSetWindowAttribute(
            hwnd,
            DWMWA_BORDER_COLOR,
            &border_color as *const _ as *const c_void,
            size_of_val(&border_color) as u32,
        )
        .map_err(|error| format!("关闭设置窗口 DWM 边框失败：{error}"))?;
    }
    Ok(())
}

#[cfg(target_os = "windows")]
fn move_window(window: &Window, _origin: Point<Pixels>) -> bool {
    use std::ffi::c_void;

    use raw_window_handle::{HasWindowHandle, RawWindowHandle};
    use windows::Win32::{
        Foundation::{HWND, POINT, RECT},
        Graphics::Gdi::{GetMonitorInfoW, MONITOR_DEFAULTTOPRIMARY, MONITORINFO, MonitorFromPoint},
        UI::WindowsAndMessaging::{
            GetWindowRect, SWP_NOACTIVATE, SWP_NOSIZE, SWP_NOZORDER, SetWindowPos,
        },
    };

    let Ok(handle) = HasWindowHandle::window_handle(window) else {
        return false;
    };
    let RawWindowHandle::Win32(handle) = handle.as_raw() else {
        return false;
    };
    let hwnd = HWND(handle.hwnd.get() as *mut c_void);
    let mut window_rect = RECT::default();
    let mut monitor_info = MONITORINFO {
        cbSize: std::mem::size_of::<MONITORINFO>() as u32,
        ..Default::default()
    };

    // SAFETY: `hwnd` 来自当前 UI 线程中仍存活的 GPUI 窗口；输出结构体在调用期间
    // 独占且具有 Win32 要求的 `cbSize`。最终调用只修改位置，不改变尺寸、Z 序或激活状态。
    unsafe {
        if GetWindowRect(hwnd, &mut window_rect).is_err() {
            return false;
        }
        let monitor = MonitorFromPoint(POINT::default(), MONITOR_DEFAULTTOPRIMARY);
        if monitor.is_invalid() || !GetMonitorInfoW(monitor, &mut monitor_info).as_bool() {
            return false;
        }
        let width = window_rect.right - window_rect.left;
        let height = window_rect.bottom - window_rect.top;
        let work_area = monitor_info.rcWork;
        let x = work_area.left + (work_area.right - work_area.left - width) / 2;
        let y = work_area.top + (work_area.bottom - work_area.top - height) / 2;
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reset_request_suppresses_bounds_before_move_is_applied() {
        let mut controller = WindowPositionController::default();
        controller.request_reset();

        assert!(!controller.observe_bounds());
    }
}
