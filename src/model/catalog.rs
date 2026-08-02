//! 扫描本地模型目录，并把同一模型的多个清单组织为服装变体。

use std::{
    collections::BTreeMap,
    error::Error,
    ffi::OsStr,
    fmt, fs, io,
    ops::Deref,
    path::{Path, PathBuf},
    sync::Arc,
};

const MODEL_FILE_SUFFIX: &str = ".model3.json";
pub(in crate::model) const MAX_DISCOVERY_DEPTH: usize = 16;
pub(in crate::model) const MAX_DISCOVERY_ENTRIES: usize = 4_096;
pub(in crate::model) const MAX_PENDING_DIRECTORIES: usize = 256;
pub(in crate::model) const MAX_DISCOVERED_MANIFESTS: usize = 1_024;
const DISCOVERY_BUDGET_WARNING: &str = "模型目录扫描达到资源预算，部分目录或模型清单未处理";

/// 确保模型根目录存在。
pub(crate) fn ensure_model_directory(root: &Path) -> io::Result<()> {
    fs::create_dir_all(root)
}

/// 表示一个可加载的 Live2D 模型清单；同一模型下的清单作为服装变体展示。
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ModelVariant {
    display_name: String,
    relative_path: PathBuf,
}

impl ModelVariant {
    /// 返回用于服装选择器展示的名称。
    pub(crate) fn display_name(&self) -> &str {
        &self.display_name
    }

    /// 返回相对于 `models/` 根目录的模型清单路径。
    pub(crate) fn relative_path(&self) -> &Path {
        &self.relative_path
    }
}

/// 表示模型目录中的一个模型及其全部服装变体。
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ModelFamily {
    display_name: String,
    variants: Vec<ModelVariant>,
}

/// 把可加载清单绑定到发现它的模型根目录快照。
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ModelManifest {
    path: PathBuf,
    relative_path: PathBuf,
    root: Arc<ScannedModelRoot>,
}

impl ModelManifest {
    fn new(root: Arc<ScannedModelRoot>, relative_path: &Path) -> Self {
        Self {
            path: root.configured_path.join(relative_path),
            relative_path: relative_path.to_path_buf(),
            root,
        }
    }

    /// 返回用于诊断和模型名称展示的清单路径。
    pub(crate) fn path(&self) -> &Path {
        &self.path
    }

    #[cfg(test)]
    pub(in crate::model) fn for_path_for_test(path: &Path) -> io::Result<Self> {
        let parent = path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new("."));
        let relative_path = path
            .file_name()
            .map(PathBuf::from)
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "模型清单路径缺少文件名"))?;
        Ok(Self {
            path: path.to_path_buf(),
            relative_path,
            root: Arc::new(ScannedModelRoot::capture(parent)?),
        })
    }

    /// 返回扫描根目录内经过发现流程验证的相对清单路径。
    pub(in crate::model) fn relative_path(&self) -> &Path {
        &self.relative_path
    }

    pub(in crate::model) fn scanned_root(&self) -> &ScannedModelRoot {
        &self.root
    }
}

impl AsRef<Path> for ModelManifest {
    fn as_ref(&self) -> &Path {
        self.path()
    }
}

impl Deref for ModelManifest {
    type Target = Path;

    fn deref(&self) -> &Self::Target {
        self.path()
    }
}

impl PartialEq<PathBuf> for ModelManifest {
    fn eq(&self, other: &PathBuf) -> bool {
        self.path == *other
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(in crate::model) struct ScannedModelRoot {
    configured_path: PathBuf,
    canonical_path: PathBuf,
    identity: DirectoryIdentity,
}

impl ScannedModelRoot {
    pub(in crate::model) fn capture(configured_path: &Path) -> io::Result<Self> {
        let canonical_path = fs::canonicalize(configured_path)?;
        let metadata = fs::metadata(&canonical_path)?;
        if !metadata.is_dir() {
            return Err(io::Error::new(
                io::ErrorKind::NotADirectory,
                "模型根路径不是目录",
            ));
        }
        let identity = DirectoryIdentity::capture(&canonical_path, &metadata)?;
        Ok(Self {
            configured_path: configured_path.to_path_buf(),
            canonical_path,
            identity,
        })
    }

    pub(in crate::model) fn canonical_path(&self) -> &Path {
        &self.canonical_path
    }

    pub(in crate::model) fn is_current(&self) -> bool {
        let Ok(canonical_path) = fs::canonicalize(&self.configured_path) else {
            return false;
        };
        if canonical_path != self.canonical_path {
            return false;
        }
        fs::metadata(&canonical_path).is_ok_and(|metadata| {
            metadata.is_dir()
                && DirectoryIdentity::capture(&canonical_path, &metadata)
                    .is_ok_and(|identity| identity == self.identity)
        })
    }

    /// 比较已打开目录句柄的元数据与扫描时保存的稳定身份。
    pub(in crate::model) fn matches_open_directory(
        &self,
        directory: &fs::File,
        metadata: &fs::Metadata,
    ) -> bool {
        metadata.is_dir()
            && DirectoryIdentity::from_open_directory(directory, metadata)
                .is_ok_and(|identity| identity == self.identity)
    }
}

#[cfg(unix)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct DirectoryIdentity {
    device: u64,
    inode: u64,
}

#[cfg(unix)]
impl DirectoryIdentity {
    fn capture(_path: &Path, metadata: &fs::Metadata) -> io::Result<Self> {
        Self::from_metadata(metadata)
    }

