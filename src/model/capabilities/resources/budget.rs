//! 管理动作与表情资源的单文件读取和 generation 累计预算。

use std::{
    io::{ErrorKind, Read as _},
    path::{Path, PathBuf},
};

use super::{
    MAX_AUXILIARY_GENERATION_BYTES, ModelDiagnosticCategory, ModelResourceResolver,
    ResourceResolutionError, opened_file::validate_resource_metadata,
};

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

impl ModelResourceResolver {
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
        checkpoint: impl FnMut() -> bool,
    ) -> Result<String, ResourceResolutionError> {
        self.read_text_with_path_and_budget_and_checkpoint(
            reference,
            maximum_bytes,
            budget,
            checkpoint,
        )
        .map(|(_, source)| source)
    }

    /// 读取文本并返回 generation 内去重用的规范标签路径；安全边界仍由已打开句柄承担。
    pub(crate) fn read_text_with_path_and_budget_and_checkpoint(
        &self,
        reference: &str,
        maximum_bytes: u64,
        budget: &mut AuxiliaryResourceBudget,
        mut checkpoint: impl FnMut() -> bool,
    ) -> Result<(PathBuf, String), ResourceResolutionError> {
        let mut before_open = |_: &Path| {};
        self.read_text_with_budget_and_hooks(
            reference,
            maximum_bytes,
            budget,
            &mut checkpoint,
            &mut before_open,
        )
    }

    /// 在最终路径打开前运行测试替换操作，且读取仍复用生产身份校验与预算逻辑。
    #[cfg(test)]
    pub(in crate::model) fn read_text_with_open_hook_for_test(
        &self,
        reference: &str,
        maximum_bytes: u64,
        mut before_open: impl FnMut(&Path),
    ) -> Result<String, ResourceResolutionError> {
        let mut budget = AuxiliaryResourceBudget::with_limit(maximum_bytes);
        let mut checkpoint = || false;
        self.read_text_with_budget_and_hooks(
            reference,
            maximum_bytes,
            &mut budget,
            &mut checkpoint,
            &mut before_open,
        )
        .map(|(_, source)| source)
    }

    fn read_text_with_budget_and_hooks(
        &self,
        reference: &str,
        maximum_bytes: u64,
        budget: &mut AuxiliaryResourceBudget,
        checkpoint: &mut dyn FnMut() -> bool,
        before_open: &mut dyn FnMut(&Path),
    ) -> Result<(PathBuf, String), ResourceResolutionError> {
        let (path, mut file) = self.open_resolved_file(reference, maximum_bytes, before_open)?;
        let metadata = file
            .metadata()
            .map_err(|error| ResourceResolutionError::from_io(&path, &error))?;
        validate_resource_metadata(&metadata, maximum_bytes)?;
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
            let read = loop {
                match file.read(&mut bytes[start..start + chunk_size]) {
                    Ok(read) => break read,
                    Err(error) if error.kind() == ErrorKind::Interrupted => continue,
                    Err(error) => return Err(ResourceResolutionError::from_io(&path, &error)),
                }
            };
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
        let source = String::from_utf8(bytes).map_err(|_| {
            ResourceResolutionError::new(
                ModelDiagnosticCategory::Parse,
                "资源内容不是有效的 UTF-8 文本",
            )
        })?;
        Ok((path, source))
    }
}
