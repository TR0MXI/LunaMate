//! 定义语言模型配置、稳定 Provider 标识与输入校验。

use std::{collections::HashSet, fmt, net::IpAddr, sync::Arc};

use genai::adapter::AdapterKind;
use rust_i18n::t;
use toml_edit::{ArrayOfTables, DocumentMut, Item, Table, Value};
use url::{Host, Url};

use super::{ConfigWriteError, ensure_table_like, remove_key, set_item_value};

const MAX_MODELS: usize = 64;
const MAX_ID_BYTES: usize = 64;
const MAX_LABEL_BYTES: usize = 128;
const MAX_MODEL_NAME_BYTES: usize = 256;
const MAX_ENDPOINT_BYTES: usize = 2_048;
const MAX_API_KEY_BYTES: usize = 4 * 1024;
const MAX_SYSTEM_PROMPT_BYTES: usize = 64 * 1024;

/// `genai 0.6.5` 默认构建中可用于对话的 Provider。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum LlmProvider {
    OpenAi,
    OpenAiResponses,
    Gemini,
    Anthropic,
    Fireworks,
    Together,
    Groq,
    Aihubmix,
    Mimo,
    Moonshot,
    Nebius,
    Xai,
    DeepSeek,
    Zai,
    BigModel,
    Aliyun,
    Baidu,
    Cohere,
    Ollama,
    OllamaCloud,
    Vertex,
    GithubModels,
    OpenCodeGo,
    BedrockApi,
    OpenRouter,
    Minimax,
}

/// 保持持久化 ID 与上游 Rust 枚举解耦的完整 Provider 目录。
pub(crate) const LLM_PROVIDERS: [LlmProvider; 26] = [
    LlmProvider::OpenAi,
    LlmProvider::OpenAiResponses,
    LlmProvider::Gemini,
    LlmProvider::Anthropic,
    LlmProvider::Fireworks,
    LlmProvider::Together,
    LlmProvider::Groq,
    LlmProvider::Aihubmix,
    LlmProvider::Mimo,
    LlmProvider::Moonshot,
    LlmProvider::Nebius,
    LlmProvider::Xai,
    LlmProvider::DeepSeek,
    LlmProvider::Zai,
    LlmProvider::BigModel,
    LlmProvider::Aliyun,
    LlmProvider::Baidu,
    LlmProvider::Cohere,
    LlmProvider::Ollama,
    LlmProvider::OllamaCloud,
    LlmProvider::Vertex,
    LlmProvider::GithubModels,
    LlmProvider::OpenCodeGo,
    LlmProvider::BedrockApi,
    LlmProvider::OpenRouter,
    LlmProvider::Minimax,
];

