//! 将模型清单中的相对引用限制在模型目录内，并执行单文件读取预算。

mod budget;
mod opened_file;

use std::{
    error::Error,
    fmt, fs,
    io::ErrorKind,
    path::{Component, Path, PathBuf},
};

#[cfg(test)]
use std::sync::Arc;

use super::ModelDiagnosticCategory;
use crate::model::catalog::{ModelManifest, ScannedModelRoot};

#[cfg(test)]
type BeforeOpenHook = Arc<dyn Fn(&Path) + Send + Sync>;

pub(crate) use budget::AuxiliaryResourceBudget;

/// 动作、表情、Physics、Pose 等 JSON 辅助资源的单文件大小上限。
pub(crate) const MAX_AUXILIARY_RESOURCE_BYTES: u64 = 8 * 1024 * 1024;
/// 单个模型 generation 的全部动作和表情允许读取的累计字节数。
pub(crate) const MAX_AUXILIARY_GENERATION_BYTES: u64 = 64 * 1024 * 1024;
/// 单个模型目录最多发现的外部动作数量。
pub(crate) const MAX_EXTERNAL_MOTION_COUNT: usize = 256;
/// 单个模型目录最多发现的外部表情数量。
pub(crate) const MAX_EXTERNAL_EXPRESSION_COUNT: usize = 128;
const MAX_EXTERNAL_RESOURCE_SCAN_ENTRIES_PER_DIRECTORY: usize = 4_096;
const MOTION_DIRECTORY: &str = "motions";
const EXPRESSION_DIRECTORY: &str = "expressions";

/// 未写入模型清单、但位于允许扫描位置的外部动作资源。
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ExternalMotionReference {
    name: String,
    reference: String,
    canonical_path: PathBuf,
}

impl ExternalMotionReference {
    /// 返回不含 `.motion3.json` 后缀的默认显示名。
    pub(crate) fn name(&self) -> &str {
        &self.name
    }

    /// 返回不会与清单动作组混淆的稳定运行时 ID。
    pub(crate) fn runtime_id(&self) -> String {
        external_runtime_id(&self.reference)
    }

    /// 返回相对于模型清单目录的安全候选引用。
    pub(crate) fn reference(&self) -> &str {
        &self.reference
    }

    /// 返回仅用于本 generation 去重的规范标签路径，不用于重新打开资源。
    pub(crate) fn canonical_path(&self) -> &Path {
        &self.canonical_path
    }
}

/// 未写入模型清单、但位于允许扫描位置的外部表情资源。
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ExternalExpressionReference {
    name: String,
    reference: String,
    movable_to_outfit: bool,
    canonical_path: PathBuf,
}

impl ExternalExpressionReference {
    /// 返回不含 `.exp3.json` 后缀的表达式名称。
    pub(crate) fn name(&self) -> &str {
        &self.name
    }

    /// 返回不会与清单表情名称混淆的稳定运行时 ID。
    pub(crate) fn runtime_id(&self) -> String {
        external_runtime_id(&self.reference)
    }

    /// 返回相对于模型清单目录的安全候选引用。
    pub(crate) fn reference(&self) -> &str {
        &self.reference
    }

    /// 根目录表达式可由用户分类为服装，专属目录表达式固定为普通表情。
    pub(crate) fn movable_to_outfit(&self) -> bool {
        self.movable_to_outfit
    }

    /// 返回仅用于本 generation 去重的规范标签路径，不用于重新打开资源。
    pub(crate) fn canonical_path(&self) -> &Path {
        &self.canonical_path
    }
}

fn external_runtime_id(reference: &str) -> String {
    format!("external:{reference}")
}

fn normalize_relative_path(path: &Path) -> Result<PathBuf, ResourceResolutionError> {
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::Normal(component) => normalized.push(component),
            Component::CurDir => {}
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                return Err(ResourceResolutionError::new(
                    ModelDiagnosticCategory::InvalidReference,
                    "引用必须是模型目录内的相对路径",
                ));
            }
        }
    }
    if normalized.as_os_str().is_empty() {
        return Err(ResourceResolutionError::new(
            ModelDiagnosticCategory::InvalidReference,
            "引用路径为空",
        ));
    }
    Ok(normalized)
}

