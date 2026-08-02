//! 从扫描根目录句柄逐级打开资源，避免路径替换改变解析锚点。

use std::{
    ffi::OsStr,
    fs,
    path::{Component, Path},
};

use super::{ModelDiagnosticCategory, ResourceResolutionError, scanned_root_changed_error};
use crate::model::catalog::ScannedModelRoot;

pub(super) fn open_anchored_file(
    root: &ScannedModelRoot,
    relative_path: &Path,
    display_path: &Path,
    maximum_bytes: u64,
    before_open: &mut dyn FnMut(&Path),
    after_open: &mut dyn FnMut(&Path),
) -> Result<fs::File, ResourceResolutionError> {
    let components = normal_components(relative_path)?;
    open_anchored_file_impl(
        root,
        &components,
        display_path,
        maximum_bytes,
        before_open,
        after_open,
    )
}

fn normal_components(path: &Path) -> Result<Vec<&OsStr>, ResourceResolutionError> {
    let mut components = Vec::new();
    for component in path.components() {
        match component {
            Component::Normal(component) => components.push(component),
            Component::CurDir => {}
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                return Err(ResourceResolutionError::new(
                    ModelDiagnosticCategory::InvalidReference,
                    "引用必须是扫描根目录内的相对路径",
                ));
            }
        }
    }
    if components.is_empty() {
        return Err(ResourceResolutionError::new(
            ModelDiagnosticCategory::InvalidReference,
            "引用路径为空",
        ));
    }
    Ok(components)
}

#[cfg(unix)]
fn open_anchored_file_impl(
    root: &ScannedModelRoot,
    components: &[&OsStr],
    display_path: &Path,
    maximum_bytes: u64,
    before_open: &mut dyn FnMut(&Path),
    after_open: &mut dyn FnMut(&Path),
) -> Result<fs::File, ResourceResolutionError> {
    use rustix::fs::{Mode, OFlags, open, openat};

    let directory_flags = OFlags::RDONLY | OFlags::CLOEXEC | OFlags::DIRECTORY | OFlags::NOFOLLOW;
    let root_fd = open(root.canonical_path(), directory_flags, Mode::empty())
        .map_err(|error| unix_open_error(root.canonical_path(), error))?;
    let root_directory = fs::File::from(root_fd);
    let root_metadata = root_directory
        .metadata()
        .map_err(|error| ResourceResolutionError::from_io(root.canonical_path(), &error))?;
    validate_directory_metadata(&root_metadata)?;
    if !root.matches_open_directory(&root_directory, &root_metadata) {
        return Err(scanned_root_changed_error());
    }

    let mut directories = Vec::with_capacity(components.len());
    directories.push(root_directory);
    for component in &components[..components.len() - 1] {
        let parent_index = directories.len() - 1;
        let directory_fd = openat(
            &directories[parent_index],
            Path::new(component),
            directory_flags,
            Mode::empty(),
        )
        .map_err(|error| unix_open_error(display_path, error))?;
        let directory = fs::File::from(directory_fd);
        let metadata = directory
            .metadata()
            .map_err(|error| ResourceResolutionError::from_io(display_path, &error))?;
        validate_directory_metadata(&metadata)?;
        directories.push(directory);
    }

    before_open(display_path);
    let parent_index = directories.len() - 1;
    let final_fd = openat(
        &directories[parent_index],
        Path::new(components[components.len() - 1]),
        OFlags::RDONLY | OFlags::CLOEXEC | OFlags::NOFOLLOW | OFlags::NONBLOCK,
        Mode::empty(),
    )
    .map_err(|error| unix_open_error(display_path, error))?;
    let file = fs::File::from(final_fd);
    after_open(display_path);
    let metadata = file
        .metadata()
        .map_err(|error| ResourceResolutionError::from_io(display_path, &error))?;
    validate_resource_metadata(&metadata, maximum_bytes)?;
    // 明确把整条父目录句柄链保持到 final 句柄完成校验之后。
    drop(directories);
    Ok(file)
}

#[cfg(unix)]
fn unix_open_error(path: &Path, error: rustix::io::Errno) -> ResourceResolutionError {
    if error == rustix::io::Errno::LOOP || error == rustix::io::Errno::NOTDIR {
        resource_path_changed_error()
    } else {
        let error = std::io::Error::from_raw_os_error(error.raw_os_error());
        ResourceResolutionError::from_io(path, &error)
    }
}

