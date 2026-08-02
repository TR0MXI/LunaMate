//! 通过权限受限临时文件为仍需保留文本格式的配置提供原子替换。

use std::{
    ffi::{OsStr, OsString},
    fs,
    io::{self, Write as _},
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

#[cfg(test)]
use std::sync::{Arc, Barrier};

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

/// 持有一份已经完整写入并同步、但尚未替换目标的同目录临时文件。
#[derive(Debug)]
pub(crate) struct PreparedAtomicReplace {
    temporary_path: PathBuf,
    target_path: PathBuf,
    replaced: bool,
    #[cfg(test)]
    parent_sync_barrier_for_test: Option<Arc<Barrier>>,
    #[cfg(all(test, unix))]
    parent_sync_failure_for_test: bool,
}

impl PreparedAtomicReplace {
    /// 原子替换目标，使新内容可见，但不在此阶段同步父目录。
    ///
    /// # Errors
    ///
    /// 重命名失败时返回完整 I/O 上下文，目标仍保持替换前状态。
    pub(crate) fn replace(mut self) -> Result<VisibleAtomicReplace, AtomicReplaceError> {
        fs::rename(&self.temporary_path, &self.target_path).map_err(|source| {
            atomic_error(AtomicReplaceOperation::Replace, &self.target_path, source)
        })?;
        self.replaced = true;
        Ok(VisibleAtomicReplace {
            target_path: self.target_path.clone(),
            #[cfg(test)]
            parent_sync_barrier_for_test: self.parent_sync_barrier_for_test.take(),
            #[cfg(all(test, unix))]
            parent_sync_failure_for_test: self.parent_sync_failure_for_test,
        })
    }

    #[cfg(test)]
    pub(crate) fn block_parent_sync_for_test(&mut self, barrier: Arc<Barrier>) {
        self.parent_sync_barrier_for_test = Some(barrier);
    }

    #[cfg(all(test, unix))]
    pub(crate) fn fail_parent_sync_for_test(&mut self) {
        self.parent_sync_failure_for_test = true;
    }
}

impl Drop for PreparedAtomicReplace {
    fn drop(&mut self) {
        if self.replaced {
            return;
        }
        if let Err(error) = fs::remove_file(&self.temporary_path)
            && error.kind() != io::ErrorKind::NotFound
        {
            log::warn!(
                "event=atomic_replace_cleanup_failed target_role=config error_kind={:?}",
                error.kind()
            );
        }
    }
}

/// 表示目标已经原子可见，但父目录尚未完成耐久性同步。
#[derive(Debug)]
#[must_use = "必须同步父目录才能完成原子替换的耐久性保证"]
pub(crate) struct VisibleAtomicReplace {
    target_path: PathBuf,
    #[cfg(test)]
    parent_sync_barrier_for_test: Option<Arc<Barrier>>,
    #[cfg(all(test, unix))]
    parent_sync_failure_for_test: bool,
}

impl VisibleAtomicReplace {
    /// 同步父目录，使已经可见的替换在系统崩溃后仍可恢复。
    ///
    /// # Errors
    ///
    /// 父目录无法打开或同步时返回完整 I/O 上下文；此时目标新内容已经可见。
    pub(crate) fn sync_parent(self) -> Result<(), AtomicReplaceError> {
        #[cfg(test)]
        if let Some(barrier) = self.parent_sync_barrier_for_test {
            barrier.wait();
            barrier.wait();
        }
        #[cfg(all(test, unix))]
        if self.parent_sync_failure_for_test {
            return Err(atomic_error(
                AtomicReplaceOperation::SyncParent,
                parent_directory(&self.target_path),
                io::Error::other("测试注入的父目录同步失败"),
            ));
        }
        sync_parent_directory(&self.target_path)
    }
}

/// 创建同目录权限受限临时文件，并完整写入、同步待提交内容。
///
/// `caller_nonce` 应使用调用方的 revision 或单调 nonce。临时名还包含 PID 与进程内序号，
/// 因而不同调用方不会复用固定临时路径。
///
/// # Errors
///
/// 临时文件创建、权限设置、写入或同步失败时返回完整 I/O 上下文。
pub(crate) fn prepare_atomic_replace(
    path: &Path,
    contents: &[u8],
    caller_nonce: u64,
) -> Result<PreparedAtomicReplace, AtomicReplaceError> {
    let sequence = TEMPORARY_FILE_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let prepared = PreparedAtomicReplace {
        temporary_path: temporary_path(path, caller_nonce, sequence),
        target_path: path.to_path_buf(),
        replaced: false,
        #[cfg(test)]
        parent_sync_barrier_for_test: None,
        #[cfg(all(test, unix))]
        parent_sync_failure_for_test: false,
    };
    write_temporary(&prepared.temporary_path, contents)?;
    Ok(prepared)
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
    prepare_atomic_replace(path, contents, caller_nonce)?
        .replace()?
        .sync_parent()
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

fn write_temporary(temporary_path: &Path, contents: &[u8]) -> Result<(), AtomicReplaceError> {
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
    Ok(())
}

#[cfg(unix)]
fn sync_parent_directory(path: &Path) -> Result<(), AtomicReplaceError> {
    let parent = parent_directory(path);
    fs::File::open(parent)
        .and_then(|directory| directory.sync_all())
        .map_err(|source| atomic_error(AtomicReplaceOperation::SyncParent, parent, source))
}

#[cfg(unix)]
fn parent_directory(path: &Path) -> &Path {
    path.parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."))
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
