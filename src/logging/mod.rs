//! 集中初始化 flexi_logger，并把配置域映射为进程级日志行为。
//!
//! 日志句柄由本模块持有到 GPUI 事件循环结束；设置界面只通过这里修改过滤和文件轮转，
//! 不直接依赖 flexi_logger 的类型。文件路径和异步写入是应用级约束，不从用户输入构造。

use std::{path::PathBuf, sync::OnceLock, time::Duration};

use flexi_logger::writers::{FileLogWriter, FileLogWriterBuilder};
use flexi_logger::{
    Age, Cleanup, Criterion, DEFAULT_MESSAGE_CAPA, DEFAULT_POOL_CAPA, FileSpec, Logger,
    LoggerHandle, Naming, WriteMode,
};
use parking_lot::Mutex;
use rust_i18n::t;

use crate::config::{CONFIG, LogLevel, LoggingSettings};

/// 日志目录固定相对于应用启动目录，便于桌面环境收集诊断文件。
const LOG_DIRECTORY: &str = "./logs";

const LOG_BASENAME: &str = "lunamate";
const APPLICATION_LOG_TARGET: &str = env!("CARGO_PKG_NAME");

struct LoggerRuntime {
    handle: LoggerHandle,
    has_file_writer: bool,
}

static LOGGER_HANDLE: OnceLock<Mutex<Option<LoggerRuntime>>> = OnceLock::new();

/// 初始化文件日志；文件目录不可写时降级为异步 stderr，避免日志宏完全失效。
pub(crate) fn init() {
    let default_settings = LoggingSettings::default();
    let (runtime, fallback_reason) = match start_file_logger(default_settings) {
        Ok(handle) => (
            LoggerRuntime {
                handle,
                has_file_writer: true,
            },
            None,
        ),
        Err(file_error) => match start_stderr_logger() {
            Ok(handle) => (
                LoggerRuntime {
                    handle,
                    has_file_writer: false,
                },
                Some(file_error),
            ),
            Err(_) => return,
        },
    };

    if LOGGER_HANDLE.set(Mutex::new(Some(runtime))).is_err() {
        return;
    }
    if let Some(reason) = fallback_reason {
        log::error!("{}", t!("log.file_fallback", reason = reason));
    }
}

/// 根据全局最新快照更新过滤等级和文件轮转；调用方应在后台执行器中调用。
pub(crate) fn apply_current_settings() -> Result<(), String> {
    let slot = LOGGER_HANDLE
        .get()
        .ok_or_else(|| "日志器尚未初始化".to_owned())?;
    // 在 writer 锁内读取快照，确保迟到任务不会在较新任务之后重新应用旧配置。
    let guard = slot.lock();
    let settings = CONFIG
        .logging_settings()
        .as_ref()
        .to_owned()
        .normalized()
        .map_err(|error| format!("日志配置无效：{error}"))?;
    let runtime = guard.as_ref().ok_or_else(|| "日志器已经关闭".to_owned())?;

    if runtime.has_file_writer {
        let builder = file_writer_builder(settings, file_spec());
        runtime
            .handle
            .reset_flw(&builder)
            .map_err(|error| format!("更新日志文件配置失败：{error}"))?;
    }
    runtime
        .handle
        .parse_new_spec(&application_log_spec(settings.level))
        .map_err(|error| format!("更新日志等级失败：{error}"))
}

/// 在应用退出时显式刷新并关闭异步写入线程。
pub(crate) fn shutdown() {
    let Some(slot) = LOGGER_HANDLE.get() else {
        return;
    };
    let handle = slot.lock().take();
    if let Some(runtime) = handle {
        runtime.handle.flush();
        runtime.handle.shutdown();
    }
}

fn start_file_logger(settings: LoggingSettings) -> Result<LoggerHandle, String> {
    file_logger(settings, file_spec())?
        .start()
        .map_err(|error| format!("启动文件日志失败：{error}"))
}

fn file_logger(settings: LoggingSettings, file_spec: FileSpec) -> Result<Logger, String> {
    let logger = Logger::try_with_str(application_log_spec(settings.level))
        .map_err(|error| format!("创建日志过滤器失败：{error}"))?
        .log_to_file(file_spec)
        .append()
        .write_mode(WriteMode::Async)
        .panic_if_error_channel_is_broken(false);
    Ok(configured_logger(logger, settings))
}

fn start_stderr_logger() -> Result<LoggerHandle, String> {
    Logger::try_with_str(application_log_spec(LogLevel::Warn))
        .map_err(|error| format!("创建 stderr 日志过滤器失败：{error}"))?
        .log_to_stderr()
        .write_mode(WriteMode::Async)
        .panic_if_error_channel_is_broken(false)
        .start()
        .map_err(|error| format!("启动 stderr 日志失败：{error}"))
}