#[cfg(target_os = "windows")]
fn open_anchored_file_impl(
    root: &ScannedModelRoot,
    components: &[&OsStr],
    display_path: &Path,
    maximum_bytes: u64,
    before_open: &mut dyn FnMut(&Path),
    after_open: &mut dyn FnMut(&Path),
) -> Result<fs::File, ResourceResolutionError> {
    let root_directory = open_windows_directory(root.canonical_path())
        .map_err(|error| windows_open_error(root.canonical_path(), &error))?;
    let root_metadata = root_directory
        .metadata()
        .map_err(|error| ResourceResolutionError::from_io(root.canonical_path(), &error))?;
    validate_directory_metadata(&root_metadata)?;
    if !root.matches_open_directory(&root_directory, &root_metadata) {
        return Err(scanned_root_changed_error());
    }

    let mut directories = Vec::with_capacity(components.len());
    directories.push(root_directory);
    let mut current_path = root.canonical_path().to_path_buf();
    for component in &components[..components.len() - 1] {
        current_path.push(component);
        let directory = open_windows_directory(&current_path)
            .map_err(|error| windows_open_error(&current_path, &error))?;
        let metadata = directory
            .metadata()
            .map_err(|error| ResourceResolutionError::from_io(&current_path, &error))?;
        validate_directory_metadata(&metadata)?;
        directories.push(directory);
    }

    current_path.push(components[components.len() - 1]);
    before_open(display_path);
    let file = open_windows_final_file(&current_path)
        .map_err(|error| windows_open_error(display_path, &error))?;
    after_open(display_path);
    let metadata = file
        .metadata()
        .map_err(|error| ResourceResolutionError::from_io(display_path, &error))?;
    validate_resource_metadata(&metadata, maximum_bytes)?;
    // 无 DELETE share 的父目录句柄在 final 句柄完成校验前都不能释放。
    drop(directories);
    Ok(file)
}

#[cfg(target_os = "windows")]
fn open_windows_directory(path: &Path) -> std::io::Result<fs::File> {
    use std::os::windows::fs::OpenOptionsExt as _;

    const FILE_SHARE_READ: u32 = 0x0000_0001;
    const FILE_SHARE_WRITE: u32 = 0x0000_0002;
    const FILE_FLAG_BACKUP_SEMANTICS: u32 = 0x0200_0000;
    const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;

    let mut options = fs::OpenOptions::new();
    options
        .read(true)
        .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE)
        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT | FILE_FLAG_BACKUP_SEMANTICS);
    options.open(path)
}

#[cfg(target_os = "windows")]
fn open_windows_final_file(path: &Path) -> std::io::Result<fs::File> {
    use std::os::windows::fs::OpenOptionsExt as _;

    const FILE_SHARE_READ: u32 = 0x0000_0001;
    const FILE_SHARE_WRITE: u32 = 0x0000_0002;
    const FILE_FLAG_BACKUP_SEMANTICS: u32 = 0x0200_0000;
    const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;

    let mut options = fs::OpenOptions::new();
    options
        .read(true)
        .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE)
        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT | FILE_FLAG_BACKUP_SEMANTICS);
    options.open(path)
}

#[cfg(target_os = "windows")]
fn windows_open_error(path: &Path, error: &std::io::Error) -> ResourceResolutionError {
    const ERROR_CANT_ACCESS_FILE: i32 = 1920;
    if error.raw_os_error() == Some(ERROR_CANT_ACCESS_FILE) {
        resource_path_changed_error()
    } else {
        ResourceResolutionError::from_io(path, error)
    }
}

#[cfg(not(any(unix, target_os = "windows")))]
fn open_anchored_file_impl(
    _root: &ScannedModelRoot,
    _components: &[&OsStr],
    _display_path: &Path,
    _maximum_bytes: u64,
    _before_open: &mut dyn FnMut(&Path),
    _after_open: &mut dyn FnMut(&Path),
) -> Result<fs::File, ResourceResolutionError> {
    Err(ResourceResolutionError::new(
        ModelDiagnosticCategory::Read,
        "当前平台无法逐级锚定模型目录句柄，已拒绝读取",
    ))
}

fn validate_directory_metadata(metadata: &fs::Metadata) -> Result<(), ResourceResolutionError> {
    if metadata_is_link_or_reparse_point(metadata) || !metadata.is_dir() {
        Err(resource_path_changed_error())
    } else {
        Ok(())
    }
}

pub(super) fn validate_resource_metadata(
    metadata: &fs::Metadata,
    maximum_bytes: u64,
) -> Result<(), ResourceResolutionError> {
    if metadata_is_link_or_reparse_point(metadata) {
        return Err(ResourceResolutionError::new(
            ModelDiagnosticCategory::InvalidReference,
            "最终路径分量不能是符号链接或重解析点",
        ));
    }
    if !metadata.file_type().is_file() {
        return Err(ResourceResolutionError::new(
            ModelDiagnosticCategory::NotFile,
            "引用目标不是普通文件",
        ));
    }
    if metadata.len() > maximum_bytes {
        return Err(ResourceResolutionError::new(
            ModelDiagnosticCategory::TooLarge,
            format!(
                "资源大小为 {} 字节，超过上限 {maximum_bytes} 字节",
                metadata.len()
            ),
        ));
    }
    Ok(())
}

#[cfg(target_os = "windows")]
fn metadata_is_link_or_reparse_point(metadata: &fs::Metadata) -> bool {
    use std::os::windows::fs::MetadataExt as _;

    const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x0000_0400;
    metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
}

#[cfg(not(target_os = "windows"))]
fn metadata_is_link_or_reparse_point(metadata: &fs::Metadata) -> bool {
    metadata.file_type().is_symlink()
}

pub(super) fn resource_path_changed_error() -> ResourceResolutionError {
    ResourceResolutionError::new(
        ModelDiagnosticCategory::InvalidReference,
        "资源路径包含链接、非目录父分量或在打开期间发生变化，已拒绝读取",
    )
}
