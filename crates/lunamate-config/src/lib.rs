//! 提供 LunaMate 的配置领域、原子持久化与一致性发布。

rust_i18n::i18n!("locales", fallback = "en", minify_key = true);

#[path = "mod.rs"]
pub mod config;

pub use config::*;
