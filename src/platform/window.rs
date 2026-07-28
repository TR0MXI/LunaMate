//! 封装桌宠、设置与托盘菜单窗口的原生样式、拖动和位置行为。

use std::time::{Duration, Instant};

use gpui::{App, Bounds, Pixels, Point, Window, WindowBounds, px};

const POSITION_RESET_SUPPRESSION: Duration = Duration::from_millis(250);

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
    use cocoa::{base::id, foundation::NSPoint};
    use objc::{msg_send, runtime::Object, sel, sel_impl};
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};

    let handle = HasWindowHandle::window_handle(window).ok()?;
    let RawWindowHandle::AppKit(handle) = handle.as_raw() else {
        return None;
    };
    let native_view = handle.ns_view.as_ptr().cast::<Object>();
    // SAFETY: NSView 来自当前主线程中仍存活的 GPUI 窗口；查询返回值均为按值复制，
    // Objective-C 消息不会保存 Rust 侧引用。
    unsafe {
        let native_window: id = msg_send![native_view, window];
        if native_window.is_null() {
            return None;
        }
        let position: NSPoint = msg_send![native_window, mouseLocationOutsideOfEventStream];
        let height = f32::from(window.viewport_size().height);
        Some(Point::new(
            px(position.x as f32),
            px(height - position.y as f32),
        ))
    }
}

