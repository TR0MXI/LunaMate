//! 隔离原生窗口适配与 underlay surface attachment，并提供窄平台接口。

mod tray;
mod underlay;
mod window;

#[cfg(test)]
mod tests;

pub(crate) use tray::{SystemTray, SystemTrayAction, TrayIconStyle, TrayMenuAnchor};
pub(crate) use underlay::{
    InitializationCancellation, NativeAttachment, SurfaceFactory, SurfaceOwner, SurfaceSeed,
    UnderlaySize, attach as attach_underlay,
};
pub(crate) use window::{
    GlobalCursorTracker, NativeTrayMenuWindow, WindowMover, WindowPositionController,
    configure_desktop_pet_window, configure_settings_window, configure_tray_menu_window,
    set_desktop_pet_window_visible,
};
