//! 集中初始化 flexi_logger，并把配置域映射为进程级日志行为。
//!
//! 日志句柄由本模块持有到 GPUI 事件循环结束；设置界面只通过这里修改过滤等级，
//! 不直接依赖 flexi_logger 的类型。文件路径和异步写入是应用级约束，不从用户输入构造。
//! 诊断文件不是用户界面：本模块和启动入口只写固定、稳定的事件与字段，不读取
//! `AppLanguage`，也不调用 i18n。用户可见的状态与错误仍由各自 UI 边界负责本地化。

use std::{fmt, fmt::Write as _, path::PathBuf, sync::OnceLock};

use flexi_logger::{
    Age, Cleanup, Criterion, DeferredNow, FileSpec, Logger, LoggerHandle, Naming, WriteMode,
};
use parking_lot::Mutex;

use crate::config::{CONFIG, LogLevel, LoggingSettings};

mod crash;
#[cfg(target_os = "windows")]
mod windows_security;

#[cfg(test)]
mod tests;

pub(crate) use crash::install_panic_hook;

/// 日志目录固定相对于应用启动目录，便于桌面环境收集诊断文件。
const LOG_DIRECTORY: &str = "./logs/lunamate";

const LOG_BASENAME: &str = "lunamate";
const APPLICATION_LOG_TARGET: &str = env!("CARGO_PKG_NAME");
const AGENT_LOG_TARGET: &str = "lunamate_agent";
const MAX_LOG_FIELD_BYTES: usize = 512;

struct LoggerRuntime {
    handle: LoggerHandle,
    /// 启动时应用到 file writer 的配置；运行期间只用于识别需要延后生效的变化。
    startup_file_settings: Option<LoggingSettings>,
}

struct BuiltLogger {
    logger: Box<dyn log::Log>,
    handle: LoggerHandle,
}

/// 运行期日志设置的应用结果；文件策略从不在运行期重建 writer。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ApplyLoggingSettingsOutcome {
    /// 过滤等级已经应用，启动时的文件策略保持有效。
    LevelApplied,
    /// 过滤等级已经应用，持久化文件策略要到下次启动才会生效。
    FilePolicyDeferredUntilRestart,
}

static LOGGER_HANDLE: OnceLock<Mutex<Option<LoggerRuntime>>> = OnceLock::new();

/// 保证主线程正常返回或 unwind 时都会排空并关闭异步日志器。
pub(crate) struct LoggerGuard {
    shutdown_pending: bool,
}

impl LoggerGuard {
    fn new(shutdown_pending: bool) -> Self {
        Self { shutdown_pending }
    }

    fn take_shutdown(&mut self) -> bool {
        std::mem::take(&mut self.shutdown_pending)
    }
}

impl Drop for LoggerGuard {
    fn drop(&mut self) {
        if self.take_shutdown() {
            shutdown();
        }
    }
}

struct SanitizedLogField {
    output: String,
}

impl SanitizedLogField {
    fn new() -> Self {
        Self {
            output: String::with_capacity(MAX_LOG_FIELD_BYTES),
        }
    }

    fn push(&mut self, value: &str) -> fmt::Result {
        if self.output.len().saturating_add(value.len()) > MAX_LOG_FIELD_BYTES {
            return Err(fmt::Error);
        }
        self.output.push_str(value);
        Ok(())
    }
}

impl fmt::Write for SanitizedLogField {
    fn write_str(&mut self, value: &str) -> fmt::Result {
        for character in value.chars() {
            match character {
                '\\' => self.push("\\\\")?,
                ' ' => self.push("\\x20")?,
                '\r' => self.push("\\r")?,
                '\n' => self.push("\\n")?,
                '\t' => self.push("\\t")?,
                character if character.is_control() => {
                    self.push(&format!("\\u{{{:x}}}", u32::from(character)))?;
                }
                character => {
                    let mut encoded = [0; 4];
                    self.push(character.encode_utf8(&mut encoded))?;
                }
            }
        }
        Ok(())
    }
}

/// 把确有诊断价值的自由文本压缩为有界、单行且不会触发终端控制序列的字段值。
pub(crate) fn sanitize_log_field(value: impl fmt::Display) -> String {
    let mut sanitized = SanitizedLogField::new();
    let _ = write!(&mut sanitized, "{value}");
    sanitized.output
}

fn stable_log_format(
    writer: &mut dyn std::io::Write,
    now: &mut DeferredNow,
    record: &log::Record<'_>,
) -> std::io::Result<()> {
    let current_thread = std::thread::current();
    let thread = sanitize_log_field(current_thread.name().unwrap_or("unnamed"));
    let target = sanitize_log_field(record.target());
    write!(
        writer,
        "timestamp={} thread={thread} level={} target={target} {}",
        now.now_utc_owned().format("%Y-%m-%dT%H:%M:%S%.3fZ"),
        record.level(),
        record.args()
    )
}

