//! 负责配置路径定位、安全读取、损坏备份与原子写回。

use std::{
    env, fs,
    io::{self, Read as _},
    path::{Path, PathBuf},
};

use toml_edit::DocumentMut;

use crate::config::atomic_file::{
    AtomicReplaceError, AtomicReplaceOperation, PreparedAtomicReplace, VisibleAtomicReplace,
    atomic_replace, prepare_atomic_replace,
};

use super::super::{ConfigWriteError, LoadedConfig};
use super::parse_document;

const MAX_CONFIG_FILE_BYTES: u64 = 1024 * 1024;

/// 返回平台用户配置目录中的默认路径；无法确定绝对目录时不提供写入位置。
pub fn default_config_path() -> Option<PathBuf> {
    #[cfg(target_os = "windows")]
    let directory = absolute_env_path("APPDATA");
    #[cfg(target_os = "macos")]
    let directory =
        absolute_env_path("HOME").map(|home| home.join("Library").join("Application Support"));
    #[cfg(all(not(target_os = "windows"), not(target_os = "macos")))]
    let directory = absolute_env_path("XDG_CONFIG_HOME")
        .or_else(|| absolute_env_path("HOME").map(|home| home.join(".config")));

    directory.map(|directory| directory.join("LunaMate").join("config.toml"))
}

fn absolute_env_path(name: &str) -> Option<PathBuf> {
    env::var_os(name)
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
}

/// 读取并解析完整配置，失败时返回默认值与可展示的启动诊断。
pub fn read_config_file(path: &Path) -> (LoadedConfig, Option<String>) {
    match read_config_source(path) {
        Ok(Some(bytes)) => match std::str::from_utf8(&bytes) {
            Ok(source) => match source.parse::<DocumentMut>() {
                Ok(document) => parse_document(&document),
                Err(error) => (
                    LoadedConfig::default(),
                    Some(format!(
                        "配置文件 {} 无法解析，已使用默认值：{}",
                        path.display(),
                        error.message()
                    )),
                ),
            },
            Err(_) => (
                LoadedConfig::default(),
                Some(format!(
                    "配置文件 {} 不是有效的 UTF-8，已使用默认值",
                    path.display()
                )),
            ),
        },
        Ok(None) => (LoadedConfig::default(), None),
        Err(error) => (
            LoadedConfig::default(),
            Some(format!(
                "配置文件 {} 无法读取，已使用默认值：{error}",
                path.display()
            )),
        ),
    }
}

/// 读取用于精确更新的 TOML；损坏内容会在备份原文件后于本次保存时重建。
pub fn document_for_update(path: &Path, nonce: u64) -> Result<DocumentMut, ConfigWriteError> {
    match read_config_source(path) {
        Ok(Some(bytes)) => match std::str::from_utf8(&bytes) {
            Ok(source) => match source.parse::<DocumentMut>() {
                Ok(document) => Ok(document),
                Err(error) => rebuild_corrupt_document(path, &bytes, nonce, error.message()),
            },
            Err(_) => rebuild_corrupt_document(path, &bytes, nonce, "配置文件不是有效的 UTF-8"),
        },
        Ok(None) => Ok(DocumentMut::new()),
        Err(source) => Err(ConfigWriteError::Io {
            operation: "读取配置文件",
            path: path.to_path_buf(),
            source,
        }),
    }
}

fn rebuild_corrupt_document(
    path: &Path,
    bytes: &[u8],
    nonce: u64,
    reason: &str,
) -> Result<DocumentMut, ConfigWriteError> {
    // 重建会丢弃全部既有键，其中可能包含凭据；必须先留下权限受限的完整副本。
    let backup = backup_path(path);
    if let Err(error) = write_corrupt_backup(&backup, bytes, nonce) {
        log::error!(
            "event=config_corrupt_backup_failed error_kind={}",
            error.diagnostic_kind()
        );
        return Err(error);
    }
    let _ = (path, reason);
    log::warn!("event=config_document_rebuilt backup_created=true");
    Ok(DocumentMut::new())
}

fn write_corrupt_backup(path: &Path, bytes: &[u8], nonce: u64) -> Result<(), ConfigWriteError> {
    let Err(error) = atomic_replace(path, bytes, nonce) else {
        return Ok(());
    };
    let (operation, error_path, source) = error.into_parts();
    let operation = match operation {
        AtomicReplaceOperation::CreateTemporary => "创建损坏配置备份临时文件",
        #[cfg(unix)]
        AtomicReplaceOperation::SetPermissions => "设置损坏配置备份临时文件权限",
        AtomicReplaceOperation::WriteTemporary => "写入损坏配置备份临时文件",
        AtomicReplaceOperation::SyncTemporary => "同步损坏配置备份临时文件",
        AtomicReplaceOperation::Replace => "提交损坏配置备份",
        #[cfg(unix)]
        AtomicReplaceOperation::SyncParent => "同步损坏配置备份目录",
    };
    Err(ConfigWriteError::Io {
        operation,
        path: error_path,
        source,
    })
}

fn backup_path(path: &Path) -> PathBuf {
    let mut name = path.file_name().unwrap_or_default().to_os_string();
    name.push(".corrupt.bak");
    path.with_file_name(name)
}

