//! 将模型清单中的相对引用限制在模型目录内，并执行单文件读取预算。

use std::{
    error::Error,
    fmt, fs,
    io::{ErrorKind, Read as _},
    path::{Component, Path, PathBuf},
};

use super::ModelDiagnosticCategory;

/// 动作、表情、Physics、Pose 等 JSON 辅助资源的单文件大小上限。
pub(crate) const MAX_AUXILIARY_RESOURCE_BYTES: u64 = 8 * 1024 * 1024;
/// 单个模型 generation 的全部动作和表情允许读取的累计字节数。
pub(crate) const MAX_AUXILIARY_GENERATION_BYTES: u64 = 64 * 1024 * 1024;
/// 单个模型目录最多发现的外部表情数量。
pub(crate) const MAX_EXTERNAL_EXPRESSION_COUNT: usize = 128;
const MAX_EXTERNAL_EXPRESSION_SCAN_ENTRIES: usize = 4_096;

/// 跟踪动作和表情跨控制器共享的 generation 读取预算。
#[derive(Debug)]
pub(crate) struct AuxiliaryResourceBudget {
    remaining_bytes: u64,
}

impl AuxiliaryResourceBudget {
    #[cfg(test)]
    pub(in crate::model) fn with_limit(maximum_bytes: u64) -> Self {
        Self {
            remaining_bytes: maximum_bytes,
        }
    }

    fn consume(&mut self, bytes: u64) -> Result<(), ResourceResolutionError> {
        self.remaining_bytes = self.remaining_bytes.checked_sub(bytes).ok_or_else(|| {
            ResourceResolutionError::new(
                ModelDiagnosticCategory::LimitExceeded,
                format!(
                    "动作与表情累计读取超过 generation 上限 {MAX_AUXILIARY_GENERATION_BYTES} 字节"
                ),
            )
        })?;
        Ok(())
    }
}

impl Default for AuxiliaryResourceBudget {
    fn default() -> Self {
        Self {
            remaining_bytes: MAX_AUXILIARY_GENERATION_BYTES,
        }
    }
}

/// 与模型清单同目录、但未必写入清单的外部表情资源。
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ExternalExpressionReference {
    name: String,
    reference: String,
}

impl ExternalExpressionReference {
    /// 返回不含 `.exp3.json` 后缀的表达式名称。
    pub(crate) fn name(&self) -> &str {
        &self.name
    }

    /// 返回相对于模型清单目录的安全候选引用。
    pub(crate) fn reference(&self) -> &str {
        &self.reference
    }
}

/// 在必需资源预检后安全解析模型目录内引用。
#[derive(Clone, Debug)]
pub(crate) struct ModelResourceResolver {
    canonical_model_dir: PathBuf,
}

impl ModelResourceResolver {
    /// 根据模型清单路径建立解析边界，不读取清单或任何可选资源。
    ///
    /// # Errors
    ///
    /// 清单缺少可解析的父目录，或模型目录无法访问时返回错误。
    pub(crate) fn for_manifest(manifest_path: &Path) -> Result<Self, ResourceResolutionError> {
        let model_dir = manifest_path
            .parent()
            .filter(|path| !path.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new("."));
        let canonical_model_dir = fs::canonicalize(model_dir)
            .map_err(|error| ResourceResolutionError::from_io(model_dir, &error))?;
        Ok(Self {
            canonical_model_dir,
        })
    }

    /// 解析并校验一个模型目录内的普通文件。
    ///
    /// # Errors
    ///
    /// 引用为空、包含越界路径、符号链接指向目录外、目标缺失、不是普通文件，或大小超限时返回错误。
    pub(crate) fn resolve_file(
        &self,
        reference: &str,
        maximum_bytes: u64,
    ) -> Result<PathBuf, ResourceResolutionError> {
        let relative_path = Path::new(reference);
        let mut has_normal_component = false;
        for component in relative_path.components() {
            match component {
                Component::Normal(_) => has_normal_component = true,
                Component::CurDir => {}
                Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                    return Err(ResourceResolutionError::new(
                        ModelDiagnosticCategory::InvalidReference,
                        "引用必须是模型目录内的相对路径",
                    ));
                }
            }
        }
        if !has_normal_component {
            return Err(ResourceResolutionError::new(
                ModelDiagnosticCategory::InvalidReference,
                "引用路径为空",
            ));
        }