#[cfg(target_os = "linux")]
fn global_cursor_position(window: &Window) -> Option<Point<Pixels>> {
    use std::ptr::NonNull;

    use raw_window_handle::{HasDisplayHandle, HasWindowHandle, RawDisplayHandle, RawWindowHandle};

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

/// 持有一次托盘菜单显示所需的原生窗口与最终逻辑边界。
pub(crate) struct NativeTrayMenuWindow {
    #[cfg(target_os = "windows")]
    hwnd: isize,
    #[cfg(target_os = "windows")]
    logical_bounds: Bounds<Pixels>,
    #[cfg(target_os = "windows")]
    initial_scale_factor: f64,
}

impl NativeTrayMenuWindow {
    /// 从仍隐藏的 GPUI 窗口准备一次原生显示操作。
    #[cfg(target_os = "windows")]
    pub(crate) fn prepare(
        window: &Window,
        logical_bounds: Bounds<Pixels>,
        scale_factor: f64,
    ) -> Result<Self, String> {
        use raw_window_handle::{HasWindowHandle, RawWindowHandle};

        let handle = HasWindowHandle::window_handle(window)
            .map_err(|error| format!("无法取得托盘菜单 Win32 句柄：{error}"))?;
        let RawWindowHandle::Win32(handle) = handle.as_raw() else {
            return Err("托盘菜单没有 Win32 原生句柄".to_owned());
        };
        Ok(Self {
            hwnd: handle.hwnd.get(),
            logical_bounds,
            initial_scale_factor: scale_factor,
        })
    }

    /// 非 Windows 平台的菜单已由 GPUI 直接显示，无需额外原生操作。
    #[cfg(not(target_os = "windows"))]
    pub(crate) fn prepare(
        _window: &Window,
        _logical_bounds: Bounds<Pixels>,
        _scale_factor: f64,
    ) -> Result<Self, String> {
        Ok(Self {})
    }

    /// 在最终物理位置显示窗口，避免重放 GPUI 按初始 HWND DPI 生成的 placement。
    #[cfg(target_os = "windows")]
    pub(crate) fn show(self) -> Result<(), String> {
        use std::ffi::c_void;

        use windows::Win32::{
            Foundation::HWND,
            UI::{
                HiDpi::GetDpiForWindow,
                WindowsAndMessaging::{
                    GetForegroundWindow, HWND_TOP, IsWindow, IsWindowVisible, SW_HIDE,
                    SWP_FRAMECHANGED, SWP_NOACTIVATE, SWP_NOZORDER, SWP_SHOWWINDOW,
                    SetForegroundWindow, SetWindowPos, ShowWindow, USER_DEFAULT_SCREEN_DPI,
                },
            },
        };

        let hwnd = HWND(self.hwnd as *mut c_void);
        // SAFETY: HWND 在窗口创建后从 GPUI 取得；任务执行前由调用方校验窗口 generation，
        // 此处再用 IsWindow 拒绝已销毁或失效的句柄。
        if !unsafe { IsWindow(Some(hwnd)) }.as_bool() {
            return Err("托盘菜单窗口已失效".to_owned());
        }
        let initial_rect = physical_window_rect(self.logical_bounds, self.initial_scale_factor);
        // SAFETY: HWND 已通过 IsWindow 校验；首次定位保持窗口隐藏，所有窗口消息都在 GPUI
        // 前台线程同步处理。坐标和尺寸已限制到 Win32 i32 范围。
        unsafe {
            SetWindowPos(
                hwnd,
                None,
                initial_rect[0],
                initial_rect[1],
                initial_rect[2],
                initial_rect[3],
                SWP_FRAMECHANGED | SWP_NOACTIVATE | SWP_NOZORDER,
            )
        }
        .map_err(|error| format!("定位隐藏托盘菜单失败：{error}"))?;

        // 首次 SetWindowPos 会同步完成跨显示器 WM_DPICHANGED；用窗口此刻的实际 DPI
        // 重算并再次写入同一逻辑边界，再把窗口显示出来。
        // SAFETY: HWND 在首次 SetWindowPos 返回后仍由当前任务持有且尚未交出控制权。
        let dpi = unsafe { GetDpiForWindow(hwnd) };
        let scale_factor = if dpi > 0 {
            f64::from(dpi) / f64::from(USER_DEFAULT_SCREEN_DPI)
        } else {
            self.initial_scale_factor
        };
        let final_rect = physical_window_rect(self.logical_bounds, scale_factor);
        // SAFETY: 与首次定位相同；SWP_SHOWWINDOW 在最终位置同步显示窗口，不读取保存的
        // WINDOWPLACEMENT，因此不会重新应用 CW_USEDEFAULT 所在显示器的 DPI。
        unsafe {
            SetWindowPos(
                hwnd,
                Some(HWND_TOP),
                final_rect[0],
                final_rect[1],
                final_rect[2],
                final_rect[3],
                SWP_NOACTIVATE | SWP_SHOWWINDOW,
            )
        }
        .map_err(|error| format!("显示托盘菜单失败：{error}"))?;

        // 托盘点击属于当前用户输入，正常情况下允许窗口取得前台；SetForegroundWindow
        // 同时激活窗口并转移键盘焦点，确保失焦观察器可以可靠关闭菜单。
        // SAFETY: 调用只同步引用已验证且当前可见的 HWND，不保存任何借用。
        let foreground = unsafe { SetForegroundWindow(hwnd) }.as_bool();
        // 某些任务栏增强工具会在显示或激活通知中同步移动新窗口；激活返回后立即重申
        // 最终矩形，且中间没有 await，不给错误位置留下独立的一帧。
        // SAFETY: HWND 仍由当前前台任务持有，最终矩形已完成范围限制。
        let final_position = unsafe {
            SetWindowPos(
                hwnd,
                None,
                final_rect[0],
                final_rect[1],
                final_rect[2],
                final_rect[3],
                SWP_NOACTIVATE | SWP_NOZORDER,
            )
        };
        if let Err(error) = final_position {
            // SAFETY: 最终定位失败时立即隐藏当前有效 HWND，避免与原生回退菜单重叠。
            unsafe {
                let _ = ShowWindow(hwnd, SW_HIDE);
            }
            return Err(format!("确认托盘菜单最终位置失败：{error}"));
        }
        // SAFETY: 两个查询只读取系统的窗口状态。
        let visible = unsafe { IsWindowVisible(hwnd) }.as_bool();
        let is_foreground = foreground || unsafe { GetForegroundWindow() == hwnd };
        if !visible || !is_foreground {
            // SAFETY: 激活失败时立即隐藏当前有效 HWND，避免与后续原生回退菜单重叠。
            unsafe {
                let _ = ShowWindow(hwnd, SW_HIDE);
            }
            return Err("系统拒绝激活托盘菜单窗口".to_owned());
        }
        Ok(())
    }

    #[cfg(not(target_os = "windows"))]
    pub(crate) fn show(self) -> Result<(), String> {
        Ok(())
    }
}

#[cfg(any(target_os = "windows", test))]
pub(in crate::platform) fn physical_window_rect(
    bounds: Bounds<Pixels>,
    scale_factor: f64,
) -> [i32; 4] {
    let scale_factor = if scale_factor.is_finite() && scale_factor > 0.0 {
        scale_factor
    } else {
        1.0
    };
    let coordinate = |value: Pixels| {
        (f64::from(f32::from(value)) * scale_factor)
            .round()
            .clamp(f64::from(i32::MIN), f64::from(i32::MAX)) as i32
    };
    let dimension = |value: Pixels| coordinate(value).max(1);
    [
        coordinate(bounds.origin.x),
        coordinate(bounds.origin.y),
        dimension(bounds.size.width),
        dimension(bounds.size.height),
    ]
}

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

/// 将 macOS 桌宠窗口修正为真正的无边框透明面板。
#[cfg(target_os = "macos")]
pub(crate) fn configure_desktop_pet_window(window: &Window) -> Result<(), String> {
    use cocoa::{
        appkit::{NSColor, NSWindow, NSWindowStyleMask},
        base::{NO, id, nil},
    };
    use objc::{msg_send, runtime::Object, sel, sel_impl};
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};

    let handle = HasWindowHandle::window_handle(window)
        .map_err(|error| format!("无法取得桌宠 AppKit 窗口句柄：{error}"))?;
    let RawWindowHandle::AppKit(handle) = handle.as_raw() else {
        return Err("桌宠窗口没有 AppKit 原生句柄".to_owned());
    };
    let native_view = handle.ns_view.as_ptr().cast::<Object>();

    // SAFETY: NSView 来自当前主线程中仍存活的 GPUI 窗口；样式和外观修改均同步发生在
    // AppKit 主线程。保留 GPUI 设置的 NonactivatingPanel 位，只移除会生成系统窗口框的位。
    unsafe {
        let native_window: id = msg_send![native_view, window];
        if native_window.is_null() {
            return Err("桌宠 NSView 尚未绑定 NSWindow".to_owned());
        }
        let existing_style = NSWindow::styleMask(native_window);
        let framed_style = NSWindowStyleMask::NSTitledWindowMask
            | NSWindowStyleMask::NSClosableWindowMask
            | NSWindowStyleMask::NSMiniaturizableWindowMask
            | NSWindowStyleMask::NSResizableWindowMask
            | NSWindowStyleMask::NSFullSizeContentViewWindowMask;
        NSWindow::setStyleMask_(native_window, existing_style & !framed_style);
        NSWindow::setHasShadow_(native_window, NO);
        NSWindow::setOpaque_(native_window, NO);
        NSWindow::setBackgroundColor_(native_window, NSColor::clearColor(nil));
    }
    Ok(())
}