fn read_config_source(path: &Path) -> io::Result<Option<Vec<u8>>> {
    let Some(file) = open_config_file(path)? else {
        return Ok(None);
    };
    let metadata = file.metadata()?;
    if !metadata.file_type().is_file() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "配置路径不是普通文件",
        ));
    }
    if metadata.len() > MAX_CONFIG_FILE_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("配置文件超过 {MAX_CONFIG_FILE_BYTES} 字节上限"),
        ));
    }

    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    file.take(MAX_CONFIG_FILE_BYTES + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_CONFIG_FILE_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("配置文件超过 {MAX_CONFIG_FILE_BYTES} 字节上限"),
        ));
    }
    Ok(Some(bytes))
}

#[cfg(unix)]
fn open_config_file(path: &Path) -> io::Result<Option<fs::File>> {
    use std::os::unix::fs::{MetadataExt as _, OpenOptionsExt as _, PermissionsExt as _};

    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
    if metadata.file_type().is_symlink() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "配置路径不能是符号链接",
        ));
    }
    if !metadata.file_type().is_file() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "配置路径不是普通文件",
        ));
    }

    let mut options = fs::OpenOptions::new();
    options.read(true);
    // Linux 与 Darwin 的稳定 ABI 标志阻止最终路径分量跟随符号链接；随后再复核 inode。
    #[cfg(any(target_os = "linux", target_os = "android"))]
    options.custom_flags(0o400000);
    #[cfg(any(target_os = "macos", target_os = "ios"))]
    options.custom_flags(0x0100);
    let file = match options.open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };

    let opened = file.metadata()?;
    let current = fs::symlink_metadata(path)?;
    if current.file_type().is_symlink()
        || current.dev() != opened.dev()
        || current.ino() != opened.ino()
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "配置路径在打开期间发生变化，已拒绝读取",
        ));
    }
    if opened.permissions().mode() & 0o777 != 0o600 {
        file.set_permissions(fs::Permissions::from_mode(0o600))?;
        if file.metadata()?.permissions().mode() & 0o777 != 0o600 {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "配置文件权限无法收紧为 0600",
            ));
        }
    }
    Ok(Some(file))
}

#[cfg(not(unix))]
fn open_config_file(path: &Path) -> io::Result<Option<fs::File>> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
    if metadata.file_type().is_symlink() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "配置路径不能是符号链接",
        ));
    }
    if !metadata.file_type().is_file() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "配置路径不是普通文件",
        ));
    }
    fs::File::open(path).map(Some)
}

/// 原子写回一份完整 TOML 文档，并把共享 helper 错误转换为配置错误。
pub fn write_config_file(
    path: &Path,
    document: &DocumentMut,
    nonce: u64,
) -> Result<(), ConfigWriteError> {
    let encoded = encode_config_file(document)?;
    finish_config_replace(atomic_replace(path, encoded.as_bytes(), nonce))
}

/// 完整写入并同步 revision 草稿，但暂不替换当前配置文件。
pub fn prepare_config_file(
    path: &Path,
    document: &DocumentMut,
    nonce: u64,
) -> Result<PreparedAtomicReplace, ConfigWriteError> {
    let encoded = encode_config_file(document)?;
    prepare_atomic_replace(path, encoded.as_bytes(), nonce).map_err(config_replace_error)
}

/// 原子替换已经同步的 revision 草稿，使其对读取方可见。
pub fn replace_config_file(
    prepared: PreparedAtomicReplace,
) -> Result<VisibleAtomicReplace, ConfigWriteError> {
    prepared.replace().map_err(config_replace_error)
}

/// 在可见提交之后同步配置父目录；失败沿用已提交配置的降级成功语义。
pub fn sync_config_file_parent(visible: VisibleAtomicReplace) -> Result<(), ConfigWriteError> {
    finish_config_replace(visible.sync_parent())
}

fn encode_config_file(document: &DocumentMut) -> Result<String, ConfigWriteError> {
    let encoded = document.to_string();
    if encoded.len() as u64 > MAX_CONFIG_FILE_BYTES {
        return Err(ConfigWriteError::InvalidValue(format!(
            "配置文件超过 {MAX_CONFIG_FILE_BYTES} 字节上限"
        )));
    }
    Ok(encoded)
}

fn finish_config_replace(result: Result<(), AtomicReplaceError>) -> Result<(), ConfigWriteError> {
    let Err(error) = result else {
        return Ok(());
    };
    let (operation, error_path, source) = error.into_parts();
    #[cfg(unix)]
    if matches!(operation, AtomicReplaceOperation::SyncParent) {
        // rename 已完成，不能把内存回滚到与磁盘不一致的旧值；这里只降低崩溃耐久性。
        log::warn!(
            "event=config_parent_sync_failed error_kind={:?}",
            source.kind()
        );
        return Ok(());
    }
    Err(config_replace_error_from_parts(
        operation, error_path, source,
    ))
}

fn config_replace_error(error: AtomicReplaceError) -> ConfigWriteError {
    let (operation, path, source) = error.into_parts();
    config_replace_error_from_parts(operation, path, source)
}

fn config_replace_error_from_parts(
    operation: AtomicReplaceOperation,
    path: PathBuf,
    source: io::Error,
) -> ConfigWriteError {
    let operation = match operation {
        AtomicReplaceOperation::CreateTemporary => "创建配置临时文件",
        #[cfg(unix)]
        AtomicReplaceOperation::SetPermissions => "设置配置临时文件权限",
        AtomicReplaceOperation::WriteTemporary => "写入配置临时文件",
        AtomicReplaceOperation::SyncTemporary => "同步配置临时文件",
        AtomicReplaceOperation::Replace => "提交配置文件",
        #[cfg(unix)]
        AtomicReplaceOperation::SyncParent => "同步配置目录",
    };
    ConfigWriteError::Io {
        operation,
        path,
        source,
    }
}
