//! 隔离原生 underlay attachment，并向模型 worker 提供窄 WGPU surface 契约。

use std::time::Duration;

use gpui::Window;
use gpui_wgpu::wgpu;

#[cfg(target_os = "macos")]
mod macos;
#[cfg(not(any(target_os = "windows", target_os = "macos", target_os = "linux")))]
mod unsupported;
#[cfg(target_os = "linux")]
mod wayland;
#[cfg(target_os = "windows")]
mod windows;

#[cfg(target_os = "macos")]
use macos as implementation;
#[cfg(not(any(target_os = "windows", target_os = "macos", target_os = "linux")))]
use unsupported as implementation;
#[cfg(target_os = "linux")]
use wayland as implementation;
#[cfg(target_os = "windows")]
use windows as implementation;

#[cfg(all(test, target_os = "linux"))]
pub(in crate::platform) use implementation::exact_buffer_scale;
pub(crate) use implementation::{NativeAttachment, SurfaceFactory, SurfaceOwner};

/// 同时保存交换链物理像素和合成器逻辑尺寸。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct UnderlaySize {
    pub(crate) physical: [u32; 2],
    pub(crate) logical: [u32; 2],
}

/// 允许平台初始化等待 worker 关闭，但不暴露 mailbox 的其他状态。
pub(crate) trait InitializationCancellation {
    fn is_shutdown(&self) -> bool;
    fn wait_for_shutdown(&self, timeout: Duration) -> bool;
}

/// 在设备初始化完成前整体保持 surface、原生 owner 与 instance 的析构顺序。
pub(crate) struct SurfaceSeed {
    pub(crate) surface: wgpu::Surface<'static>,
    pub(crate) owner: SurfaceOwner,
    pub(crate) instance: wgpu::Instance,
}

impl SurfaceSeed {
    /// 按 surface、owner、instance 的字段顺序保存平台 surface 种子。
    fn new(instance: wgpu::Instance, surface: wgpu::Surface<'static>, owner: SurfaceOwner) -> Self {
        Self {
            surface,
            owner,
            instance,
        }
    }
}

/// 尝试为当前原生窗口建立 underlay attachment；不支持的后端返回 `None`。
pub(crate) fn attach(
    window: &Window,
) -> Result<Option<(SurfaceFactory, NativeAttachment)>, String> {
    implementation::attach(window)
}