/// 其他非 Windows 平台暂不追加原生样式；具体置顶语义由当前窗口后端决定。
#[cfg(not(any(target_os = "windows", target_os = "macos")))]
pub(crate) fn configure_desktop_pet_window(_window: &Window) -> Result<(), String> {
    Ok(())
}

/// 非 Windows 平台不需要额外关闭 DWM 原生边框。
#[cfg(not(target_os = "windows"))]
pub(crate) fn configure_settings_window(_window: &Window) -> Result<(), String> {
    Ok(())
}

/// 非 Windows 平台由透明内容边界和 GPUI 原生模糊窗口共同呈现圆角。
#[cfg(not(target_os = "windows"))]
pub(crate) fn configure_tray_menu_window(_window: &Window) -> Result<(), String> {
    Ok(())
}

/// 显示或隐藏桌宠原生窗口，同时保留 GPUI 实体与 Live2D 运行时。
#[cfg(target_os = "windows")]
pub(crate) fn set_desktop_pet_window_visible(window: &Window, visible: bool) -> Result<(), String> {
    use std::ffi::c_void;

    use raw_window_handle::{HasWindowHandle, RawWindowHandle};
    use windows::Win32::{
        Foundation::HWND,
        UI::WindowsAndMessaging::{SW_HIDE, SW_SHOWNOACTIVATE, ShowWindow},
    };

    let handle = HasWindowHandle::window_handle(window)
        .map_err(|error| format!("无法取得桌宠 Win32 窗口句柄：{error}"))?;
    let RawWindowHandle::Win32(handle) = handle.as_raw() else {
        return Err("桌宠窗口没有 Win32 原生句柄".to_owned());
    };
    let hwnd = HWND(handle.hwnd.get() as *mut c_void);

    // SAFETY: `hwnd` 来自当前 UI 线程中仍存活的 GPUI 窗口；ShowWindow 不保存传入指针，
    // 隐藏或无激活显示也不会改变窗口所有权和 underlay surface 生命周期。
    unsafe {
        let _ = ShowWindow(hwnd, if visible { SW_SHOWNOACTIVATE } else { SW_HIDE });
    }
    Ok(())
}