impl LlmProvider {
    /// 返回写入配置文件的稳定小写标识。
    pub(crate) const fn id(self) -> &'static str {
        match self {
            Self::OpenAi => "openai",
            Self::OpenAiResponses => "openai-responses",
            Self::Gemini => "gemini",
            Self::Anthropic => "anthropic",
            Self::Fireworks => "fireworks",
            Self::Together => "together",
            Self::Groq => "groq",
            Self::Aihubmix => "aihubmix",
            Self::Mimo => "mimo",
            Self::Moonshot => "moonshot",
            Self::Nebius => "nebius",
            Self::Xai => "xai",
            Self::DeepSeek => "deepseek",
            Self::Zai => "zai",
            Self::BigModel => "bigmodel",
            Self::Aliyun => "aliyun",
            Self::Baidu => "baidu",
            Self::Cohere => "cohere",
            Self::Ollama => "ollama",
            Self::OllamaCloud => "ollama-cloud",
            Self::Vertex => "vertex",
            Self::GithubModels => "github-models",
            Self::OpenCodeGo => "opencode-go",
            Self::BedrockApi => "bedrock-api-key",
            Self::OpenRouter => "openrouter",
            Self::Minimax => "minimax",
        }
    }

    /// 返回设置界面中的 Provider 名称。
    pub(crate) const fn display_name(self) -> &'static str {
        match self {
            Self::OpenAi => "OpenAI",
            Self::OpenAiResponses => "OpenAI Responses",
            Self::Gemini => "Gemini",
            Self::Anthropic => "Anthropic",
            Self::Fireworks => "Fireworks",
            Self::Together => "Together",
            Self::Groq => "Groq",
            Self::Aihubmix => "AIHubMix",
            Self::Mimo => "Mimo",
            Self::Moonshot => "Moonshot",
            Self::Nebius => "Nebius",
            Self::Xai => "xAI",
            Self::DeepSeek => "DeepSeek",
            Self::Zai => "ZAI",
            Self::BigModel => "BigModel",
            Self::Aliyun => "Aliyun",
            Self::Baidu => "Baidu",
            Self::Cohere => "Cohere",
            Self::Ollama => "Ollama",
            Self::OllamaCloud => "Ollama Cloud",
            Self::Vertex => "Google Vertex",
            Self::GithubModels => "GitHub Models",
            Self::OpenCodeGo => "OpenCode Go",
            Self::BedrockApi => "Bedrock API Key",
            Self::OpenRouter => "OpenRouter",
            Self::Minimax => "MiniMax",
        }
    }

    /// 从持久化 ID 恢复 Provider；未知 ID 保持为配置错误而不是回退到 Ollama。
    pub(crate) fn from_id(id: &str) -> Option<Self> {
        LLM_PROVIDERS
            .into_iter()
            .find(|provider| provider.id() == id)
    }

    /// 从设置界面展示名恢复 Provider。
    pub(crate) fn from_display_name(name: &str) -> Option<Self> {
        LLM_PROVIDERS
            .into_iter()
            .find(|provider| provider.display_name() == name)
    }

    /// 返回锁定版 `genai` 对应的 adapter。
    pub(crate) const fn adapter_kind(self) -> AdapterKind {
        match self {
            Self::OpenAi => AdapterKind::OpenAI,
            Self::OpenAiResponses => AdapterKind::OpenAIResp,
            Self::Gemini => AdapterKind::Gemini,
            Self::Anthropic => AdapterKind::Anthropic,
            Self::Fireworks => AdapterKind::Fireworks,
            Self::Together => AdapterKind::Together,
            Self::Groq => AdapterKind::Groq,
            Self::Aihubmix => AdapterKind::Aihubmix,
            Self::Mimo => AdapterKind::Mimo,
            Self::Moonshot => AdapterKind::Moonshot,
            Self::Nebius => AdapterKind::Nebius,
            Self::Xai => AdapterKind::Xai,
            Self::DeepSeek => AdapterKind::DeepSeek,
            Self::Zai => AdapterKind::Zai,
            Self::BigModel => AdapterKind::BigModel,
            Self::Aliyun => AdapterKind::Aliyun,
            Self::Baidu => AdapterKind::Baidu,
            Self::Cohere => AdapterKind::Cohere,
            Self::Ollama => AdapterKind::Ollama,
            Self::OllamaCloud => AdapterKind::OllamaCloud,
            Self::Vertex => AdapterKind::Vertex,
            Self::GithubModels => AdapterKind::GithubCopilot,
            Self::OpenCodeGo => AdapterKind::OpenCodeGo,
            Self::BedrockApi => AdapterKind::BedrockApi,
            Self::OpenRouter => AdapterKind::OpenRouter,
            Self::Minimax => AdapterKind::MiniMax,
        }
    }

    fn allows_endpoint_override(self) -> bool {
        !matches!(self, Self::Zai | Self::Baidu)
    }
}

/// 一个可选择的语言模型配置；API key 由用户直接填写并保存在本地配置中。
#[derive(Clone, Eq, PartialEq)]
pub(crate) struct LlmModelConfig {
    pub(crate) id: String,
    pub(crate) label: String,
    pub(crate) provider: LlmProvider,
    pub(crate) model: String,
    pub(crate) endpoint: Option<String>,
    pub(crate) api_key: Option<String>,
}

impl fmt::Debug for LlmModelConfig {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("LlmModelConfig")
            .field("id", &self.id)
            .field("label", &self.label)
            .field("provider", &self.provider)
            .field("model", &self.model)
            .field("endpoint", &self.endpoint)
            .field("api_key", &self.api_key.as_ref().map(|_| "[REDACTED]"))
            .finish()
    }
}

impl LlmModelConfig {
    /// 创建设置界面中的新模型草稿。
    pub(crate) fn draft(id: String) -> Self {
        Self {
            id,
            label: "新模型".to_owned(),
            provider: LlmProvider::Ollama,
            model: String::new(),
            endpoint: Some("http://localhost:11434/".to_owned()),
            api_key: None,
        }
    }
}

/// 一次性发布的语言模型、当前选择与系统提示词配置。
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct LlmSettings {
    pub(crate) models: Vec<LlmModelConfig>,
    pub(crate) selected_model: Option<String>,
    pub(crate) system_prompt: String,
}

