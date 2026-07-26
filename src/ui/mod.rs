//! 组合桌宠与设置窗口视图，并向其他子系统提供窄 UI 接口。

mod desktop_pet;
mod settings;
mod theme;
mod tray_menu;
mod window;

#[cfg(test)]
mod tests;

pub(crate) use desktop_pet::DesktopPetView;
pub(crate) use settings::{SettingsEvent, SettingsView, SettingsWindowView};
pub(crate) use theme::{UiPalette, apply, apply_language};
pub(in crate::ui) use tray_menu::{TrayMenuView, tray_menu_window_options};
pub(crate) use window::{
    cache_window_position, desktop_pet_window_min_size, desktop_pet_window_size, gpu_underlay_size,
    gpu_underlay_size_for_window, raster_dimensions_for_window, restored_window_bounds,
    settings_window_sizes,
};
