//! 启动 LunaMate 桌面应用。

#![cfg_attr(target_os = "windows", windows_subsystem = "windows")]

mod animation;
mod app;
mod capabilities;
mod chat;
mod config;
mod expression;
mod frame_scheduler;
mod gpu_underlay;
mod interaction;
mod live2d_image;
mod logging;
mod model_view;
mod persistence;
mod platform_window;
mod theme;
mod window;

rust_i18n::i18n!("locales", fallback = "en");

use mimalloc::MiMalloc;

#[global_allocator]
static GLOBAL_ALLOCATOR: MiMalloc = MiMalloc;

fn main() {
    logging::init();
    if let Err(error) = logging::apply_current_settings() {
        log::error!("应用日志配置失败，继续使用启动配置：{error}");
    }
    app::run();
    logging::shutdown();
}
