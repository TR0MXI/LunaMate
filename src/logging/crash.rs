//! 提供早于常规日志器的 panic 诊断与私有日志目录准备。
//!
//! 本模块刻意只依赖标准库，不调用 `log`、配置、i18n、日志句柄或关闭流程。崩溃记录不包含
//! panic payload，只保留有界的进程元数据、源码文件名和已去除路径 token 的 backtrace。
//! Rust panic hook 不覆盖显式 `abort`、OOM abort、致命信号、掉电或操作系统强制终止。

use std::fmt::Write as _;
use std::io::{Seek as _, Write as _};
use std::{
    backtrace::{Backtrace, BacktraceStatus},
    ffi::OsStr,
    fmt,
    fs::{self, File, Metadata},
    io,
    panic::{self, PanicHookInfo},
    path::{Component, Path, PathBuf},
    process,
    sync::{
        OnceLock,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::{SystemTime, UNIX_EPOCH},
};

#[cfg(not(target_os = "windows"))]
use std::fs::OpenOptions;
#[cfg(unix)]
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
#[cfg(target_os = "windows")]
use std::os::windows::fs::MetadataExt as _;

pub(super) const CRASH_LOG_BASENAME: &str = "lunamate-crash.log";
pub(super) const MAX_CRASH_RECORD_BYTES: usize = 64 * 1024;
const MAX_BACKTRACE_BYTES: usize = 48 * 1024;
const MAX_CRASH_FILE_BYTES: u64 = 1024 * 1024;
const MAX_LABEL_BYTES: usize = 96;
const PRIVATE_DIRECTORY_MODE: u32 = 0o700;
const PRIVATE_FILE_MODE: u32 = 0o600;

static PANIC_HOOK_INSTALLED: OnceLock<()> = OnceLock::new();
static CRASH_HOOK_ACTIVE: AtomicBool = AtomicBool::new(false);

#[cfg(all(unix, target_os = "macos"))]
type UnixMode = u16;
#[cfg(all(unix, not(target_os = "macos")))]
type UnixMode = u32;

#[cfg(unix)]
unsafe extern "C" {
    fn umask(mask: UnixMode) -> UnixMode;
    fn geteuid() -> u32;
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum CrashBacktraceStatus {
    Captured,
    Disabled,
    Unsupported,
    Unknown,
}

impl CrashBacktraceStatus {
    fn id(self) -> &'static str {
        match self {
            Self::Captured => "captured",
            Self::Disabled => "disabled",
            Self::Unsupported => "unsupported",
            Self::Unknown => "unknown",
        }
    }
}

impl From<BacktraceStatus> for CrashBacktraceStatus {
    fn from(status: BacktraceStatus) -> Self {
        match status {
            BacktraceStatus::Captured => Self::Captured,
            BacktraceStatus::Disabled => Self::Disabled,
            BacktraceStatus::Unsupported => Self::Unsupported,
            _ => Self::Unknown,
        }
    }
}

#[derive(Clone, Copy)]
pub(super) struct CrashLocation<'a> {
    pub(super) file: &'a str,
    pub(super) line: u32,
    pub(super) column: u32,
}

#[derive(Clone, Copy)]
pub(super) struct CrashContext<'a> {
    pub(super) unix_time_seconds: u64,
    pub(super) version: &'a str,
    pub(super) pid: u32,
    pub(super) thread_name: Option<&'a str>,
    pub(super) location: Option<CrashLocation<'a>>,
    pub(super) backtrace_status: CrashBacktraceStatus,
}

struct CrashHookInvocation;

impl Drop for CrashHookInvocation {
    fn drop(&mut self) {
        CRASH_HOOK_ACTIVE.store(false, Ordering::Release);
    }
}

struct BoundedText {
    text: String,
    capacity: usize,
    truncated: bool,
}

impl BoundedText {
    fn new(capacity: usize) -> Self {
        Self {
            text: String::with_capacity(capacity),
            capacity,
            truncated: false,
        }
    }

