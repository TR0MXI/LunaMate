//! 启动 LunaMate 桌面应用。

#![cfg_attr(target_os = "windows", windows_subsystem = "windows")]

mod agent;
mod app;
mod config;
mod database;
mod logging;
mod model;
mod platform;
mod ui;
mod voice;

rust_i18n::i18n!("locales", fallback = "en", minify_key = true);

use mimalloc::MiMalloc;
use rust_i18n::t;

#[global_allocator]
static GLOBAL_ALLOCATOR: MiMalloc = MiMalloc;

fn main() {
    logging::init();
    if let Err(error) = logging::apply_current_settings() {
        log::error!("{}", t!("log.apply_settings_failed", error = error));
    }
    config::CONFIG.log_startup_summary();
    log::info!(
        "LunaMate 启动：version={}, os={}, arch={}",
        env!("CARGO_PKG_VERSION"),
        std::env::consts::OS,
        std::env::consts::ARCH
    );
    app::run();
    log::info!("LunaMate 进程退出");
    logging::shutdown();
}
