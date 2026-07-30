//! 集中索引原生窗口、屏幕捕获与 underlay attachment 测试。

mod screenshot;
mod tray;
#[cfg(target_os = "linux")]
mod underlay_wayland;
mod window;
