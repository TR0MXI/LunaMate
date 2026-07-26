//! 定义语言模型配置、稳定 Provider 标识与输入校验。

use std::{collections::HashSet, fmt, net::IpAddr, sync::Arc};

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

/// LunaMate 配置 schema 支持的稳定 Provider。
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

    /// 从持久化 ID 恢复 Provider；未知 ID 保持为配置错误而不是回退到 Ollama。
    pub(crate) fn from_id(id: &str) -> Option<Self> {
        LLM_PROVIDERS
            .into_iter()
            .find(|provider| provider.id() == id)
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

impl LlmModelConfig {
    /// 就地规范化并校验单个模型条目，不涉及跨条目的唯一性约束。
    fn normalize(&mut self) -> Result<(), ConfigWriteError> {
        self.id = normalized_required(&self.id, t!("llm.model_id").as_ref(), MAX_ID_BYTES)?;
        if !self
            .id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
        {
            return Err(invalid(t!("llm.error.id_characters").to_string()));
        }
        self.label = normalized_required(&self.label, t!("llm.name").as_ref(), MAX_LABEL_BYTES)?;
        self.model = normalized_required(
            &self.model,
            t!("llm.provider_model_id").as_ref(),
            MAX_MODEL_NAME_BYTES,
        )?;
        self.api_key =
            normalize_optional(&self.api_key, MAX_API_KEY_BYTES, t!("llm.api_key").as_ref())?;
        self.endpoint = normalize_endpoint(self.provider, self.endpoint.as_deref())?;
        Ok(())
    }
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
            model.normalize()?;
            if !ids.insert(model.id.clone()) {
                return Err(invalid(
                    t!("llm.error.duplicate_id", id = &model.id).to_string(),
                ));
            }
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

pub(in crate::config) fn normalize_endpoint(
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
        let provider_name = match provider {
            LlmProvider::Zai => "ZAI",
            LlmProvider::Baidu => "Baidu",
            _ => provider.id(),
        };
        return Err(invalid(
            t!("llm.error.endpoint_unsupported", provider = provider_name).to_string(),
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
                let mut ids = HashSet::with_capacity(models.len());
                for (index, table) in models.iter().enumerate() {
                    // 逐条规范化并跳过无效条目，避免一处手写错误丢弃其余模型和已保存的 API key。
                    let mut model = match parse_llm_model(table) {
                        Ok(model) => model,
                        Err(error) => {
                            warnings.push(format!("llm.models[{index}] 已忽略：{error}"));
                            continue;
                        }
                    };
                    if let Err(error) = model.normalize() {
                        warnings.push(format!("llm.models[{index}] 已忽略：{error}"));
                        continue;
                    }
                    if !ids.insert(model.id.clone()) {
                        warnings.push(format!(
                            "llm.models[{index}] 已忽略：{}",
                            t!("llm.error.duplicate_id", id = &model.id)
                        ));
                        continue;
                    }
                    if settings.models.len() == MAX_MODELS {
                        warnings.push(t!("llm.error.max_models", max = MAX_MODELS).to_string());
                        break;
                    }
                    settings.models.push(model);
                }
            }
            None => warnings.push("llm.models 必须是 TOML 表数组，已忽略".to_owned()),
        }
    }

    if settings.system_prompt.len() > MAX_SYSTEM_PROMPT_BYTES {
        warnings.push(
            t!(
                "llm.error.system_prompt_too_long",
                max = MAX_SYSTEM_PROMPT_BYTES
            )
            .to_string(),
        );
        settings.system_prompt = String::new();
    }
    settings.selected_model = settings
        .selected_model
        .map(|selected| selected.trim().to_owned())
        .filter(|selected| !selected.is_empty());
    if let Some(selected) = settings.selected_model.as_deref()
        && !settings.models.iter().any(|model| model.id == selected)
    {
        warnings.push(format!("llm.selected 指向不存在的模型 {selected}，已忽略"));
        settings.selected_model = None;
    }
    settings
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
