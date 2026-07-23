//! 定义模型可选能力加载期间可汇总、展示的结构化诊断。

use std::fmt;

use rust_i18n::t;

use super::ModelDiagnosticResource;

/// 标识可选模型资源未能生效的具体原因。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ModelDiagnosticCategory {
    /// 引用不是安全的模型目录内相对路径。
    InvalidReference,
    /// 引用的资源不存在。
    Missing,
    /// 引用目标不是普通文件。
    NotFile,
    /// 单个资源超过读取大小上限。
    TooLarge,
    /// 资源存在但无法读取。
    Read,
    /// 资源内容无法解析。
    Parse,
    /// 动作或表情淡入淡出时长无效。
    InvalidDuration,
    /// 声明数量或 generation 累计读取超过应用处理上限。
    LimitExceeded,
    /// 后声明的成功表情覆盖了同名成功项。
    DuplicateName,
}

impl fmt::Display for ModelDiagnosticCategory {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let label = match self {
            Self::InvalidReference => "引用无效",
            Self::Missing => "资源缺失",
            Self::NotFile => "目标不是普通文件",
            Self::TooLarge => "资源过大",
            Self::Read => "读取失败",
            Self::Parse => "解析失败",
            Self::InvalidDuration => "时长无效",
            Self::LimitExceeded => "超过处理上限",
            Self::DuplicateName => "名称重复",
        };
        formatter.write_str(label)
    }
}

/// 描述一个不会阻止模型主体加载的能力问题。
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ModelLoadDiagnostic {
    resource: ModelDiagnosticResource,
    category: ModelDiagnosticCategory,
    group: Option<String>,
    name: Option<String>,
    declaration_index: Option<usize>,
    reference: Option<String>,
    affected_count: usize,
    message: String,
}

impl ModelLoadDiagnostic {
    /// 创建一个动作声明诊断。
    pub(crate) fn motion(
        group: &str,
        declaration_index: usize,
        reference: &str,
        category: ModelDiagnosticCategory,
        message: impl Into<String>,
    ) -> Self {
        Self {
            resource: ModelDiagnosticResource::Motion,
            category,
            group: Some(group.to_owned()),
            name: None,
            declaration_index: Some(declaration_index),
            reference: Some(reference.to_owned()),
            affected_count: 1,
            message: message.into(),
        }
    }

    /// 创建一个表情声明诊断。
    pub(crate) fn expression(
        name: &str,
        declaration_index: usize,
        reference: &str,
        category: ModelDiagnosticCategory,
        message: impl Into<String>,
    ) -> Self {
        Self {
            resource: ModelDiagnosticResource::Expression,
            category,
            group: None,
            name: Some(name.to_owned()),
            declaration_index: Some(declaration_index),
            reference: Some(reference.to_owned()),
            affected_count: 1,
            message: message.into(),
        }
    }

    /// 创建一个 HitArea 声明诊断。
    pub(crate) fn hit_area(
        name: &str,
        declaration_index: usize,
        reference: &str,
        category: ModelDiagnosticCategory,
        message: impl Into<String>,
    ) -> Self {
        Self {
            resource: ModelDiagnosticResource::HitArea,
            category,
            group: None,
            name: Some(name.to_owned()),
            declaration_index: Some(declaration_index),
            reference: Some(reference.to_owned()),
            affected_count: 1,
            message: message.into(),
        }
    }

    /// 设置一个聚合诊断实际影响的声明数量。
    pub(crate) fn with_affected_count(mut self, affected_count: usize) -> Self {
        self.affected_count = affected_count.max(1);
        self
    }

    /// 返回诊断对应的资源类型。
    pub(crate) fn resource(&self) -> ModelDiagnosticResource {
        self.resource
    }

    /// 返回诊断类别。
    pub(crate) fn category(&self) -> ModelDiagnosticCategory {
        self.category
    }

    /// 返回动作组名称；非动作诊断返回 `None`。
    pub(crate) fn group(&self) -> Option<&str> {
        self.group.as_deref()
    }

    /// 返回表情或 HitArea 名称；动作诊断返回 `None`。
    pub(crate) fn name(&self) -> Option<&str> {
        self.name.as_deref()
    }

    /// 返回资源在所属声明列表中的索引。
    pub(crate) fn declaration_index(&self) -> Option<usize> {
        self.declaration_index
    }

    /// 返回模型清单中的原始引用；HitArea 使用 Drawable ID。
    pub(crate) fn reference(&self) -> Option<&str> {
        self.reference.as_deref()
    }

    /// 返回该诊断影响的声明数量；普通逐项诊断固定为一项。
    pub(crate) fn affected_count(&self) -> usize {
        self.affected_count
    }

    /// 返回不含结构化定位字段的原始诊断说明。
    pub(crate) fn message(&self) -> &str {
        &self.message
    }
}