/// 显示或隐藏桌宠原生窗口，同时保留 GPUI 实体与 Live2D 运行时。
#[cfg(target_os = "macos")]
pub(crate) fn set_desktop_pet_window_visible(window: &Window, visible: bool) -> Result<(), String> {
    use cocoa::base::{id, nil};
    use objc::{msg_send, runtime::Object, sel, sel_impl};
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};

    let handle = HasWindowHandle::window_handle(window)
        .map_err(|error| format!("无法取得桌宠 AppKit 窗口句柄：{error}"))?;
    let RawWindowHandle::AppKit(handle) = handle.as_raw() else {
        return Err("桌宠窗口没有 AppKit 原生句柄".to_owned());
    };
    let native_view = handle.ns_view.as_ptr().cast::<Object>();

    // SAFETY: `native_view` 来自当前主线程中仍存活的 GPUI NSView；其 window 在查询和
    // 同步 order 调用期间保持有效，消息参数 `nil` 不转移任何对象所有权。
    unsafe {
        let native_window: id = msg_send![native_view, window];
        if native_window.is_null() {
            return Err("桌宠 NSView 尚未绑定 NSWindow".to_owned());
        }
        if visible {
            let _: () = msg_send![native_window, orderFrontRegardless];
        } else {
            let _: () = msg_send![native_window, orderOut: nil];
        }
    }
    Ok(())
}

/// 显示或隐藏桌宠原生窗口，同时保留 GPUI 实体与 Live2D 运行时。
#[cfg(target_os = "linux")]
pub(crate) fn set_desktop_pet_window_visible(window: &Window, visible: bool) -> Result<(), String> {
    use raw_window_handle::{HasDisplayHandle, HasWindowHandle, RawDisplayHandle, RawWindowHandle};

    let handle = HasWindowHandle::window_handle(window)
        .map_err(|error| format!("无法取得桌宠 Linux 窗口句柄：{error}"))?;
    match handle.as_raw() {
        RawWindowHandle::Xcb(handle) => {
            let display = HasDisplayHandle::display_handle(window)
                .map_err(|error| format!("无法取得桌宠 XCB display：{error}"))?;
            let RawDisplayHandle::Xcb(display) = display.as_raw() else {
                return Err("桌宠窗口与 display 的 XCB 类型不一致".to_owned());
            };
            let Some(connection) = display.connection else {
                return Err("桌宠 XCB display 没有可用连接".to_owned());
            };

            // SAFETY: 连接和窗口 ID 均来自仍存活的当前 GPUI X11 窗口；调用发生在 UI
            // 线程，不会与 GPUI 对同一连接的事件处理跨线程并发，随后立即 flush 请求。
            let flushed = unsafe {
                if visible {
                    let _ = xcb_map_window(connection.as_ptr(), handle.window.get());
                } else {
                    let _ = xcb_unmap_window(connection.as_ptr(), handle.window.get());
                }
                xcb_flush(connection.as_ptr())
            };
            if flushed <= 0 {
                return Err("提交桌宠 X11 显隐请求失败".to_owned());
            }
            if visible {
                window.activate_window();
            }
            Ok(())
        }
        RawWindowHandle::Wayland(_) => {
            // xdg-shell 没有客户端主动取消最小化的请求；隐藏使用最小化，恢复交给合成器
            // 处理 GPUI 的激活请求，避免绕过 GPUI 破坏 surface role。
            if visible {
                window.activate_window();
            } else {
                window.minimize_window();
            }
            Ok(())
        }
        _ => Err("当前 Linux 窗口后端不支持桌宠显隐".to_owned()),
    }
}