/// 在必需资源预检后安全解析模型目录内引用。
#[derive(Clone)]
pub(crate) struct ModelResourceResolver {
    canonical_model_dir: PathBuf,
    model_relative_dir: PathBuf,
    scanned_root: ScannedModelRoot,
    #[cfg(test)]
    before_open_for_test: Option<BeforeOpenHook>,
}

impl fmt::Debug for ModelResourceResolver {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ModelResourceResolver")
            .field("canonical_model_dir", &self.canonical_model_dir)
            .field("model_relative_dir", &self.model_relative_dir)
            .field("scanned_root", &self.scanned_root)
            .finish_non_exhaustive()
    }
}

impl ModelResourceResolver {
    /// 根据模型清单路径建立解析边界，不读取清单或任何可选资源。
    ///
    /// # Errors
    ///
    /// 清单缺少可解析的父目录，或模型目录无法访问时返回错误。
    #[cfg(test)]
    pub(crate) fn for_manifest(manifest_path: &Path) -> Result<Self, ResourceResolutionError> {
        let model_dir = manifest_path
            .parent()
            .filter(|path| !path.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new("."));
        let scanned_root = ScannedModelRoot::capture(model_dir)
            .map_err(|error| ResourceResolutionError::from_io(model_dir, &error))?;
        let canonical_model_dir = scanned_root.canonical_path().to_path_buf();
        Ok(Self {
            canonical_model_dir,
            model_relative_dir: PathBuf::new(),
            scanned_root,
            before_open_for_test: None,
        })
    }

    fn for_scanned_manifest(manifest: &ModelManifest) -> Result<Self, ResourceResolutionError> {
        let relative_manifest = normalize_relative_path(manifest.relative_path())?;
        let model_relative_dir = relative_manifest
            .parent()
            .unwrap_or_else(|| Path::new(""))
            .to_path_buf();
        let scanned_root = manifest.scanned_root().clone();
        let canonical_model_dir = scanned_root.canonical_path().join(&model_relative_dir);
        Ok(Self {
            canonical_model_dir,
            model_relative_dir,
            scanned_root,
            #[cfg(test)]
            before_open_for_test: None,
        })
    }

    /// 返回建立路径边界时解析出的规范模型目录。
    pub(crate) fn model_dir(&self) -> &Path {
        &self.canonical_model_dir
    }

    /// 建立模型目录边界，并返回已打开且完成身份校验的清单句柄。
    pub(in crate::model) fn open_manifest(
        manifest: &ModelManifest,
        maximum_bytes: u64,
    ) -> Result<(Self, fs::File), ResourceResolutionError> {
        let mut before_open = |_: &Path| {};
        Self::open_manifest_with_hook(manifest, maximum_bytes, &mut before_open)
    }

    /// 在最终清单路径打开前运行确定性测试替换。
    #[cfg(all(test, unix))]
    pub(in crate::model) fn open_manifest_with_open_hook_for_test(
        manifest: &ModelManifest,
        maximum_bytes: u64,
        before_open: &mut dyn FnMut(&Path),
    ) -> Result<(Self, fs::File), ResourceResolutionError> {
        Self::open_manifest_with_hook(manifest, maximum_bytes, before_open)
    }

    fn open_manifest_with_hook(
        manifest: &ModelManifest,
        maximum_bytes: u64,
        before_open: &mut dyn FnMut(&Path),
    ) -> Result<(Self, fs::File), ResourceResolutionError> {
        let resolver = Self::for_scanned_manifest(manifest)?;
        let relative_manifest = normalize_relative_path(manifest.relative_path())?;
        let mut after_open = |_: &Path| {};
        let (_, file) = resolver.open_root_relative_file(
            &relative_manifest,
            manifest.path(),
            maximum_bytes,
            before_open,
            &mut after_open,
        )?;
        Ok((resolver, file))
    }

    /// 在最终清单句柄打开后运行恢复操作，确定性验证 swap-and-restore 竞态。
    #[cfg(all(test, unix))]
    pub(in crate::model) fn open_manifest_with_open_hooks_for_test(
        manifest: &ModelManifest,
        maximum_bytes: u64,
        before_open: &mut dyn FnMut(&Path),
        after_open: &mut dyn FnMut(&Path),
    ) -> Result<(Self, fs::File), ResourceResolutionError> {
        let resolver = Self::for_scanned_manifest(manifest)?;
        let relative_manifest = normalize_relative_path(manifest.relative_path())?;
        let (_, file) = resolver.open_root_relative_file(
            &relative_manifest,
            manifest.path(),
            maximum_bytes,
            before_open,
            after_open,
        )?;
        Ok((resolver, file))
    }