impl LlmSettings {
    /// 返回当前选择且仍存在的模型。
    pub(crate) fn selected(&self) -> Option<&LlmModelConfig> {
        let selected = self.selected_model.as_deref()?;
        self.models.iter().find(|model| model.id == selected)
    }

    /// 规范化并校验准备发布的完整配置。
    pub(crate) fn normalized(mut self) -> Result<Self, ConfigWriteError> {
        if self.models.len() > MAX_MODELS {
            return Err(invalid(
                t!("llm.error.max_models", max = MAX_MODELS).to_string(),
            ));
        }
        if self.system_prompt.len() > MAX_SYSTEM_PROMPT_BYTES {
            return Err(invalid(
                t!(
                    "llm.error.system_prompt_too_long",
                    max = MAX_SYSTEM_PROMPT_BYTES
                )
                .to_string(),
            ));
        }

        let mut ids = HashSet::with_capacity(self.models.len());
        for model in &mut self.models {
            model.id = normalized_required(&model.id, t!("llm.model_id").as_ref(), MAX_ID_BYTES)?;
            if !model
                .id
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
            {
                return Err(invalid(t!("llm.error.id_characters").to_string()));
            }
            if !ids.insert(model.id.clone()) {
                return Err(invalid(
                    t!("llm.error.duplicate_id", id = &model.id).to_string(),
                ));
            }
            model.label =
                normalized_required(&model.label, t!("llm.name").as_ref(), MAX_LABEL_BYTES)?;
            model.model = normalized_required(
                &model.model,
                t!("llm.provider_model_id").as_ref(),
                MAX_MODEL_NAME_BYTES,
            )?;
            model.api_key = normalize_optional(
                &model.api_key,
                MAX_API_KEY_BYTES,
                t!("llm.api_key").as_ref(),
            )?;
            model.endpoint = normalize_endpoint(model.provider, model.endpoint.as_deref())?;
        }

        self.selected_model = normalize_optional(
            &self.selected_model,
            MAX_ID_BYTES,
            t!("llm.selected").as_ref(),
        )?;
        if let Some(selected) = &self.selected_model
            && !ids.contains(selected)
        {
            return Err(invalid(
                t!("llm.error.missing_selected", id = selected).to_string(),
            ));
        }
        Ok(self)
    }
}

fn normalized_required(
    value: &str,
    field: &str,
    max_bytes: usize,
) -> Result<String, ConfigWriteError> {
    let value = value.trim();
    if value.is_empty() {
        return Err(invalid(t!("llm.error.required", field = field).to_string()));
    }
    if value.len() > max_bytes {
        return Err(invalid(
            t!("llm.error.too_long", field = field, max = max_bytes).to_string(),
        ));
    }
    Ok(value.to_owned())
}

fn normalize_optional(
    value: &Option<String>,
    max_bytes: usize,
    field: &str,
) -> Result<Option<String>, ConfigWriteError> {
    let value = value
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty());
    match value {
        Some(value) if value.len() > max_bytes => Err(invalid(
            t!("llm.error.too_long", field = field, max = max_bytes).to_string(),
        )),
        Some(value) => Ok(Some(value.to_owned())),
        None => Ok(None),
    }
}

fn normalize_endpoint(
    provider: LlmProvider,
    endpoint: Option<&str>,
) -> Result<Option<String>, ConfigWriteError> {
    let Some(endpoint) = endpoint
        .map(str::trim)
        .filter(|endpoint| !endpoint.is_empty())
    else {
        return Ok(None);
    };
    if endpoint.len() > MAX_ENDPOINT_BYTES {
        return Err(invalid(
            t!(
                "llm.error.too_long",
                field = "Endpoint",
                max = MAX_ENDPOINT_BYTES
            )
            .to_string(),
        ));
    }
    if !provider.allows_endpoint_override() {
        return Err(invalid(
            t!(
                "llm.error.endpoint_unsupported",
                provider = provider.display_name()
            )
            .to_string(),
        ));
    }

    let mut url = Url::parse(endpoint)
        .map_err(|error| invalid(t!("llm.error.endpoint_invalid", error = error).to_string()))?;
    if url.host().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err(invalid(t!("llm.error.endpoint_requirements").to_string()));
    }
    let allows_http = endpoint_is_loopback(&url);
    if url.scheme() != "https" && !(url.scheme() == "http" && allows_http) {
        return Err(invalid(t!("llm.error.endpoint_https").to_string()));
    }

    let path = url.path().trim_end_matches('/');
    let normalized_path = if path.is_empty() {
        "/".to_owned()
    } else {
        format!("{path}/")
    };
    url.set_path(&normalized_path);
    Ok(Some(url.into()))
}