    fn from_open_directory(_directory: &fs::File, metadata: &fs::Metadata) -> io::Result<Self> {
        Self::from_metadata(metadata)
    }

    fn from_metadata(metadata: &fs::Metadata) -> io::Result<Self> {
        use std::os::unix::fs::MetadataExt as _;

        Ok(Self {
            device: metadata.dev(),
            inode: metadata.ino(),
        })
    }
}

#[cfg(target_os = "windows")]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct DirectoryIdentity {
    volume: u32,
    file_index: u64,
}

#[cfg(target_os = "windows")]
impl DirectoryIdentity {
    fn capture(path: &Path, _metadata: &fs::Metadata) -> io::Result<Self> {
        use std::os::windows::fs::{MetadataExt as _, OpenOptionsExt as _};

        const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x0000_0400;
        const FILE_SHARE_READ: u32 = 0x0000_0001;
        const FILE_SHARE_WRITE: u32 = 0x0000_0002;
        const FILE_SHARE_DELETE: u32 = 0x0000_0004;
        const FILE_FLAG_BACKUP_SEMANTICS: u32 = 0x0200_0000;
        const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;

        let mut options = fs::OpenOptions::new();
        options
            .read(true)
            .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE)
            .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT | FILE_FLAG_BACKUP_SEMANTICS);
        let directory = options.open(path)?;
        let metadata = directory.metadata()?;
        if !metadata.is_dir() || metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
            return Err(io::Error::other(
                "Windows 模型根目录句柄不是普通目录，已拒绝扫描",
            ));
        }
        Self::from_open_directory(&directory, &metadata)
    }

    fn from_open_directory(directory: &fs::File, _metadata: &fs::Metadata) -> io::Result<Self> {
        use std::os::windows::io::AsRawHandle as _;
        use windows::Win32::{
            Foundation::HANDLE,
            Storage::FileSystem::{BY_HANDLE_FILE_INFORMATION, GetFileInformationByHandle},
        };

        let mut information = BY_HANDLE_FILE_INFORMATION::default();
        // SAFETY: `File` 保证原始句柄在调用期间有效，输出地址独占且指向完整可写结构。
        unsafe { GetFileInformationByHandle(HANDLE(directory.as_raw_handle()), &mut information) }
            .map_err(|_| {
                io::Error::other("无法取得 Windows 模型根目录卷号与文件索引，已拒绝读取")
            })?;
        let file_index =
            (u64::from(information.nFileIndexHigh) << 32) | u64::from(information.nFileIndexLow);
        Ok(Self {
            volume: information.dwVolumeSerialNumber,
            file_index,
        })
    }
}

#[cfg(not(any(unix, target_os = "windows")))]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct DirectoryIdentity;

#[cfg(not(any(unix, target_os = "windows")))]
impl DirectoryIdentity {
    fn capture(_path: &Path, _metadata: &fs::Metadata) -> io::Result<Self> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "当前平台无法验证模型根目录身份，已拒绝扫描",
        ))
    }

    fn from_open_directory(_directory: &fs::File, _metadata: &fs::Metadata) -> io::Result<Self> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "当前平台无法验证模型根目录身份，已拒绝扫描",
        ))
    }
}

impl ModelFamily {
    /// 返回模型列表中展示的稳定名称。
    pub(crate) fn display_name(&self) -> &str {
        &self.display_name
    }