    #[cfg(test)]
    fn from_str(value: &str, capacity: usize) -> Self {
        let mut output = Self::new(capacity);
        let _ = output.write_str(value);
        output
    }

    fn from_display(value: &impl fmt::Display, capacity: usize) -> Self {
        let mut output = Self::new(capacity);
        let _ = write!(&mut output, "{value}");
        output
    }
}

impl fmt::Write for BoundedText {
    fn write_str(&mut self, value: &str) -> fmt::Result {
        for character in value.chars() {
            let character = if character.is_control() && character != '\n' && character != '\t' {
                '?'
            } else {
                character
            };
            if self.text.len() + character.len_utf8() > self.capacity {
                self.truncated = true;
                return Err(fmt::Error);
            }
            self.text.push(character);
        }
        Ok(())
    }
}

/// 安装一次进程级 hook；先同步写入独立 crash 文件，再调用安装前的默认 hook。
pub(crate) fn install_panic_hook() {
    PANIC_HOOK_INSTALLED.get_or_init(|| {
        let default_hook = panic::take_hook();
        panic::set_hook(Box::new(move |info| {
            let owns_crash_writer = CRASH_HOOK_ACTIVE
                .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
                .is_ok();
            let _invocation = owns_crash_writer.then_some(CrashHookInvocation);
            if owns_crash_writer {
                write_crash_record(info);
            }
            default_hook(info);
        }));
    });
}

/// 在 flexi_logger 启动前建立固定目录，并收紧已有诊断文件。
pub(super) fn prepare_log_directory() -> io::Result<()> {
    prepare_log_directory_at(Path::new(super::LOG_DIRECTORY))
}

pub(super) fn prepare_log_directory_at(directory: &Path) -> io::Result<()> {
    ensure_private_directory_at(directory)?;
    for entry in fs::read_dir(directory)? {
        let entry = entry?;
        if !is_lunamate_diagnostic_name(&entry.file_name()) {
            continue;
        }
        let file_type = entry.file_type()?;
        if file_type.is_symlink() || !file_type.is_file() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "diagnostic path must be a regular file",
            ));
        }
        set_private_owned_file_at(&entry.path())?;
    }
    Ok(())
}

fn write_crash_record(info: &PanicHookInfo<'_>) {
    let backtrace = Backtrace::force_capture();
    let backtrace_status = CrashBacktraceStatus::from(backtrace.status());
    let rendered_backtrace = (backtrace_status == CrashBacktraceStatus::Captured).then(|| {
        let raw = BoundedText::from_display(&backtrace, MAX_BACKTRACE_BYTES);
        sanitize_backtrace(&raw)
    });
    let current_thread = thread::current();
    let location = info.location().map(|location| CrashLocation {
        file: location.file(),
        line: location.line(),
        column: location.column(),
    });
    let context = CrashContext {
        unix_time_seconds: SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |duration| duration.as_secs()),
        version: env!("CARGO_PKG_VERSION"),
        pid: process::id(),
        thread_name: current_thread.name(),
        location,
        backtrace_status,
    };
    let record = assemble_crash_record(context, rendered_backtrace.as_ref());

    if persist_crash_record_at(Path::new(super::LOG_DIRECTORY), &record).is_err() {
        let mut stderr = io::stderr();
        let _ = stderr.write_all(
            b"event=crash_persistence_failed message=Crash record could not be persisted\n",
        );
        let _ = stderr.write_all(record.as_bytes());
        let _ = stderr.flush();
    }
}

#[cfg(test)]
pub(super) fn format_crash_record(context: CrashContext<'_>, backtrace: Option<&str>) -> String {
    let rendered_backtrace = backtrace.map(|value| {
        let raw = BoundedText::from_str(value, MAX_BACKTRACE_BYTES);
        sanitize_backtrace(&raw)
    });
    assemble_crash_record(context, rendered_backtrace.as_ref())
}

