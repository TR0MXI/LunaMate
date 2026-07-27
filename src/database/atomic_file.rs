//! 通过权限受限临时文件为仍需保留文本格式的配置提供原子替换。

use std::{
    ffi::{OsStr, OsString},
    fs,
    io::{self, Write as _},
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

static TEMPORARY_FILE_SEQUENCE: AtomicU64 = AtomicU64::new(0);

/// 标识原子替换失败时正在执行的文件系统操作。
#[derive(Clone, Copy, Debug)]
pub(crate) enum AtomicReplaceOperation {
    CreateTemporary,
    #[cfg(unix)]
    SetPermissions,
    WriteTemporary,
    SyncTemporary,
    Replace,
    #[cfg(unix)]
    SyncParent,
}

/// 保留底层操作、路径与 I/O 错误，供调用方转换为领域错误。
#[derive(Debug)]
pub(crate) struct AtomicReplaceError {
    operation: AtomicReplaceOperation,
    path: PathBuf,
    source: io::Error,
}

impl AtomicReplaceError {
    /// 将错误拆为调用方构造领域错误所需的上下文。
    pub(crate) fn into_parts(self) -> (AtomicReplaceOperation, PathBuf, io::Error) {
        (self.operation, self.path, self.source)
    }
}

/// 通过同目录临时文件持久化内容并原子替换目标文件。
///
/// `caller_nonce` 应使用调用方的 revision 或单调 nonce。临时名还包含 PID 与进程内序号，
/// 因而不同调用方不会复用固定临时路径。
///
/// # Errors
///
/// 临时文件创建、权限设置、写入、同步、重命名或父目录同步失败时返回完整 I/O 上下文。
pub(crate) fn atomic_replace(
    path: &Path,
    contents: &[u8],
    caller_nonce: u64,
) -> Result<(), AtomicReplaceError> {
    let sequence = TEMPORARY_FILE_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let temporary_path = temporary_path(path, caller_nonce, sequence);
    let result = write_and_replace(&temporary_path, path, contents);
    if result.is_err()
        && let Err(error) = fs::remove_file(&temporary_path)
        && error.kind() != io::ErrorKind::NotFound
    {
        log::warn!(
            "原子写入失败后无法清理临时文件：target_role=config, error_kind={:?}",
            error.kind()
        );
    }
    result
}

fn temporary_path(path: &Path, caller_nonce: u64, sequence: u64) -> PathBuf {
    let file_name = path
        .file_name()
        .unwrap_or_else(|| OsStr::new("lunamate-data"));
    let mut temporary_name = OsString::from(".");
    temporary_name.push(file_name);
    temporary_name.push(format!(
        ".tmp-{}-{caller_nonce}-{sequence}",
        std::process::id()
    ));
    path.with_file_name(temporary_name)
}

fn write_and_replace(
    temporary_path: &Path,
    path: &Path,
    contents: &[u8],
) -> Result<(), AtomicReplaceError> {
    let mut options = fs::OpenOptions::new();
    options.create_new(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;

        options.mode(0o600);
    }
    let mut file = options.open(temporary_path).map_err(|source| {
        atomic_error(
            AtomicReplaceOperation::CreateTemporary,
            temporary_path,
            source,
        )
    })?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;

        file.set_permissions(fs::Permissions::from_mode(0o600))
            .map_err(|source| {
                atomic_error(
                    AtomicReplaceOperation::SetPermissions,
                    temporary_path,
                    source,
                )
            })?;
    }

    file.write_all(contents).map_err(|source| {
        atomic_error(
            AtomicReplaceOperation::WriteTemporary,
            temporary_path,
            source,
        )
    })?;
    file.sync_all().map_err(|source| {
        atomic_error(
            AtomicReplaceOperation::SyncTemporary,
            temporary_path,
            source,
        )
    })?;
    drop(file);

    fs::rename(temporary_path, path)
        .map_err(|source| atomic_error(AtomicReplaceOperation::Replace, path, source))?;
    sync_parent_directory(path)
}

#[cfg(unix)]
fn sync_parent_directory(path: &Path) -> Result<(), AtomicReplaceError> {
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    fs::File::open(parent)
        .and_then(|directory| directory.sync_all())
        .map_err(|source| atomic_error(AtomicReplaceOperation::SyncParent, parent, source))
}

#[cfg(not(unix))]
fn sync_parent_directory(_: &Path) -> Result<(), AtomicReplaceError> {
    Ok(())
}

fn atomic_error(
    operation: AtomicReplaceOperation,
    path: &Path,
    source: io::Error,
) -> AtomicReplaceError {
    AtomicReplaceError {
        operation,
        path: path.to_path_buf(),
        source,
    }
}
