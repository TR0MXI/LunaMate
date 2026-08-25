//! 封装桌宠、设置与托盘菜单窗口的原生样式、拖动和位置行为。

mod cursor;
mod native;
mod placement;
mod tray_menu;

pub(crate) use cursor::GlobalCursorTracker;
pub(crate) use native::{
    configure_desktop_pet_window, configure_settings_window, configure_tray_menu_window,
    set_desktop_pet_window_visible,
};
pub(crate) use placement::{WindowMover, WindowPositionController, move_window_to_default};
pub(crate) use tray_menu::NativeTrayMenuWindow;
#[cfg(test)]
pub(in crate::platform) use tray_menu::physical_window_rect;
