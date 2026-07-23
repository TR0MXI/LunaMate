//! 使用同一 HWND 的 non-topmost DirectComposition target 承载 Live2D。

use std::num::NonZeroIsize;

use gpui::Window;
use gpui_wgpu::wgpu;
use raw_window_handle::{HasWindowHandle, RawDisplayHandle, RawWindowHandle};

use super::SurfaceSeed;

/// Windows surface 直接附着同一 HWND，不需要额外 UI 线程资源。
pub(super) struct NativeAttachment;
/// 保持与其他平台一致的 surface owner 接口。
pub(super) struct SurfaceOwner;

impl SurfaceOwner {
    /// Windows DComp surface 不需要逐帧提交额外原生状态。
    pub(super) fn prepare_present(&mut self, _size: super::GpuUnderlaySize) -> Result<(), String> {
        Ok(())
    }
}

/// 把 UI 线程创建的 HWND surface 转交给 GPU worker。
pub(super) struct SurfaceFactory {
    seed: SurfaceSeed,
}

/// 尝试在同一 HWND 上创建 non-topmost DirectComposition surface。
pub(super) fn attach(
    window: &Window,
) -> Result<Option<(SurfaceFactory, NativeAttachment)>, String> {
    if std::env::var("GPUI_DISABLE_DIRECT_COMPOSITION")
        .ok()
        .is_some_and(|value| value == "1" || value.eq_ignore_ascii_case("true"))
    {
        return Ok(None);
    }
    let handle = HasWindowHandle::window_handle(window)
        .map_err(|error| format!("无法取得 GPUI Windows 窗口句柄：{error}"))?;
    let RawWindowHandle::Win32(handle) = handle.as_raw() else {
        return Ok(None);
    };
    let mut descriptor = wgpu::InstanceDescriptor::new_without_display_handle();
    descriptor.backends = wgpu::Backends::DX12;
    descriptor.backend_options.dx12.presentation_system = wgpu::Dx12SwapchainKind::DxgiFromVisual;
    let instance = wgpu::Instance::new(descriptor);
    let hwnd = NonZeroIsize::new(handle.hwnd.get())
        .ok_or_else(|| "GPUI Windows 窗口句柄为空".to_owned())?;
    let mut window_handle = raw_window_handle::Win32WindowHandle::new(hwnd);
    window_handle.hinstance = handle.hinstance;
    let raw_window_handle = RawWindowHandle::Win32(window_handle);
    let raw_display_handle =
        RawDisplayHandle::Windows(raw_window_handle::WindowsDisplayHandle::new());
    // SAFETY: 句柄来自当前 UI 线程中仍存活的 GPUI 窗口；GpuUnderlay 在窗口释放前
    // 同步停止 worker 并销毁 surface。该 surface 只占用 non-topmost DComp target。
    let surface = unsafe {
        instance.create_surface_unsafe(wgpu::SurfaceTargetUnsafe::RawHandle {
            raw_display_handle: Some(raw_display_handle),
            raw_window_handle,
        })
    }
    .map_err(|error| format!("无法创建 Windows Live2D DirectComposition surface：{error}"))?;
    Ok(Some((
        SurfaceFactory {
            seed: SurfaceSeed::new(instance, surface, SurfaceOwner),
        },
        NativeAttachment,
    )))
}

impl SurfaceFactory {
    /// 将已经建立的 surface 种子移动到 GPU worker。
    pub(super) fn create(self, _mailbox: &super::WorkerMailbox) -> Result<SurfaceSeed, String> {
        Ok(self.seed)
    }
}
