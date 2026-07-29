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

#[cfg(test)]
mod tests;

/// 日志目录固定相对于应用启动目录，便于桌面环境收集诊断文件。
const LOG_DIRECTORY: &str = "./logs";

const LOG_BASENAME: &str = "lunamate";
const APPLICATION_LOG_TARGET: &str = env!("CARGO_PKG_NAME");

struct LoggerRuntime {
    handle: LoggerHandle,
    has_file_writer: bool,
    /// 最近一次应用到 file writer 的轮转配置；仅等级变化时无需重建 writer。
    applied_file_settings: Option<LoggingSettings>,
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
                applied_file_settings: Some(default_settings),
            },
            None,
        ),
        Err(file_error) => match start_stderr_logger() {
            Ok(handle) => (
                LoggerRuntime {
                    handle,
                    has_file_writer: false,
                    applied_file_settings: None,
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
        log::warn!("{}", t!("log.file_fallback", reason = reason));
    }
}

/// 根据全局最新快照更新过滤等级和文件轮转；调用方应在后台执行器中调用。
pub(crate) fn apply_current_settings() -> Result<(), String> {
    let slot = LOGGER_HANDLE
        .get()
        .ok_or_else(|| "日志器尚未初始化".to_owned())?;
    // 在 writer 锁内读取快照，确保迟到任务不会在较新任务之后重新应用较早配置。
    let mut guard = slot.lock();
    let settings = CONFIG
        .logging_settings()
        .as_ref()
        .to_owned()
        .normalized()
        .map_err(|error| format!("日志配置无效：{error}"))?;
    let runtime = guard.as_mut().ok_or_else(|| "日志器已经关闭".to_owned())?;

    // reset_flw 会重建 writer 并重跑轮转与清理，只在轮转参数真正变化时执行。
    let mut writer_rebuilt = false;
    if runtime.has_file_writer
        && runtime
            .applied_file_settings
            .is_none_or(|applied| !file_writer_settings_match(applied, settings))
    {
        let builder = file_writer_builder(settings, file_spec());
        runtime
            .handle
            .reset_flw(&builder)
            .map_err(|error| format!("更新日志文件配置失败：{error}"))?;
        runtime.applied_file_settings = Some(settings);
        writer_rebuilt = true;
    }
    runtime
        .handle
        .parse_new_spec(&application_log_spec(settings.level))
        .map_err(|error| format!("更新日志等级失败：{error}"))?;
    drop(guard);
    log::debug!(
        "日志配置已应用：level={}, rotation={}, compression={}, max_size_mb={}, keep_files={}, writer_rebuilt={writer_rebuilt}",
        settings.level.id(),
        settings.rotation,
        settings.compression,
        settings.max_size_mb,
        settings.keep_files
    );
    Ok(())
}

/// 比较影响 file writer 的轮转字段；`level` 由日志过滤器单独应用。
fn file_writer_settings_match(left: LoggingSettings, right: LoggingSettings) -> bool {
    left.rotation == right.rotation
        && left.compression == right.compression
        && left.max_size_mb == right.max_size_mb
        && left.keep_files == right.keep_files
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