#[cfg(not(any(target_os = "windows", target_os = "macos", target_os = "linux")))]
pub(crate) fn set_desktop_pet_window_visible(window: &Window, visible: bool) -> Result<(), String> {
    if visible {
        window.activate_window();
    } else {
        window.minimize_window();
    }
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
#[repr(C)]
struct XcbQueryPointerCookie {
    sequence: u32,
}

#[cfg(target_os = "linux")]
#[repr(C)]
struct XcbQueryPointerReply {
    response_type: u8,
    same_screen: u8,
    sequence: u16,
    length: u32,
    root: u32,
    child: u32,
    root_x: i16,
    root_y: i16,
    win_x: i16,
    win_y: i16,
    mask: u16,
    padding: [u8; 2],
}

#[cfg(target_os = "linux")]
#[link(name = "xcb")]
unsafe extern "C" {
    fn xcb_map_window(connection: *mut std::ffi::c_void, window: u32) -> XcbVoidCookie;
    fn xcb_unmap_window(connection: *mut std::ffi::c_void, window: u32) -> XcbVoidCookie;
    fn xcb_query_pointer(connection: *mut std::ffi::c_void, window: u32) -> XcbQueryPointerCookie;
    fn xcb_query_pointer_reply(
        connection: *mut std::ffi::c_void,
        cookie: XcbQueryPointerCookie,
        error: *mut *mut std::ffi::c_void,
    ) -> *mut XcbQueryPointerReply;
    fn xcb_configure_window(
        connection: *mut std::ffi::c_void,
        window: u32,
        value_mask: u16,
        value_list: *const std::ffi::c_void,
    ) -> XcbVoidCookie;
    fn xcb_flush(connection: *mut std::ffi::c_void) -> i32;
}

#[cfg(target_os = "linux")]
unsafe extern "C" {
    fn free(pointer: *mut std::ffi::c_void);
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

/// 将 GPUI 逻辑坐标换算为原生窗口系统使用的物理像素坐标。
#[cfg(any(target_os = "linux", target_os = "windows"))]
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

/// 将 Windows 自绘菜单收紧为无过渡动画的原生弹出窗口。
#[cfg(target_os = "windows")]
pub(crate) fn configure_tray_menu_window(window: &Window) -> Result<(), String> {
    use std::{ffi::c_void, mem::size_of_val};

    use raw_window_handle::{HasWindowHandle, RawWindowHandle};
    use windows::Win32::{
        Foundation::{GetLastError, SetLastError, WIN32_ERROR},
        Graphics::Dwm::{
            DWMNCRP_DISABLED, DWMWA_BORDER_COLOR, DWMWA_COLOR_NONE, DWMWA_NCRENDERING_POLICY,
            DWMWA_TRANSITIONS_FORCEDISABLED, DWMWA_WINDOW_CORNER_PREFERENCE, DWMWCP_DONOTROUND,
            DwmSetWindowAttribute,
        },
        UI::WindowsAndMessaging::{GWL_STYLE, GetWindowLongPtrW, SetWindowLongPtrW, WS_POPUP},
    };

    let handle = HasWindowHandle::window_handle(window)
        .map_err(|error| format!("无法取得托盘菜单 Win32 句柄：{error}"))?;
    let RawWindowHandle::Win32(handle) = handle.as_raw() else {
        return Err("托盘菜单没有 Win32 原生句柄".to_owned());
    };
    let hwnd = windows::Win32::Foundation::HWND(handle.hwnd.get() as *mut c_void);
    let border_color = DWMWA_COLOR_NONE;
    let corner_preference = DWMWCP_DONOTROUND;
    let rendering_policy = DWMNCRP_DISABLED;
    let transitions_disabled: i32 = 1;

    // SAFETY: `hwnd` 来自当前 UI 线程中仍存活且尚未显示的托盘菜单窗口。样式操作不
    // 改变位置或可见性；DWM 属性指针只在同步调用期间引用具有准确字节长度的局部值。
    unsafe {
        SetLastError(WIN32_ERROR(0));
        let existing_style = GetWindowLongPtrW(hwnd, GWL_STYLE);
        let get_error = GetLastError();
        if existing_style == 0 && get_error.0 != 0 {
            return Err(format!(
                "读取托盘菜单窗口样式失败（Win32 错误 {}）",
                get_error.0
            ));
        }
        SetLastError(WIN32_ERROR(0));
        let previous_style = SetWindowLongPtrW(
            hwnd,
            GWL_STYLE,
            (existing_style as u32 | WS_POPUP.0) as isize,
        );
        let set_error = GetLastError();
        if previous_style == 0 && set_error.0 != 0 {
            return Err(format!(
                "设置托盘菜单弹出样式失败（Win32 错误 {}）",
                set_error.0
            ));
        }
        DwmSetWindowAttribute(
            hwnd,
            DWMWA_BORDER_COLOR,
            &border_color as *const _ as *const c_void,
            size_of_val(&border_color) as u32,
        )
        .map_err(|error| format!("关闭托盘菜单 DWM 边框失败：{error}"))?;
        DwmSetWindowAttribute(
            hwnd,
            DWMWA_NCRENDERING_POLICY,
            &rendering_policy as *const _ as *const c_void,
            size_of_val(&rendering_policy) as u32,
        )
        .map_err(|error| format!("关闭托盘菜单 DWM 非客户区渲染失败：{error}"))?;
        DwmSetWindowAttribute(
            hwnd,
            DWMWA_WINDOW_CORNER_PREFERENCE,
            &corner_preference as *const _ as *const c_void,
            size_of_val(&corner_preference) as u32,
        )
        .map_err(|error| format!("关闭托盘菜单 DWM 圆角失败：{error}"))?;
        DwmSetWindowAttribute(
            hwnd,
            DWMWA_TRANSITIONS_FORCEDISABLED,
            &transitions_disabled as *const _ as *const c_void,
            size_of_val(&transitions_disabled) as u32,
        )
        .map_err(|error| format!("关闭托盘菜单 DWM 过渡动画失败：{error}"))?;
    }
    Ok(())
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