    #[cfg(test)]
    pub(in crate::model) fn open_unscanned_manifest_for_test(
        manifest_path: &Path,
        maximum_bytes: u64,
    ) -> Result<(Self, fs::File), ResourceResolutionError> {
        let resolver = Self::for_manifest(manifest_path)?;
        let relative_manifest = manifest_path
            .file_name()
            .map(PathBuf::from)
            .ok_or_else(|| {
                ResourceResolutionError::new(
                    ModelDiagnosticCategory::InvalidReference,
                    "模型清单路径缺少文件名",
                )
            })?;
        let mut before_open = |_: &Path| {};
        let mut after_open = |_: &Path| {};
        let (_, file) = resolver.open_root_relative_file(
            &relative_manifest,
            manifest_path,
            maximum_bytes,
            &mut before_open,
            &mut after_open,
        )?;
        Ok((resolver, file))
    }

    #[cfg(test)]
    pub(in crate::model) fn with_open_hook_for_test(
        mut self,
        before_open: impl Fn(&Path) + Send + Sync + 'static,
    ) -> Self {
        self.before_open_for_test = Some(Arc::new(before_open));
        self
    }

    /// 返回从扫描根目录句柄逐级打开且完成类型、大小校验的资源句柄。
    pub(in crate::model) fn open_file(
        &self,
        reference: &str,
        maximum_bytes: u64,
    ) -> Result<fs::File, ResourceResolutionError> {
        let mut before_open = |_: &Path| {};
        self.open_resolved_file(reference, maximum_bytes, &mut before_open)
            .map(|(_, file)| file)
    }

    /// 在最终资源路径打开前运行确定性测试替换。
    #[cfg(all(test, unix))]
    pub(in crate::model) fn open_file_with_open_hook_for_test(
        &self,
        reference: &str,
        maximum_bytes: u64,
        before_open: &mut dyn FnMut(&Path),
    ) -> Result<fs::File, ResourceResolutionError> {
        self.open_resolved_file(reference, maximum_bytes, before_open)
            .map(|(_, file)| file)
    }

    /// 解析并校验一个模型目录内的普通文件。
    ///
    /// # Errors
    ///
    /// 引用为空、包含越界路径或任意链接分量、目标缺失、不是普通文件，或大小超限时返回错误。
    pub(crate) fn resolve_file(
        &self,
        reference: &str,
        maximum_bytes: u64,
    ) -> Result<PathBuf, ResourceResolutionError> {
        let mut before_open = |_: &Path| {};
        self.open_resolved_file(reference, maximum_bytes, &mut before_open)
            .map(|(path, _)| path)
    }

    fn open_resolved_file(
        &self,
        reference: &str,
        maximum_bytes: u64,
        before_open: &mut dyn FnMut(&Path),
    ) -> Result<(PathBuf, fs::File), ResourceResolutionError> {
        let model_relative_path = normalize_relative_path(Path::new(reference))?;
        let root_relative_path = self.model_relative_dir.join(&model_relative_path);
        let display_path = self.canonical_model_dir.join(model_relative_path);
        let mut after_open = |_: &Path| {};
        self.open_root_relative_file(
            &root_relative_path,
            &display_path,
            maximum_bytes,
            before_open,
            &mut after_open,
        )
    }

    fn open_root_relative_file(
        &self,
        relative_path: &Path,
        display_path: &Path,
        maximum_bytes: u64,
        before_open: &mut dyn FnMut(&Path),
        after_open: &mut dyn FnMut(&Path),
    ) -> Result<(PathBuf, fs::File), ResourceResolutionError> {
        let mut invoke_before_open = |path: &Path| {
            before_open(path);
            #[cfg(test)]
            if let Some(test_hook) = &self.before_open_for_test {
                test_hook(path);
            }
        };
        let file = opened_file::open_anchored_file(
            &self.scanned_root,
            relative_path,
            display_path,
            maximum_bytes,
            &mut invoke_before_open,
            after_open,
        )?;
        Ok((display_path.to_path_buf(), file))
    }

    fn verify_scanned_root(&self) -> Result<(), ResourceResolutionError> {
        if !self.scanned_root.is_current() {
            Err(scanned_root_changed_error())
        } else {
            Ok(())
        }
    }