    /// 返回该模型可切换的全部清单变体。
    pub(crate) fn variants(&self) -> &[ModelVariant] {
        &self.variants
    }

    /// 返回模型清单提供的服装变体数量。
    pub(crate) fn outfit_count(&self) -> usize {
        self.variants.len()
    }

    /// 返回当前家族是否包含指定相对清单路径。
    pub(crate) fn contains(&self, relative_path: &Path) -> bool {
        self.variants
            .iter()
            .any(|variant| variant.relative_path == relative_path)
    }
}

/// 保存模型目录、已发现模型家族和当前选择。
#[derive(Clone, Debug)]
pub(crate) struct ModelCatalog {
    root: PathBuf,
    scanned_root: Option<Arc<ScannedModelRoot>>,
    families: Vec<ModelFamily>,
    selected: Option<PathBuf>,
    warning: Option<String>,
}

impl ModelCatalog {
    /// 扫描模型目录，并从统一配置恢复选择。
    ///
    /// 不存在的模型目录会被视为空目录，而不是启动错误。只有一个模型家族时会自动选择
    /// 已配置服装或第一个可用服装。
    ///
    /// # Errors
    ///
    /// 模型根目录存在但无法读取时返回错误；子目录错误只产生可恢复警告。
    pub(crate) fn load(
        root: PathBuf,
        configured_selection: Option<&Path>,
    ) -> Result<Self, ModelCatalogError> {
        let discovery = discover_models(&root)?;
        let selected = choose_selection(&discovery.families, configured_selection);
        Ok(Self {
            root,
            scanned_root: discovery.scanned_root,
            families: discovery.families,
            selected,
            warning: discovery.warning,
        })
    }

    /// 创建保留目录位置的空目录，用于向界面呈现根目录扫描错误。
    pub(crate) fn empty(root: PathBuf) -> Self {
        Self {
            root,
            scanned_root: None,
            families: Vec::new(),
            selected: None,
            warning: None,
        }
    }

    /// 返回已发现的模型家族。
    pub(crate) fn families(&self) -> &[ModelFamily] {
        &self.families
    }

    /// 返回模型家族与服装变体总数。
    pub(crate) fn counts(&self) -> (usize, usize) {
        (
            self.families.len(),
            self.families.iter().map(ModelFamily::outfit_count).sum(),
        )
    }

    /// 返回用于扫描模型的根目录。
    pub(crate) fn root(&self) -> &Path {
        &self.root
    }

    /// 返回不影响已发现模型使用的扫描诊断。
    pub(crate) fn warning(&self) -> Option<&str> {
        self.warning.as_deref()
    }

    /// 返回当前选中的模型家族。
    pub(crate) fn selected_family(&self) -> Option<&ModelFamily> {
        let selected = self.selected.as_deref()?;
        self.families
            .iter()
            .find(|family| family.contains(selected))
    }

    /// 返回当前选择模型清单的相对路径。
    pub(crate) fn selected_relative_path(&self) -> Option<&Path> {
        self.selected.as_deref()
    }

    /// 只解析本次扫描确认存在的模型清单，避免调用方自行拼接不可信路径。
    pub(crate) fn model_path(&self, relative_path: &Path) -> Option<ModelManifest> {
        self.families
            .iter()
            .any(|family| family.contains(relative_path))
            .then(|| {
                self.scanned_root
                    .as_ref()
                    .map(|root| ModelManifest::new(Arc::clone(root), relative_path))
            })
            .flatten()
    }

    /// 更新当前运行时模型但不持久化全局选择，供人格绑定切换复用目录校验。
    pub(crate) fn set_runtime_selection(
        &mut self,
        relative_path: Option<&Path>,
    ) -> Result<Option<ModelManifest>, ModelCatalogError> {
        let Some(relative_path) = relative_path else {
            self.selected = None;
            return Ok(None);
        };
        let path = self.select_variant(relative_path)?;
        Ok(Some(path))
    }

    /// 选择指定服装变体，并返回其绝对模型清单路径。
    ///
    /// # Errors
    ///
    /// 请求路径不属于当前扫描结果时返回错误。
    pub(crate) fn select_variant(
        &mut self,
        relative_path: &Path,
    ) -> Result<ModelManifest, ModelCatalogError> {
        if !self
            .families
            .iter()
            .any(|family| family.contains(relative_path))
        {
            return Err(ModelCatalogError::message(format!(
                "模型不在当前目录扫描结果中：{}",
                relative_path.display()
            )));
        }

        self.selected = Some(relative_path.to_path_buf());
        let Some(root) = &self.scanned_root else {
            return Err(ModelCatalogError::message("模型目录没有有效的扫描根快照"));
        };
        Ok(ModelManifest::new(Arc::clone(root), relative_path))
    }
}

