//! 准备并显示 GPUI 托盘菜单的原生窗口。

use gpui::{Bounds, Pixels, Window};

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