fn assemble_crash_record(context: CrashContext<'_>, backtrace: Option<&BoundedText>) -> String {
    let thread_name = sanitize_label(context.thread_name, "unnamed");
    let version = sanitize_label(Some(context.version), "unknown");
    let mut location = String::with_capacity(MAX_LABEL_BYTES + 24);
    if let Some(source) = context.location {
        let filename = source
            .file
            .rsplit(['/', '\\'])
            .next()
            .map_or("unknown", |value| value);
        let filename = sanitize_label(Some(filename), "unknown");
        let _ = write!(
            &mut location,
            "{filename}:{}:{}",
            source.line, source.column
        );
    } else {
        location.push_str("unknown:0:0");
    }

    let backtrace_truncated = backtrace.is_some_and(|value| value.truncated);
    let mut record = String::with_capacity(MAX_CRASH_RECORD_BYTES);
    let _ = writeln!(&mut record, "event=process_panic");
    let _ = writeln!(
        &mut record,
        "message=A Rust panic was observed; panic payload omitted"
    );
    let _ = writeln!(
        &mut record,
        "unix_time_seconds={}",
        context.unix_time_seconds
    );
    let _ = writeln!(&mut record, "version={version}");
    let _ = writeln!(&mut record, "pid={}", context.pid);
    let _ = writeln!(&mut record, "thread_name={thread_name}");
    let _ = writeln!(&mut record, "location={location}");
    let _ = writeln!(
        &mut record,
        "backtrace_status={}",
        context.backtrace_status.id()
    );
    let _ = writeln!(&mut record, "backtrace_truncated={backtrace_truncated}");
    if let Some(backtrace) = backtrace {
        let _ = writeln!(&mut record, "backtrace_begin");
        record.push_str(&backtrace.text);
        if !record.ends_with('\n') {
            record.push('\n');
        }
        let _ = writeln!(&mut record, "backtrace_end");
    }
    let _ = writeln!(&mut record, "record_end");
    record
}

fn sanitize_backtrace(raw: &BoundedText) -> BoundedText {
    let mut sanitized = BoundedText::new(MAX_BACKTRACE_BYTES);
    let mut token_start = 0;
    for (index, character) in raw.text.char_indices() {
        if !character.is_whitespace() {
            continue;
        }
        if !append_backtrace_token(&mut sanitized, &raw.text[token_start..index]) {
            sanitized.truncated = true;
            return sanitized;
        }
        let whitespace_end = index + character.len_utf8();
        if sanitized
            .write_str(&raw.text[index..whitespace_end])
            .is_err()
        {
            sanitized.truncated = true;
            return sanitized;
        }
        token_start = whitespace_end;
    }
    if !append_backtrace_token(&mut sanitized, &raw.text[token_start..]) {
        sanitized.truncated = true;
    }
    sanitized.truncated |= raw.truncated;
    sanitized
}

fn append_backtrace_token(output: &mut BoundedText, token: &str) -> bool {
    let contains_path_separator = token
        .chars()
        .any(|character| character == '/' || character == '\\');
    output
        .write_str(if contains_path_separator {
            "<path>"
        } else {
            token
        })
        .is_ok()
}

fn sanitize_label(value: Option<&str>, missing: &str) -> String {
    let Some(value) = value else {
        return missing.to_owned();
    };
    if value.is_empty()
        || value.len() > MAX_LABEL_BYTES
        || !value.chars().all(|character| {
            character.is_ascii_alphanumeric() || matches!(character, '-' | '_' | '.' | ':' | '+')
        })
    {
        return "redacted".to_owned();
    }
    value.to_owned()
}