    /// 发现模型目录根层及 `motions/` 直属的外部 `.motion3.json` 文件。
    #[cfg(test)]
    pub(crate) fn discover_external_motions(&self) -> Vec<ExternalMotionReference> {
        self.discover_external_motions_with_checkpoint(|| false)
    }

    pub(crate) fn discover_external_motions_with_checkpoint(
        &self,
        mut checkpoint: impl FnMut() -> bool,
    ) -> Vec<ExternalMotionReference> {
        self.try_discover_external_motions_with_checkpoint(&mut checkpoint)
            .unwrap_or_default()
    }

    fn try_discover_external_motions_with_checkpoint(
        &self,
        checkpoint: &mut dyn FnMut() -> bool,
    ) -> Result<Vec<ExternalMotionReference>, ResourceResolutionError> {
        self.try_discover_external_resources(
            ".motion3.json",
            MOTION_DIRECTORY,
            MAX_EXTERNAL_MOTION_COUNT,
            checkpoint,
        )
        .map(|resources| {
            resources
                .into_iter()
                .map(|resource| ExternalMotionReference {
                    name: resource.name,
                    reference: resource.reference,
                    canonical_path: resource.canonical_path,
                })
                .collect()
        })
    }

    /// 发现模型目录根层及 `expressions/` 直属的外部 `.exp3.json` 文件。
    #[cfg(test)]
    pub(crate) fn discover_external_expressions(&self) -> Vec<ExternalExpressionReference> {
        self.discover_external_expressions_with_checkpoint(|| false)
    }

    pub(crate) fn discover_external_expressions_with_checkpoint(
        &self,
        mut checkpoint: impl FnMut() -> bool,
    ) -> Vec<ExternalExpressionReference> {
        self.try_discover_external_expressions_with_checkpoint(&mut checkpoint)
            .unwrap_or_default()
    }

    /// 发现外部表情，并保留无法读取模型目录时的诊断。
    ///
    /// # Errors
    ///
    /// 模型目录在解析器创建后变得不可读时返回错误。
    #[cfg(test)]
    pub(crate) fn try_discover_external_expressions(
        &self,
    ) -> Result<Vec<ExternalExpressionReference>, ResourceResolutionError> {
        self.try_discover_external_expressions_with_checkpoint(&mut || false)
    }

    fn try_discover_external_expressions_with_checkpoint(
        &self,
        checkpoint: &mut dyn FnMut() -> bool,
    ) -> Result<Vec<ExternalExpressionReference>, ResourceResolutionError> {
        self.try_discover_external_resources(
            ".exp3.json",
            EXPRESSION_DIRECTORY,
            MAX_EXTERNAL_EXPRESSION_COUNT,
            checkpoint,
        )
        .map(|resources| {
            resources
                .into_iter()
                .map(|resource| ExternalExpressionReference {
                    name: resource.name,
                    movable_to_outfit: resource.in_model_root,
                    reference: resource.reference,
                    canonical_path: resource.canonical_path,
                })
                .collect()
        })
    }

    fn try_discover_external_resources(
        &self,
        suffix: &str,
        dedicated_directory: &str,
        maximum_count: usize,
        checkpoint: &mut dyn FnMut() -> bool,
    ) -> Result<Vec<DiscoveredResource>, ResourceResolutionError> {
        let mut candidates = Vec::new();
        self.scan_external_directory(None, suffix, true, &mut candidates, checkpoint)?;
        if !checkpoint() {
            self.scan_external_directory(
                Some(dedicated_directory),
                suffix,
                false,
                &mut candidates,
                checkpoint,
            )?;
        }
        // 同一真实文件可通过目录内符号链接出现多次；根目录候选优先保留其可分类语义。
        candidates.sort_unstable_by(|left, right| {
            right
                .in_model_root
                .cmp(&left.in_model_root)
                .then_with(|| left.reference.cmp(&right.reference))
        });

        let mut canonical_files = std::collections::BTreeSet::new();
        let mut resources = Vec::with_capacity(candidates.len().min(maximum_count));
        for mut candidate in candidates {
            if checkpoint() {
                break;
            }
            let Ok(canonical_path) =
                self.resolve_file(&candidate.reference, MAX_AUXILIARY_RESOURCE_BYTES)
            else {
                continue;
            };
            if !canonical_files.insert(canonical_path.clone()) {
                continue;
            }
            candidate.canonical_path = canonical_path;
            resources.push(candidate);
            if resources.len() == maximum_count {
                break;
            }
        }
        resources.sort_unstable_by(|left, right| left.reference.cmp(&right.reference));
        Ok(resources)
    }