struct ModelDiscovery {
    families: Vec<ModelFamily>,
    warning: Option<String>,
    scanned_root: Option<Arc<ScannedModelRoot>>,
}

fn discover_models(root: &Path) -> Result<ModelDiscovery, ModelCatalogError> {
    let scanned_root = match ScannedModelRoot::capture(root) {
        Ok(root) => Arc::new(root),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            return Ok(ModelDiscovery {
                families: Vec::new(),
                warning: None,
                scanned_root: None,
            });
        }
        Err(source) => {
            return Err(ModelCatalogError::io(
                format!("无法扫描模型目录 {}", root.display()),
                source,
            ));
        }
    };
    let mut grouped = BTreeMap::<String, Vec<ModelVariant>>::new();
    let mut warning = None;
    let mut directories = vec![(scanned_root.canonical_path.clone(), 0_usize)];
    let mut scanned_entries = 0_usize;
    let mut discovered_manifests = 0_usize;
    let mut directory_expansion_stopped = false;
    let mut scan_truncated = false;

    'discovery: while let Some((directory, depth)) = directories.pop() {
        let canonical_directory = match fs::canonicalize(&directory) {
            Ok(path) if path.starts_with(scanned_root.canonical_path()) => path,
            Ok(_) => {
                append_warning(
                    &mut warning,
                    format!("跳过模型根目录外的子目录 {}", directory.display()),
                );
                continue;
            }
            Err(source) => {
                if directory == scanned_root.canonical_path {
                    return Err(ModelCatalogError::io(
                        format!("无法扫描模型目录 {}", root.display()),
                        source,
                    ));
                }
                append_warning(
                    &mut warning,
                    format!("跳过无法扫描的模型子目录 {}：{source}", directory.display()),
                );
                continue;
            }
        };
        let entries = match fs::read_dir(&canonical_directory) {
            Ok(entries) => entries,
            Err(source) => {
                if canonical_directory == scanned_root.canonical_path {
                    return Err(ModelCatalogError::io(
                        format!("无法扫描模型目录 {}", root.display()),
                        source,
                    ));
                }
                append_warning(
                    &mut warning,
                    format!(
                        "跳过无法扫描的模型子目录 {}：{source}",
                        canonical_directory.display()
                    ),
                );
                continue;
            }
        };

        for entry in entries {
            if scanned_entries >= MAX_DISCOVERY_ENTRIES {
                scan_truncated = true;
                break 'discovery;
            }
            scanned_entries += 1;

            let entry = match entry {
                Ok(entry) => entry,
                Err(source) => {
                    append_warning(
                        &mut warning,
                        format!("跳过无法读取的模型目录项 {}：{source}", directory.display()),
                    );
                    continue;
                }
            };
            let path = entry.path();
            let file_type = match entry.file_type() {
                Ok(file_type) => file_type,
                Err(source) => {
                    append_warning(
                        &mut warning,
                        format!("跳过无法识别的模型目录项 {}：{source}", path.display()),
                    );
                    continue;
                }
            };

            if file_type.is_dir() {
                if depth < MAX_DISCOVERY_DEPTH {
                    // 队列一旦触顶便永久停止扩张，避免后续出队后又恢复累积扫描工作。
                    if directory_expansion_stopped || directories.len() >= MAX_PENDING_DIRECTORIES {
                        directory_expansion_stopped = true;
                        scan_truncated = true;
                    } else {
                        directories.push((path, depth + 1));
                    }
                } else {
                    append_warning(
                        &mut warning,
                        format!(
                            "跳过超过最大扫描深度 {MAX_DISCOVERY_DEPTH} 的目录 {}",
                            path.display()
                        ),
                    );
                }
                continue;
            }
            if !file_type.is_file() || !is_model_manifest(&path) {
                continue;
            }
            if discovered_manifests >= MAX_DISCOVERED_MANIFESTS {
                scan_truncated = true;
                break 'discovery;
            }
            discovered_manifests += 1;

            let canonical_manifest = match fs::canonicalize(&path) {
                Ok(path) if path.starts_with(scanned_root.canonical_path()) => path,
                Ok(_) => {
                    append_warning(
                        &mut warning,
                        format!("跳过模型根目录外清单 {}", path.display()),
                    );
                    continue;
                }
                Err(source) => {
                    append_warning(
                        &mut warning,
                        format!("跳过无法验证的模型清单 {}：{source}", path.display()),
                    );
                    continue;
                }
            };
            let relative_path = match canonical_manifest.strip_prefix(scanned_root.canonical_path())
            {
                Ok(relative_path) => relative_path,
                Err(error) => {
                    append_warning(
                        &mut warning,
                        format!("跳过模型目录外路径 {}：{error}", path.display()),
                    );
                    continue;
                }
            };
            if relative_path.to_str().is_none() {
                append_warning(
                    &mut warning,
                    format!("跳过非 UTF-8 模型路径 {}", path.display()),
                );
                continue;
            }

            let family_name = model_family_name(relative_path);
            let variant_name = variant_display_name(&path, &family_name);
            grouped
                .entry(family_name.clone())
                .or_default()
                .push(ModelVariant {
                    display_name: variant_name,
                    relative_path: relative_path.to_path_buf(),
                });
        }
    }

    if scan_truncated {
        append_warning(&mut warning, DISCOVERY_BUDGET_WARNING.to_owned());
    }

    if !scanned_root.is_current() {
        return Err(ModelCatalogError::message(
            "模型根目录在扫描期间发生变化，请重新扫描",
        ));
    }

    let families = grouped
        .into_iter()
        .map(|(display_name, mut variants)| {
            variants.sort_by(|left, right| left.relative_path.cmp(&right.relative_path));
            ModelFamily {
                display_name,
                variants,
            }
        })
        .collect();
    Ok(ModelDiscovery {
        families,
        warning,
        scanned_root: Some(scanned_root),
    })
}