pub(super) fn persist_crash_record_at(directory: &Path, record: &str) -> io::Result<()> {
    if record.len() > MAX_CRASH_RECORD_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "crash record exceeds its fixed bound",
        ));
    }
    ensure_private_directory_at(directory)?;
    let path = directory.join(CRASH_LOG_BASENAME);
    let mut file = open_private_crash_file(&path)?;
    let record_size = u64::try_from(record.len()).unwrap_or(u64::MAX);
    if file.metadata()?.len().saturating_add(record_size) > MAX_CRASH_FILE_BYTES {
        file.set_len(0)?;
    }
    file.seek(io::SeekFrom::End(0))?;
    file.write_all(record.as_bytes())?;
    file.flush()?;
    file.sync_all()
}

fn ensure_private_directory_at(directory: &Path) -> io::Result<()> {
    let parent = directory.parent().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "log directory must have a parent",
        )
    })?;
    fs::create_dir_all(parent)?;
    reject_link_components(parent)?;
    set_private_creation_mask();
    match fs::create_dir(directory) {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
        Err(error) => return Err(error),
    }
    reject_link_components(directory)?;
    let metadata = fs::symlink_metadata(directory)?;
    if metadata_is_link_like(&metadata) || !metadata.is_dir() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "log path must be a real directory",
        ));
    }
    set_private_path_permissions(directory, PRIVATE_DIRECTORY_MODE)
}

fn is_lunamate_diagnostic_name(name: &OsStr) -> bool {
    let Some(name) = name.to_str() else {
        return false;
    };
    if name == CRASH_LOG_BASENAME || name == "lunamate.log" {
        return true;
    }
    let Some(infix) = name.strip_prefix("lunamate_") else {
        return false;
    };
    (infix.ends_with(".log") || infix.ends_with(".log.gz"))
        && infix
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.'))
}

#[cfg(test)]
pub(super) fn is_lunamate_diagnostic_name_for_test(name: &OsStr) -> bool {
    is_lunamate_diagnostic_name(name)
}

fn reject_link_components(path: &Path) -> io::Result<()> {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()?.join(path)
    };
    let mut current = PathBuf::new();
    for component in absolute.components() {
        match component {
            Component::CurDir => continue,
            Component::ParentDir => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "log path must not contain parent traversal",
                ));
            }
            Component::Prefix(_) | Component::RootDir | Component::Normal(_) => {
                current.push(component.as_os_str());
            }
        }
        let metadata = fs::symlink_metadata(&current)?;
        if metadata_is_link_like(&metadata) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "log path must not traverse a link or reparse point",
            ));
        }
    }
    Ok(())
}

#[cfg(target_os = "windows")]
fn metadata_is_link_like(metadata: &Metadata) -> bool {
    const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;
    metadata.file_type().is_symlink()
        || metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
}

#[cfg(not(target_os = "windows"))]
fn metadata_is_link_like(metadata: &Metadata) -> bool {
    metadata.file_type().is_symlink()
}

#[cfg(unix)]
fn set_private_creation_mask() {
    // SAFETY: `umask` 不解引用指针；参数类型按 LunaMate 支持的 Linux/macOS ABI 匹配，
    // 进程启动阶段将掩码单向收紧为 0077，后续并发文件创建无需临界区。
    let _ = unsafe { umask(0o077) };
}

#[cfg(not(unix))]
fn set_private_creation_mask() {}

#[cfg(unix)]
fn set_private_path_permissions(path: &Path, mode: u32) -> io::Result<()> {
    let path_metadata = fs::symlink_metadata(path)?;
    validate_unix_directory(&path_metadata)?;
    let directory = File::open(path)?;
    let opened_metadata = directory.metadata()?;
    validate_unix_directory(&opened_metadata)?;
    if path_metadata.dev() != opened_metadata.dev() || path_metadata.ino() != opened_metadata.ino()
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "log directory identity changed during verification",
        ));
    }
    directory.set_permissions(fs::Permissions::from_mode(mode))?;
    let secured = directory.metadata()?;
    validate_unix_directory(&secured)?;
    if secured.permissions().mode() & 0o777 != mode {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "private directory mode verification failed",
        ));
    }
    let current_path = fs::symlink_metadata(path)?;
    if metadata_is_link_like(&current_path)
        || current_path.dev() != secured.dev()
        || current_path.ino() != secured.ino()
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "log directory path changed while securing it",
        ));
    }
    Ok(())
}