fn configured_logger(logger: Logger, settings: LoggingSettings) -> Logger {
    if settings.rotation {
        logger.rotate(
            Criterion::AgeOrSize(Age::Day, settings.max_size_bytes()),
            Naming::Timestamps,
            cleanup(settings),
        )
    } else {
        logger
    }
}

fn file_writer_builder(settings: LoggingSettings, file_spec: FileSpec) -> FileLogWriterBuilder {
    let builder = FileLogWriter::builder(file_spec)
        .append()
        // Logger 会把 Async 转换为零刷新间隔的内部 writer 模式，定时刷新由句柄线程负责。
        .write_mode(WriteMode::AsyncWith {
            pool_capa: DEFAULT_POOL_CAPA,
            message_capa: DEFAULT_MESSAGE_CAPA,
            flush_interval: Duration::ZERO,
        });
    if settings.rotation {
        builder.rotate(
            Criterion::AgeOrSize(Age::Day, settings.max_size_bytes()),
            Naming::Timestamps,
            cleanup(settings),
        )
    } else {
        builder.o_rotate(None)
    }
}

fn cleanup(settings: LoggingSettings) -> Cleanup {
    let keep_files = settings.keep_files as usize;
    if settings.compression {
        Cleanup::KeepCompressedFiles(keep_files)
    } else {
        Cleanup::KeepLogFiles(keep_files)
    }
}

fn file_spec() -> FileSpec {
    FileSpec::default()
        .directory(PathBuf::from(LOG_DIRECTORY))
        .basename(LOG_BASENAME)
}

fn application_log_spec(level: LogLevel) -> String {
    // 第三方网络库的原始诊断不受本项目脱敏约束，因此只启用 LunaMate 自身目标。
    format!("off,{APPLICATION_LOG_TARGET}={}", level.id())
}

#[cfg(test)]
mod tests {
    use std::{env, fs, process};

    use flexi_logger::LogSpecification;
    use rust_i18n::t;

    use super::*;

    #[test]
    fn every_application_log_level_produces_a_valid_spec() {
        for level in [
            LogLevel::Error,
            LogLevel::Warn,
            LogLevel::Info,
            LogLevel::Debug,
            LogLevel::Trace,
        ] {
            assert!(LogSpecification::parse(application_log_spec(level)).is_ok());
        }
    }

    #[test]
    fn log_messages_exist_in_every_supported_language() {
        let keys = [
            "log.apply_settings_failed",
            "log.file_fallback",
            "log.config_rebuilt",
            "log.gpu_device_lost",
            "log.gpu_worker_panicked",
            "log.model_capability_warning",
            "log.chat_close_save_failed",
            "log.chat_save_failed",
            "log.frame_render_stopped",
            "log.pet_move_unsupported",
            "log.startup_config_warning",
            "log.pet_window_config_failed",
            "log.exit_chat_save_failed",
            "log.exit_position_save_failed",
            "log.main_window_create_failed",
            "log.gpu_underlay_init_failed",
            "log.gpu_worker_exited",
            "log.gpu_model_cpu_fallback",
            "log.gpu_underlay_cpu_fallback",
            "log.settings_window_config_failed",
            "log.settings_window_create_failed",
            "log.image_release_failed",
            "log.logging_update_failed",
            "log.settings_move_unsupported",
        ];

        for locale in ["zh-CN", "zh-TW", "en", "ja"] {
            for key in keys {
                assert!(
                    crate::_rust_i18n_try_translate(locale, key).is_some(),
                    "{locale} 缺少日志翻译：{key}"
                );
            }
        }
        assert_eq!(
            t!("log.chat_save_failed", locale = "en", error = "disk full"),
            "Failed to save the chat session: disk full"
        );
    }

    #[test]
    fn asynchronous_file_writer_can_be_reconfigured() {
        let directory =
            env::temp_dir().join(format!("lunamate-logging-reset-test-{}", process::id()));
        let _ = fs::remove_dir_all(&directory);
        let test_file_spec = || {
            FileSpec::default()
                .directory(directory.clone())
                .basename("lunamate-test")
        };
        {
            let logger = file_logger(LoggingSettings::default(), test_file_spec())
                .expect("测试日志器配置应当有效");
            let (_logger, handle) = logger.build().expect("测试日志器应当可以构建");
            let updated = LoggingSettings {
                max_size_mb: 25,
                keep_files: 5,
                ..LoggingSettings::default()
            };

            handle
                .reset_flw(&file_writer_builder(updated, test_file_spec()))
                .expect("启动和重配使用相同异步模式时应当允许重置");
            handle.shutdown();
        }
        let _ = fs::remove_dir_all(directory);
    }
}