/// 初始化文件日志；文件目录不可写时降级为异步 stderr，避免日志宏完全失效。
pub(crate) fn init(settings: LoggingSettings) -> LoggerGuard {
    let file_logger = crash::prepare_log_directory()
        .map_err(|_| "failed to prepare the private log directory".to_owned())
        .and_then(|()| build_file_logger(settings));
    let (built, startup_file_settings, used_stderr_fallback) = match file_logger {
        Ok(built) => (built, Some(settings), false),
        Err(_) => match build_stderr_logger(settings.level) {
            Ok(built) => (built, None, true),
            Err(_) => {
                eprintln!("event=logging_unavailable reason=logger_start_failed");
                return LoggerGuard::new(false);
            }
        },
    };

    let BuiltLogger { logger, handle } = built;
    if log::set_boxed_logger(logger).is_err() {
        handle.shutdown();
        eprintln!("event=logging_unavailable reason=global_logger_install_failed");
        return LoggerGuard::new(false);
    }
    let runtime = LoggerRuntime {
        handle,
        startup_file_settings,
    };

    if LOGGER_HANDLE.set(Mutex::new(Some(runtime))).is_err() {
        eprintln!("event=logging_runtime_duplicate");
        return LoggerGuard::new(false);
    }
    if used_stderr_fallback {
        log::warn!("event=logging_file_fallback reason=file_writer_unavailable");
    }
    LoggerGuard::new(true)
}

/// 根据全局最新快照更新过滤等级；文件 writer 参数在下次启动时生效。
pub(crate) fn apply_current_settings() -> Result<ApplyLoggingSettingsOutcome, String> {
    let slot = LOGGER_HANDLE
        .get()
        .ok_or_else(|| "logger is not initialized".to_owned())?;
    // 在 writer 锁内读取快照，确保迟到任务不会在较新任务之后重新应用较早配置。
    let mut guard = slot.lock();
    let settings = CONFIG
        .logging_settings()
        .as_ref()
        .to_owned()
        .normalized()
        .map_err(|_| "logging settings are invalid".to_owned())?;
    let runtime = guard
        .as_mut()
        .ok_or_else(|| "logger is already shut down".to_owned())?;

    let outcome = settings_apply_outcome(runtime.startup_file_settings, settings);
    runtime
        .handle
        .parse_new_spec(&application_log_spec(settings.level))
        .map_err(|_| "failed to update the log level".to_owned())?;
    drop(guard);
    if outcome == ApplyLoggingSettingsOutcome::FilePolicyDeferredUntilRestart {
        log::debug!(
            "event=logging_file_settings_deferred rotation={} compression={} max_size_mb={} keep_files={}",
            settings.rotation,
            settings.compression,
            settings.max_size_mb,
            settings.keep_files
        );
    } else {
        log::debug!("event=logging_level_applied level={}", settings.level.id());
    }
    Ok(outcome)
}

fn settings_apply_outcome(
    startup: Option<LoggingSettings>,
    current: LoggingSettings,
) -> ApplyLoggingSettingsOutcome {
    if startup.is_none_or(|startup| file_policy_differs(startup, current)) {
        ApplyLoggingSettingsOutcome::FilePolicyDeferredUntilRestart
    } else {
        ApplyLoggingSettingsOutcome::LevelApplied
    }
}

/// 比较影响 file writer 的轮转字段；`level` 由日志过滤器单独应用。
pub(crate) fn file_policy_differs(left: LoggingSettings, right: LoggingSettings) -> bool {
    left.rotation != right.rotation
        || left.compression != right.compression
        || left.max_size_mb != right.max_size_mb
        || left.keep_files != right.keep_files
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

fn build_file_logger(settings: LoggingSettings) -> Result<BuiltLogger, String> {
    let (logger, handle) = file_logger(settings, file_spec())?
        .build()
        .map_err(|_| "failed to build file logging".to_owned())?;
    if crash::prepare_log_directory().is_err() {
        handle.shutdown();
        return Err("failed to protect the active log file".to_owned());
    }
    Ok(BuiltLogger { logger, handle })
}

fn file_logger(settings: LoggingSettings, file_spec: FileSpec) -> Result<Logger, String> {
    let logger = Logger::try_with_str(application_log_spec(settings.level))
        .map_err(|_| "failed to create the log filter".to_owned())?
        .log_to_file(file_spec)
        .append()
        .write_mode(WriteMode::Async)
        .format(stable_log_format)
        .use_utc()
        .panic_if_error_channel_is_broken(false);
    Ok(configured_logger(logger, settings))
}

fn build_stderr_logger(level: LogLevel) -> Result<BuiltLogger, String> {
    let (logger, handle) = Logger::try_with_str(application_log_spec(level))
        .map_err(|_| "failed to create the stderr log filter".to_owned())?
        .log_to_stderr()
        .write_mode(WriteMode::Async)
        .format(stable_log_format)
        .use_utc()
        .panic_if_error_channel_is_broken(false)
        .build()
        .map_err(|_| "failed to build stderr logging".to_owned())?;
    Ok(BuiltLogger { logger, handle })
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
    format!(
        "off,{APPLICATION_LOG_TARGET}={level},{AGENT_LOG_TARGET}={level}",
        level = level.id()
    )
}