impl fmt::Display for ModelLoadDiagnostic {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.resource() {
            ModelDiagnosticResource::Motion => write!(
                formatter,
                "动作组 {}[{}]（{}）：{}，{}",
                self.group().unwrap_or("未命名"),
                self.declaration_index().unwrap_or(0),
                self.reference().unwrap_or("未提供引用"),
                self.category(),
                self.message()
            )?,
            ModelDiagnosticResource::Expression => write!(
                formatter,
                "表情 {}[{}]（{}）：{}，{}",
                self.name().unwrap_or("未命名"),
                self.declaration_index().unwrap_or(0),
                self.reference().unwrap_or("未提供引用"),
                self.category(),
                self.message()
            )?,
            ModelDiagnosticResource::HitArea => write!(
                formatter,
                "HitArea {}[{}]（{}）：{}，{}",
                self.name().unwrap_or("未命名"),
                self.declaration_index().unwrap_or(0),
                self.reference().unwrap_or("未提供引用"),
                self.category(),
                self.message()
            )?,
        }

        if self.affected_count() > 1 {
            write!(formatter, "；共影响 {} 项", self.affected_count())?;
        }
        Ok(())
    }
}

/// 保存一次模型加载过程中产生的全部非致命诊断。
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct ModelLoadDiagnostics {
    entries: Vec<ModelLoadDiagnostic>,
}

impl ModelLoadDiagnostics {
    /// 追加一个按声明顺序生成的诊断。
    pub(crate) fn push(&mut self, diagnostic: ModelLoadDiagnostic) {
        self.entries.push(diagnostic);
    }

    /// 返回按发现顺序排列的诊断。
    pub(crate) fn entries(&self) -> &[ModelLoadDiagnostic] {
        &self.entries
    }

    /// 返回本次加载是否没有产生非致命诊断。
    pub(crate) fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// 汇总各类不可用能力的声明数量，供状态界面生成简短说明。
    pub(crate) fn summary(&self) -> Option<String> {
        if self.is_empty() {
            return None;
        }

        let mut motions = 0_usize;
        let mut expressions = 0_usize;
        let mut hit_areas = 0_usize;
        for diagnostic in &self.entries {
            let count = diagnostic.affected_count();
            match diagnostic.resource() {
                ModelDiagnosticResource::Motion => motions = motions.saturating_add(count),
                ModelDiagnosticResource::Expression => {
                    expressions = expressions.saturating_add(count);
                }
                ModelDiagnosticResource::HitArea => {
                    hit_areas = hit_areas.saturating_add(count);
                }
            }
        }

        let mut labels = Vec::with_capacity(3);
        if motions > 0 {
            labels.push(t!("model_state.unavailable_motions", count = motions).to_string());
        }
        if expressions > 0 {
            labels.push(t!("model_state.unavailable_expressions", count = expressions).to_string());
        }
        if hit_areas > 0 {
            labels.push(t!("model_state.unavailable_hit_areas", count = hit_areas).to_string());
        }
        (!labels.is_empty()).then(|| labels.join(t!("model_state.summary_separator").as_ref()))
    }
}

impl Extend<ModelLoadDiagnostic> for ModelLoadDiagnostics {
    fn extend<T>(&mut self, diagnostics: T)
    where
        T: IntoIterator<Item = ModelLoadDiagnostic>,
    {
        self.entries.extend(diagnostics);
    }
}

impl IntoIterator for ModelLoadDiagnostics {
    type Item = ModelLoadDiagnostic;
    type IntoIter = std::vec::IntoIter<ModelLoadDiagnostic>;

    fn into_iter(self) -> Self::IntoIter {
        self.entries.into_iter()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn summary_counts_aggregate_and_individual_diagnostics() {
        let mut diagnostics = ModelLoadDiagnostics::default();
        diagnostics.push(
            ModelLoadDiagnostic::motion(
                "Idle",
                0,
                "missing.motion3.json",
                ModelDiagnosticCategory::Missing,
                "路径不存在",
            )
            .with_affected_count(2),
        );
        diagnostics.push(ModelLoadDiagnostic::expression(
            "Smile",
            0,
            "broken.exp3.json",
            ModelDiagnosticCategory::Parse,
            "表情 JSON 内容无效",
        ));

        let expected = [
            t!("model_state.unavailable_motions", count = 2).to_string(),
            t!("model_state.unavailable_expressions", count = 1).to_string(),
        ]
        .join(t!("model_state.summary_separator").as_ref());
        assert_eq!(diagnostics.summary().as_deref(), Some(expected.as_str()));
    }

    #[test]
    fn empty_diagnostics_have_no_summary() {
        assert!(ModelLoadDiagnostics::default().summary().is_none());
    }
}