#[cfg(target_os = "windows")]
fn set_private_path_permissions(path: &Path, _mode: u32) -> io::Result<()> {
    super::windows_security::protect_directory(path)
}

#[cfg(unix)]
fn set_private_file_permissions(file: &File, mode: u32) -> io::Result<()> {
    validate_unix_file(&file.metadata()?)?;
    file.set_permissions(fs::Permissions::from_mode(mode))?;
    let secured = file.metadata()?;
    validate_unix_file(&secured)?;
    if secured.permissions().mode() & 0o777 != mode {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "private file mode verification failed",
        ));
    }
    Ok(())
}

#[cfg(all(not(unix), not(target_os = "windows")))]
fn set_private_file_permissions(_file: &File, _mode: u32) -> io::Result<()> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "private diagnostic permissions are unsupported on this platform",
    ))
}

#[cfg(all(not(unix), not(target_os = "windows")))]
fn set_private_path_permissions(_path: &Path, _mode: u32) -> io::Result<()> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "private diagnostic permissions are unsupported on this platform",
    ))
}

#[cfg(unix)]
fn set_private_owned_file_at(path: &Path) -> io::Result<()> {
    let path_metadata = fs::symlink_metadata(path)?;
    validate_unix_file(&path_metadata)?;
    let file = File::open(path)?;
    let opened_metadata = file.metadata()?;
    validate_unix_file(&opened_metadata)?;
    if path_metadata.dev() != opened_metadata.dev() || path_metadata.ino() != opened_metadata.ino()
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "diagnostic file identity changed during verification",
        ));
    }
    set_private_file_permissions(&file, PRIVATE_FILE_MODE)
}

#[cfg(target_os = "windows")]
fn set_private_owned_file_at(path: &Path) -> io::Result<()> {
    super::windows_security::protect_file(path)
}

#[cfg(all(not(unix), not(target_os = "windows")))]
fn set_private_owned_file_at(_path: &Path) -> io::Result<()> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "private diagnostic permissions are unsupported on this platform",
    ))
}

#[cfg(not(target_os = "windows"))]
fn open_private_crash_file(path: &Path) -> io::Result<File> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata_is_link_like(&metadata) || !metadata.is_file() => {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "crash record path must be a regular file",
            ));
        }
        Ok(_) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }

    let mut options = OpenOptions::new();
    options.write(true).create(true).append(true);
    #[cfg(unix)]
    options.mode(PRIVATE_FILE_MODE);
    let file = options.open(path)?;
    set_private_file_permissions(&file, PRIVATE_FILE_MODE)?;
    Ok(file)
}

#[cfg(target_os = "windows")]
fn open_private_crash_file(path: &Path) -> io::Result<File> {
    super::windows_security::open_private_append_file(path)
}

#[cfg(unix)]
fn validate_unix_directory(metadata: &Metadata) -> io::Result<()> {
    if metadata_is_link_like(metadata) || !metadata.is_dir() || metadata.uid() != effective_uid() {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "log directory ownership or type verification failed",
        ));
    }
    Ok(())
}

#[cfg(unix)]
fn validate_unix_file(metadata: &Metadata) -> io::Result<()> {
    if metadata_is_link_like(metadata)
        || !metadata.is_file()
        || metadata.uid() != effective_uid()
        || metadata.nlink() != 1
    {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "diagnostic file ownership or identity verification failed",
        ));
    }
    Ok(())
}

#[cfg(unix)]
fn effective_uid() -> u32 {
    // SAFETY: `geteuid` 无参数且不访问调用方内存，返回当前进程的有效用户 ID。
    unsafe { geteuid() }
}
