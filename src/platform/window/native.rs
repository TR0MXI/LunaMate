//! 配置各平台窗口的原生样式与显隐状态。

use gpui::Window;

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

    use super::xcb::{xcb_flush, xcb_map_window, xcb_unmap_window};

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
