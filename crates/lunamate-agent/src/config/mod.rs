//! 定义 Agent 运行所需的配置快照、稳定标识与输入校验。

use std::{error::Error, fmt};

pub use genai::{adapter::AdapterKind as LlmProvider, chat::ReasoningEffort};
use rust_i18n::t;

mod catalog;
mod endpoint;
mod model;
mod persona;

pub use catalog::*;
pub(crate) use endpoint::endpoint_is_plaintext_loopback;
pub use endpoint::normalize_endpoint;
pub use model::*;
pub use persona::*;

pub(super) const MAX_ID_BYTES: usize = 64;
pub(super) const MAX_LABEL_BYTES: usize = 128;
pub(super) const MAX_SYSTEM_PROMPT_BYTES: usize = 64 * 1024;

/// Agent 配置校验失败，不包含凭据或完整提示词。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AgentConfigError(String);

impl AgentConfigError {
    fn new(message: impl Into<String>) -> Self {
        Self(message.into())
    }
}

impl fmt::Display for AgentConfigError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl Error for AgentConfigError {}

/// 应用支持的 Agent 提示词语言。
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum AppLanguage {
    #[default]
    SimplifiedChinese,
    TraditionalChinese,
    English,
    Japanese,
}

impl AppLanguage {
    pub const fn id(self) -> &'static str {
        match self {
            Self::SimplifiedChinese => "zh-CN",
            Self::TraditionalChinese => "zh-TW",
            Self::English => "en",
            Self::Japanese => "ja",
        }
    }

    pub fn from_id(value: &str) -> Option<Self> {
        match value {
            "zh-CN" => Some(Self::SimplifiedChinese),
            "zh-TW" => Some(Self::TraditionalChinese),
            "en" => Some(Self::English),
            "ja" => Some(Self::Japanese),
            _ => None,
        }
    }
}

pub(super) fn normalized_safe_id(
    id: &str,
    field: &str,
    language: AppLanguage,
) -> Result<String, AgentConfigError> {
    let id = normalized_required(id, field, MAX_ID_BYTES, language)?;
    if !id
        .bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
    {
        return Err(invalid(
            t!(
                "llm.error.id_characters",
                locale = language.id(),
                field = field
            )
            .to_string(),
        ));
    }
    Ok(id)
}

pub(super) fn normalized_required(
    value: &str,
    field: &str,
    max_bytes: usize,
    language: AppLanguage,
) -> Result<String, AgentConfigError> {
    let value = value.trim();
    if value.is_empty() {
        return Err(invalid(
            t!("llm.error.required", locale = language.id(), field = field).to_string(),
        ));
    }
    if value.len() > max_bytes {
        return Err(invalid(
            t!(
                "llm.error.too_long",
                locale = language.id(),
                field = field,
                max = max_bytes
            )
            .to_string(),
        ));
    }
    Ok(value.to_owned())
}

pub(super) fn normalize_optional(
    value: &Option<String>,
    max_bytes: usize,
    field: &str,
    language: AppLanguage,
) -> Result<Option<String>, AgentConfigError> {
    let value = value
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty());
    match value {
        Some(value) if value.len() > max_bytes => Err(invalid(
            t!(
                "llm.error.too_long",
                locale = language.id(),
                field = field,
                max = max_bytes
            )
            .to_string(),
        )),
        Some(value) => Ok(Some(value.to_owned())),
        None => Ok(None),
    }
}

pub(super) fn check_range(
    value: Option<u32>,
    min: u32,
    max: u32,
    field: &str,
    language: AppLanguage,
) -> Result<(), AgentConfigError> {
    match value {
        Some(value) if !(min..=max).contains(&value) => Err(invalid(
            t!(
                "llm.error.out_of_range",
                locale = language.id(),
                field = field,
                min = min,
                max = max
            )
            .to_string(),
        )),
        _ => Ok(()),
    }
}

pub(super) fn check_ratio(
    value: Option<f64>,
    min: f64,
    max: f64,
    field: &str,
    language: AppLanguage,
) -> Result<(), AgentConfigError> {
    let Some(value) = value else {
        return Ok(());
    };
    if !value.is_finite() || value < min || value > max {
        return Err(invalid(
            t!(
                "llm.error.out_of_range",
                locale = language.id(),
                field = field,
                min = format!("{min}"),
                max = format!("{max}")
            )
            .to_string(),
        ));
    }
    Ok(())
}

pub(super) fn invalid(message: impl Into<String>) -> AgentConfigError {
    AgentConfigError::new(message)
}
