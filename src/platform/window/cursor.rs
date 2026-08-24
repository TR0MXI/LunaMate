//! 查询桌宠窗口外的全局光标位置。

use gpui::{Pixels, Point, Window, px};

/// 在原生平台允许时查询桌宠窗口外的当前光标位置。
pub(crate) struct GlobalCursorTracker;

impl GlobalCursorTracker {
    /// 仅为能够查询全局光标的窗口后端创建追踪器。
    pub(crate) fn new(window: &Window) -> Option<Self> {
        supports_global_cursor_tracking(window).then_some(Self)
    }

    /// 返回光标相对桌宠窗口左上角的逻辑坐标，坐标可以落在窗口范围之外。
    pub(crate) fn position(&self, window: &Window) -> Option<Point<Pixels>> {
        global_cursor_position(window)
    }
}

#[cfg(target_os = "windows")]
fn supports_global_cursor_tracking(window: &Window) -> bool {
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};

    HasWindowHandle::window_handle(window)
        .is_ok_and(|handle| matches!(handle.as_raw(), RawWindowHandle::Win32(_)))
}

#[cfg(target_os = "macos")]
fn supports_global_cursor_tracking(window: &Window) -> bool {
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};

    HasWindowHandle::window_handle(window)
        .is_ok_and(|handle| matches!(handle.as_raw(), RawWindowHandle::AppKit(_)))
}

#[cfg(target_os = "linux")]
fn supports_global_cursor_tracking(window: &Window) -> bool {
    use raw_window_handle::{HasDisplayHandle, HasWindowHandle, RawDisplayHandle, RawWindowHandle};

    let Ok(window_handle) = HasWindowHandle::window_handle(window) else {
        return false;
    };
    let Ok(display_handle) = HasDisplayHandle::display_handle(window) else {
        return false;
    };
    matches!(window_handle.as_raw(), RawWindowHandle::Xcb(_))
        && matches!(display_handle.as_raw(), RawDisplayHandle::Xcb(_))
}

#[cfg(not(any(target_os = "windows", target_os = "macos", target_os = "linux")))]
fn supports_global_cursor_tracking(_window: &Window) -> bool {
    false
}

#[cfg(target_os = "windows")]
fn global_cursor_position(window: &Window) -> Option<Point<Pixels>> {
    use std::ffi::c_void;

    use raw_window_handle::{HasWindowHandle, RawWindowHandle};
    use windows::Win32::{
        Foundation::{HWND, POINT},
        Graphics::Gdi::ScreenToClient,
        UI::WindowsAndMessaging::GetCursorPos,
    };

    let handle = HasWindowHandle::window_handle(window).ok()?;
    let RawWindowHandle::Win32(handle) = handle.as_raw() else {
        return None;
    };
    let hwnd = HWND(handle.hwnd.get() as *mut c_void);
    let mut position = POINT::default();
    // SAFETY: HWND 来自当前 UI 线程中仍存活的 GPUI 窗口；两个调用只同步读取光标位置，
    // `position` 在调用期间保持有效，且不会被原生 API 保存。
    unsafe {
        GetCursorPos(&mut position).ok()?;
        if !ScreenToClient(hwnd, &mut position).as_bool() {
            return None;
        }
    }
    Some(logical_cursor_position(
        position.x as f32,
        position.y as f32,
        window.scale_factor(),
    ))
}

#[cfg(target_os = "macos")]
fn global_cursor_position(window: &Window) -> Option<Point<Pixels>> {
    use objc2_app_kit::NSView;
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};

    let handle = HasWindowHandle::window_handle(window).ok()?;
    let RawWindowHandle::AppKit(handle) = handle.as_raw() else {
        return None;
    };
    // SAFETY: raw-window-handle 保证该指针在 WindowHandle 生命周期内指向当前 GPUI NSView。
    let native_view = unsafe { handle.ns_view.cast::<NSView>().as_ref() };
    let native_window = native_view.window()?;
    let position = native_window.mouseLocationOutsideOfEventStream();
    let height = f32::from(window.viewport_size().height);
    Some(Point::new(
        px(position.x as f32),
        px(height - position.y as f32),
    ))
}

#[cfg(target_os = "linux")]
fn global_cursor_position(window: &Window) -> Option<Point<Pixels>> {
    use std::ptr::NonNull;

    use raw_window_handle::{HasDisplayHandle, HasWindowHandle, RawDisplayHandle, RawWindowHandle};

    use super::xcb::{free, xcb_query_pointer, xcb_query_pointer_reply};

    let window_handle = HasWindowHandle::window_handle(window).ok()?;
    let RawWindowHandle::Xcb(window_handle) = window_handle.as_raw() else {
        return None;
    };
    let display_handle = HasDisplayHandle::display_handle(window).ok()?;
    let RawDisplayHandle::Xcb(display_handle) = display_handle.as_raw() else {
        return None;
    };
    let connection = display_handle.connection?;

    // SAFETY: XCB 连接和窗口 ID 来自当前 UI 线程中仍存活的 GPUI X11 窗口；请求与
    // GPUI 的 X11 事件处理在同一线程串行执行。reply 由 libxcb 分配并在读取后立即释放。
    let reply = unsafe {
        let cookie = xcb_query_pointer(connection.as_ptr(), window_handle.window.get());
        xcb_query_pointer_reply(connection.as_ptr(), cookie, std::ptr::null_mut())
    };
    let reply = NonNull::new(reply)?;
    // SAFETY: 非空 reply 指向完整的 xcb_query_pointer_reply_t，释放前只按值读取字段。
    let (same_screen, x, y) = unsafe {
        let reply_ref = reply.as_ref();
        (
            reply_ref.same_screen != 0,
            f32::from(reply_ref.win_x),
            f32::from(reply_ref.win_y),
        )
    };
    // SAFETY: reply 由上面的 xcb_query_pointer_reply 唯一返回，尚未释放或转移。
    unsafe { free(reply.as_ptr().cast()) };
    same_screen.then(|| logical_cursor_position(x, y, window.scale_factor()))
}

#[cfg(not(any(target_os = "windows", target_os = "macos", target_os = "linux")))]
fn global_cursor_position(_window: &Window) -> Option<Point<Pixels>> {
    None
}

#[cfg(any(target_os = "windows", target_os = "linux"))]
fn logical_cursor_position(x: f32, y: f32, scale_factor: f32) -> Point<Pixels> {
    let scale_factor = if scale_factor.is_finite() && scale_factor > 0.0 {
        scale_factor
    } else {
        1.0
    };
    Point::new(px(x / scale_factor), px(y / scale_factor))
}
