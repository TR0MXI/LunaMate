//! 启动 LunaMate 桌面应用。

#![cfg_attr(
    all(target_os = "windows", not(debug_assertions)),
    windows_subsystem = "windows"
)]

mod app;
mod config;
mod database;
mod logging;
mod model;
mod platform;
mod shortcut;
mod ui;
mod voice;

rust_i18n::i18n!("locales", fallback = "en", minify_key = true);

use mimalloc::MiMalloc;

#[global_allocator]
static GLOBAL_ALLOCATOR: MiMalloc = MiMalloc;

fn main() {
    logging::install_panic_hook();
    let logging_settings = config::CONFIG.logging_settings().as_ref().to_owned();
    let logger_guard = logging::init(logging_settings);
    config::CONFIG.log_startup_summary();
    log::info!(
        "event=process_started version={} os={} arch={}",
        env!("CARGO_PKG_VERSION"),
        std::env::consts::OS,
        std::env::consts::ARCH
    );
    app::run();
    log::info!("event=process_exiting");
    drop(logger_guard);
}