    fn scan_external_directory(
        &self,
        relative_directory: Option<&str>,
        suffix: &str,
        in_model_root: bool,
        candidates: &mut Vec<DiscoveredResource>,
        checkpoint: &mut dyn FnMut() -> bool,
    ) -> Result<(), ResourceResolutionError> {
        if checkpoint() {
            return Ok(());
        }
        self.verify_scanned_root()?;
        let directory = relative_directory
            .map(|relative| self.canonical_model_dir.join(relative))
            .unwrap_or_else(|| self.canonical_model_dir.clone());
        let canonical_directory = match fs::canonicalize(&directory) {
            Ok(directory) => directory,
            Err(error) if relative_directory.is_some() && error.kind() == ErrorKind::NotFound => {
                return Ok(());
            }
            Err(_) if relative_directory.is_some() => return Ok(()),
            Err(error) => return Err(ResourceResolutionError::from_io(&directory, &error)),
        };
        if !canonical_directory.starts_with(&self.canonical_model_dir)
            || !canonical_directory.is_dir()
            || (relative_directory.is_none() && canonical_directory != self.canonical_model_dir)
        {
            return if relative_directory.is_some() {
                Ok(())
            } else {
                Err(opened_file::resource_path_changed_error())
            };
        }
        let entries = match fs::read_dir(&canonical_directory) {
            Ok(entries) => entries,
            Err(_) if relative_directory.is_some() => return Ok(()),
            Err(error) => {
                return Err(ResourceResolutionError::from_io(
                    &canonical_directory,
                    &error,
                ));
            }
        };
        for entry in entries.take(MAX_EXTERNAL_RESOURCE_SCAN_ENTRIES_PER_DIRECTORY) {
            if checkpoint() {
                break;
            }
            let Ok(entry) = entry else {
                continue;
            };
            let Ok(file_name) = entry.file_name().into_string() else {
                continue;
            };
            let Some(name) = file_name
                .strip_suffix(suffix)
                .filter(|name| !name.is_empty())
                .map(str::to_owned)
            else {
                continue;
            };
            let reference = relative_directory
                .map(|directory| format!("{directory}/{file_name}"))
                .unwrap_or(file_name);
            candidates.push(DiscoveredResource {
                name,
                reference,
                in_model_root,
                canonical_path: PathBuf::new(),
            });
        }
        Ok(())
    }
}

#[derive(Clone, Debug)]
struct DiscoveredResource {
    name: String,
    reference: String,
    in_model_root: bool,
    canonical_path: PathBuf,
}

fn scanned_root_changed_error() -> ResourceResolutionError {
    ResourceResolutionError::new(
        ModelDiagnosticCategory::InvalidReference,
        "模型根目录在目录扫描后发生变化，已拒绝加载",
    )
}

/// 描述单个模型引用未通过安全解析的原因。
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ResourceResolutionError {
    category: ModelDiagnosticCategory,
    message: String,
}

impl ResourceResolutionError {
    fn new(category: ModelDiagnosticCategory, message: impl Into<String>) -> Self {
        Self {
            category,
            message: message.into(),
        }
    }

    fn from_io(path: &Path, error: &std::io::Error) -> Self {
        let (category, action) = match error.kind() {
            ErrorKind::NotFound => (ModelDiagnosticCategory::Missing, "路径不存在"),
            ErrorKind::PermissionDenied => (ModelDiagnosticCategory::Read, "没有读取权限"),
            _ => (ModelDiagnosticCategory::Read, "无法访问路径"),
        };
        Self::new(category, format!("{action}：{}", path.display()))
    }

    /// 返回可直接映射到模型加载诊断的类别。
    pub(crate) fn category(&self) -> ModelDiagnosticCategory {
        self.category
    }

    /// 返回不包含操作系统本地化文本的简体中文说明。
    pub(crate) fn message(&self) -> &str {
        &self.message
    }
}

impl fmt::Display for ResourceResolutionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}，{}", self.category(), self.message())
    }
}

impl Error for ResourceResolutionError {}