fn endpoint_is_loopback(url: &Url) -> bool {
    match url.host() {
        Some(Host::Domain(domain)) => domain.eq_ignore_ascii_case("localhost"),
        Some(Host::Ipv4(address)) => IpAddr::V4(address).is_loopback(),
        Some(Host::Ipv6(address)) => IpAddr::V6(address).is_loopback(),
        None => false,
    }
}

fn invalid(message: impl Into<String>) -> ConfigWriteError {
    ConfigWriteError::InvalidValue(message.into())
}

/// 为跨线程任务共享当前语言模型配置提供清晰的所有权类型。
pub(crate) type SharedLlmSettings = Arc<LlmSettings>;

pub(super) fn parse_llm_settings(
    document: &DocumentMut,
    warnings: &mut Vec<String>,
) -> LlmSettings {
    let mut settings = LlmSettings::default();
    let Some(llm) = document.get("llm") else {
        return settings;
    };

    if let Some(selected) = llm.get("selected") {
        match selected.as_str() {
            Some(selected) => settings.selected_model = Some(selected.to_owned()),
            None => warnings.push("llm.selected 必须是字符串，已忽略".to_owned()),
        }
    }
    if let Some(prompt) = llm.get("system_prompt") {
        match prompt.as_str() {
            Some(prompt) => settings.system_prompt = prompt.to_owned(),
            None => warnings.push("llm.system_prompt 必须是字符串，已忽略".to_owned()),
        }
    }
    if let Some(models) = llm.get("models") {
        match models.as_array_of_tables() {
            Some(models) => {
                for (index, table) in models.iter().enumerate() {
                    match parse_llm_model(table) {
                        Ok(model) => settings.models.push(model),
                        Err(error) => warnings.push(format!("llm.models[{index}] 已忽略：{error}")),
                    }
                }
            }
            None => warnings.push("llm.models 必须是 TOML 表数组，已忽略".to_owned()),
        }
    }

    if let Some(selected) = settings.selected_model.as_deref()
        && !settings.models.iter().any(|model| model.id == selected)
    {
        warnings.push(format!("llm.selected 指向不存在的模型 {selected}，已忽略"));
        settings.selected_model = None;
    }
    match settings.normalized() {
        Ok(settings) => settings,
        Err(error) => {
            warnings.push(format!("语言模型配置无效，已使用空配置：{error}"));
            LlmSettings::default()
        }
    }
}

fn parse_llm_model(table: &Table) -> Result<LlmModelConfig, ConfigWriteError> {
    let required = |key: &str| {
        table
            .get(key)
            .and_then(Item::as_str)
            .map(str::to_owned)
            .ok_or_else(|| ConfigWriteError::InvalidValue(format!("{key} 必须是字符串")))
    };
    let provider_id = required("provider")?;
    let provider = LlmProvider::from_id(&provider_id)
        .ok_or_else(|| ConfigWriteError::InvalidValue(format!("未知 Provider：{provider_id}")))?;
    let optional = |key: &str| table.get(key).and_then(Item::as_str).map(str::to_owned);

    Ok(LlmModelConfig {
        id: required("id")?,
        label: required("label")?,
        provider,
        model: required("model")?,
        endpoint: optional("endpoint"),
        api_key: optional("api_key"),
    })
}