        let joined = self.canonical_model_dir.join(relative_path);
        let canonical_path = fs::canonicalize(&joined)
            .map_err(|error| ResourceResolutionError::from_io(&joined, &error))?;
        if !canonical_path.starts_with(&self.canonical_model_dir) {
            return Err(ResourceResolutionError::new(
                ModelDiagnosticCategory::InvalidReference,
                "引用或符号链接越出模型目录",
            ));
        }

        let metadata = fs::metadata(&canonical_path)
            .map_err(|error| ResourceResolutionError::from_io(&canonical_path, &error))?;
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
                    "资源大小为 {} 字节，单文件上限为 {maximum_bytes} 字节",
                    metadata.len()
                ),
            ));
        }
        Ok(canonical_path)
    }

    /// 在完成路径与大小校验后读取 UTF-8 文本，并再次检查实际读取大小。
    ///
    /// # Errors
    ///
    /// 文件无法安全解析、读取失败、读取期间超过上限，或内容不是 UTF-8 时返回错误。
    #[cfg(test)]
    pub(in crate::model) fn read_text(
        &self,
        reference: &str,
        maximum_bytes: u64,
    ) -> Result<String, ResourceResolutionError> {
        let mut budget = AuxiliaryResourceBudget::with_limit(maximum_bytes);
        self.read_text_with_budget(reference, maximum_bytes, &mut budget)
    }

    /// 在单文件上限之外扣减当前 generation 的共享累计读取预算。
    ///
    /// # Errors
    ///
    /// 文件无法安全打开、读取超过单项或累计预算，或内容不是 UTF-8 时返回错误。
    #[cfg(test)]
    pub(in crate::model) fn read_text_with_budget(
        &self,
        reference: &str,
        maximum_bytes: u64,
        budget: &mut AuxiliaryResourceBudget,
    ) -> Result<String, ResourceResolutionError> {
        self.read_text_with_budget_and_checkpoint(reference, maximum_bytes, budget, || false)
    }

    /// 读取共享预算内的文本，并在分块读取之间检查调用方的取消状态。
    pub(crate) fn read_text_with_budget_and_checkpoint(
        &self,
        reference: &str,
        maximum_bytes: u64,
        budget: &mut AuxiliaryResourceBudget,
        mut checkpoint: impl FnMut() -> bool,
    ) -> Result<String, ResourceResolutionError> {
        let path = self.resolve_file(reference, maximum_bytes)?;
        let mut file = fs::File::open(&path)
            .map_err(|error| ResourceResolutionError::from_io(&path, &error))?;
        let metadata = file
            .metadata()
            .map_err(|error| ResourceResolutionError::from_io(&path, &error))?;
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
                    "资源大小为 {} 字节，单文件上限为 {maximum_bytes} 字节",
                    metadata.len()
                ),
            ));
        }
        if metadata.len() > budget.remaining_bytes {
            return Err(ResourceResolutionError::new(
                ModelDiagnosticCategory::LimitExceeded,
                format!(
                    "资源大小为 {} 字节，generation 剩余读取预算为 {} 字节",
                    metadata.len(),
                    budget.remaining_bytes
                ),
            ));
        }

        // 文件可能在元数据检查后增长，只读取两个上限中更小值再加一字节用于识别越界。
        let read_limit = maximum_bytes
            .min(budget.remaining_bytes)
            .checked_add(1)
            .ok_or_else(|| {
                ResourceResolutionError::new(
                    ModelDiagnosticCategory::TooLarge,
                    "资源读取上限发生整数溢出",
                )
            })?;
        let read_limit = usize::try_from(read_limit).map_err(|_| {
            ResourceResolutionError::new(
                ModelDiagnosticCategory::TooLarge,
                "资源读取上限无法表示为当前平台内存大小",
            )
        })?;
        let mut bytes = Vec::new();
        let initial_capacity = usize::try_from(metadata.len())
            .unwrap_or(read_limit)
            .min(read_limit);
        bytes.try_reserve_exact(initial_capacity).map_err(|error| {
            ResourceResolutionError::new(
                ModelDiagnosticCategory::Read,
                format!("无法分配资源读取缓冲：{error}"),
            )
        })?;
        let mut remaining = read_limit;
        while remaining > 0 {
            if checkpoint() {
                return Err(ResourceResolutionError::new(
                    ModelDiagnosticCategory::Read,
                    "资源读取已取消",
                ));
            }
            let chunk_size = remaining.min(64 * 1024);
            let start = bytes.len();
            bytes.try_reserve(chunk_size).map_err(|error| {
                ResourceResolutionError::new(
                    ModelDiagnosticCategory::Read,
                    format!("无法扩展资源读取缓冲：{error}"),
                )
            })?;
            bytes.resize(start + chunk_size, 0);
            let read = file
                .read(&mut bytes[start..start + chunk_size])
                .map_err(|error| ResourceResolutionError::from_io(&path, &error))?;
            bytes.truncate(start + read);
            remaining -= read;
            if read == 0 {
                break;
            }
        }
        if bytes.len() as u64 > maximum_bytes {
            return Err(ResourceResolutionError::new(
                ModelDiagnosticCategory::TooLarge,
                format!(
                    "实际读取大小为 {} 字节，单文件上限为 {maximum_bytes} 字节",
                    bytes.len()
                ),
            ));
        }
        if bytes.len() as u64 > budget.remaining_bytes {
            return Err(ResourceResolutionError::new(
                ModelDiagnosticCategory::LimitExceeded,
                format!(
                    "实际读取大小为 {} 字节，generation 剩余读取预算为 {} 字节",
                    bytes.len(),
                    budget.remaining_bytes
                ),
            ));
        }
        budget.consume(bytes.len() as u64)?;
        String::from_utf8(bytes).map_err(|_| {
            ResourceResolutionError::new(
                ModelDiagnosticCategory::Parse,
                "资源内容不是有效的 UTF-8 文本",
            )
        })
    }

    /// 发现模型目录直属的外部 `.exp3.json` 文件。
    ///
    /// VTube Studio 等工具允许热键引用未写入 `.model3.json` 的服装表达式。这里只返回
    /// 文件名候选，真正读取时仍会经过 [`Self::read_text_with_budget_and_checkpoint`] 的路径和大小校验。
    pub(crate) fn discover_external_expressions(&self) -> Vec<ExternalExpressionReference> {
        self.try_discover_external_expressions().unwrap_or_default()
    }

    /// 发现外部表情，并保留无法读取模型目录时的诊断。
    ///
    /// # Errors
    ///
    /// 模型目录在解析器创建后变得不可读时返回错误。
    pub(crate) fn try_discover_external_expressions(
        &self,
    ) -> Result<Vec<ExternalExpressionReference>, ResourceResolutionError> {
        let entries = fs::read_dir(&self.canonical_model_dir)
            .map_err(|error| ResourceResolutionError::from_io(&self.canonical_model_dir, &error))?;
        let mut candidates = entries
            .take(MAX_EXTERNAL_EXPRESSION_SCAN_ENTRIES)
            .filter_map(Result::ok)
            .filter_map(|entry| entry.file_name().into_string().ok())
            .filter(|reference| {
                reference
                    .strip_suffix(".exp3.json")
                    .is_some_and(|name| !name.is_empty())
            })
            .collect::<Vec<_>>();
        candidates.sort_unstable();

        let mut expressions =
            Vec::with_capacity(candidates.len().min(MAX_EXTERNAL_EXPRESSION_COUNT));
        for reference in candidates {
            if self
                .resolve_file(&reference, MAX_AUXILIARY_RESOURCE_BYTES)
                .is_err()
            {
                continue;
            }
            let Some(name) = reference.strip_suffix(".exp3.json") else {
                continue;
            };
            expressions.push(ExternalExpressionReference {
                name: name.to_owned(),
                reference,
            });
            if expressions.len() == MAX_EXTERNAL_EXPRESSION_COUNT {
                break;
            }
        }
        Ok(expressions)
    }
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