fn is_model_manifest(path: &Path) -> bool {
    path.file_name()
        .and_then(OsStr::to_str)
        .is_some_and(|name| name.ends_with(MODEL_FILE_SUFFIX))
}

fn model_family_name(relative_path: &Path) -> String {
    let mut components = relative_path.components();
    if let (Some(first), Some(_)) = (components.next(), components.next()) {
        return first.as_os_str().to_string_lossy().into_owned();
    }
    model_manifest_stem(relative_path).to_owned()
}

fn variant_display_name(path: &Path, family_name: &str) -> String {
    let stem = model_manifest_stem(path);
    let shortened = stem
        .strip_prefix(family_name)
        .map(|suffix| suffix.trim_start_matches(['_', '-', ' ']))
        .filter(|suffix| !suffix.is_empty());
    shortened.unwrap_or(stem).to_owned()
}

fn model_manifest_stem(path: &Path) -> &str {
    path.file_name()
        .and_then(OsStr::to_str)
        .and_then(|name| name.strip_suffix(MODEL_FILE_SUFFIX))
        .filter(|name| !name.is_empty())
        .unwrap_or("未命名模型")
}

fn choose_selection(
    families: &[ModelFamily],
    configured_selection: Option<&Path>,
) -> Option<PathBuf> {
    if let Some(configured_selection) = configured_selection
        && families
            .iter()
            .any(|family| family.contains(configured_selection))
    {
        return Some(configured_selection.to_path_buf());
    }

    let [family] = families else {
        return None;
    };
    family
        .variants
        .first()
        .map(|variant| variant.relative_path.clone())
}

fn append_warning(warning: &mut Option<String>, message: String) {
    match warning {
        Some(warning) => {
            warning.push('；');
            warning.push_str(&message);
        }
        None => *warning = Some(message),
    }
}

/// 描述模型目录扫描或选择阶段的错误。
#[derive(Debug)]
pub(crate) struct ModelCatalogError {
    message: String,
    source: Option<io::Error>,
}

impl ModelCatalogError {
    fn message(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            source: None,
        }
    }

    fn io(message: impl Into<String>, source: io::Error) -> Self {
        Self {
            message: message.into(),
            source: Some(source),
        }
    }
}

impl fmt::Display for ModelCatalogError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)?;
        if let Some(source) = &self.source {
            write!(formatter, "：{source}")?;
        }
        Ok(())
    }
}

impl Error for ModelCatalogError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        self.source
            .as_ref()
            .map(|source| source as &(dyn Error + 'static))
    }
}