pub(super) fn write_llm_settings(document: &mut DocumentMut, settings: &LlmSettings) {
    ensure_table_like(&mut document["llm"]);
    if let Some(mut key) = document.as_table_mut().key_mut("llm") {
        key.fmt();
    }
    match &settings.selected_model {
        Some(selected) => set_item_value(
            &mut document["llm"]["selected"],
            Value::from(selected.clone()),
        ),
        None => remove_key(document, "llm", "selected"),
    }
    set_item_value(
        &mut document["llm"]["system_prompt"],
        Value::from(settings.system_prompt.clone()),
    );

    let existing_models = document
        .get("llm")
        .and_then(|llm| llm.get("models"))
        .and_then(Item::as_array_of_tables)
        .map(|models| models.iter().cloned().collect::<Vec<_>>())
        .unwrap_or_default();
    let mut models = ArrayOfTables::new();
    for model in &settings.models {
        let mut table = existing_models
            .iter()
            .find(|table| table.get("id").and_then(Item::as_str) == Some(&model.id))
            .cloned()
            .unwrap_or_else(Table::new);
        set_item_value(&mut table["id"], Value::from(model.id.clone()));
        set_item_value(&mut table["label"], Value::from(model.label.clone()));
        set_item_value(&mut table["provider"], Value::from(model.provider.id()));
        set_item_value(&mut table["model"], Value::from(model.model.clone()));
        if let Some(endpoint) = &model.endpoint {
            set_item_value(&mut table["endpoint"], Value::from(endpoint.clone()));
        } else {
            table.remove("endpoint");
        }
        if let Some(api_key) = &model.api_key {
            set_item_value(&mut table["api_key"], Value::from(api_key.clone()));
        } else {
            table.remove("api_key");
        }
        table.remove("api_key_env");
        models.push(table);
    }
    document["llm"]["models"] = Item::ArrayOfTables(models);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn provider_ids_are_unique_and_round_trip() {
        let ids = LLM_PROVIDERS
            .into_iter()
            .map(LlmProvider::id)
            .collect::<HashSet<_>>();
        assert_eq!(ids.len(), LLM_PROVIDERS.len());
        for provider in LLM_PROVIDERS {
            assert_eq!(LlmProvider::from_id(provider.id()), Some(provider));
        }
    }

    #[test]
    fn settings_reject_missing_selection_and_duplicate_ids() {
        let model = LlmModelConfig {
            id: "local".to_owned(),
            label: "Local".to_owned(),
            provider: LlmProvider::Ollama,
            model: "qwen3:8b".to_owned(),
            endpoint: Some("http://localhost:11434".to_owned()),
            api_key: None,
        };
        let duplicate = LlmSettings {
            models: vec![model.clone(), model],
            selected_model: Some("local".to_owned()),
            system_prompt: String::new(),
        };
        assert!(duplicate.normalized().is_err());

        let missing = LlmSettings {
            selected_model: Some("missing".to_owned()),
            ..LlmSettings::default()
        };
        assert!(missing.normalized().is_err());
    }

    #[test]
    fn direct_api_key_is_normalized_and_redacted_in_debug() {
        let settings = LlmSettings {
            models: vec![LlmModelConfig {
                id: "cloud".to_owned(),
                label: "Cloud".to_owned(),
                provider: LlmProvider::OpenAi,
                model: "gpt-5-mini".to_owned(),
                endpoint: None,
                api_key: Some(" 1/key+=value ".to_owned()),
            }],
            selected_model: Some("cloud".to_owned()),
            system_prompt: String::new(),
        };
        let normalized = settings
            .normalized()
            .expect("直接填写的 API key 应当可以规范化");
        let model = normalized.models.first().expect("测试模型应当存在");

        assert_eq!(model.api_key.as_deref(), Some("1/key+=value"));
        let debug = format!("{model:?}");
        assert!(debug.contains("[REDACTED]"));
        assert!(!debug.contains("1/key+=value"));
    }

    #[test]
    fn legacy_environment_key_reference_is_ignored_and_removed_on_write() {
        let mut document = r#"
[llm]
selected = "cloud"

[[llm.models]]
id = "cloud"
label = "Cloud"
provider = "openai"
model = "gpt-5-mini"
api_key_env = "OPENAI_API_KEY"
"#
        .parse::<DocumentMut>()
        .expect("旧版语言模型配置应当可以解析");
        let mut warnings = Vec::new();
        let settings = parse_llm_settings(&document, &mut warnings);

        assert!(warnings.is_empty());
        assert_eq!(
            settings
                .selected()
                .and_then(|model| model.api_key.as_deref()),
            None
        );
        write_llm_settings(&mut document, &settings);
        assert!(!document.to_string().contains("api_key_env"));
    }

    #[test]
    fn endpoint_normalization_preserves_base_path() {
        assert_eq!(
            normalize_endpoint(LlmProvider::OpenAi, Some("https://example.com/v1"))
                .expect("HTTPS endpoint 应当有效")
                .as_deref(),
            Some("https://example.com/v1/")
        );
        assert!(normalize_endpoint(LlmProvider::OpenAi, Some("http://example.com/v1")).is_err());
        assert!(normalize_endpoint(LlmProvider::Ollama, Some("http://example.com")).is_err());
    }
}
