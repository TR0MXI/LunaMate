//! 隔离原生窗口适配、屏幕捕获、用户图片读取与 underlay surface attachment，并提供窄平台接口。

mod screenshot;
mod tray;
mod underlay;
#[cfg(target_os = "linux")]
mod wayland_activation;
mod window;

#[cfg(test)]
mod tests;

pub(crate) use screenshot::{capture_primary_screen, load_agent_image};
pub(crate) use tray::{SystemTray, SystemTrayAction, TrayIconStyle, TrayMenuAnchor};
pub(crate) use underlay::{
    InitializationCancellation, NativeAttachment, SurfaceFactory, SurfaceOwner, SurfaceSeed,
    UnderlaySize, attach as attach_underlay,
};
#[cfg(target_os = "linux")]
pub(crate) use wayland_activation::{
    WaylandActivationController, WaylandActivationTarget, wayland_activation_target,
};
pub(crate) use window::{
    GlobalCursorTracker, NativeTrayMenuWindow, WindowMover, WindowPositionController,
    configure_desktop_pet_window, configure_settings_window, configure_tray_menu_window,
    set_desktop_pet_window_visible,
};

/// 桌面入口、Wayland surface 与 portal 权限存储共同使用的稳定应用标识。
pub(crate) const APPLICATION_ID: &str = "io.github.tr0mxi.lunamate";
