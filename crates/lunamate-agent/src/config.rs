//! 定义 Agent 运行所需的配置快照、稳定标识与输入校验。

use std::{
    collections::HashSet,
    error::Error,
    fmt,
    path::{Component, Path, PathBuf},
    sync::Arc,
};

pub use genai::{adapter::AdapterKind as LlmProvider, chat::ReasoningEffort};
use rust_i18n::t;
use url::Url;

const MAX_MODELS: usize = 64;
const MAX_ID_BYTES: usize = 64;
const MAX_LABEL_BYTES: usize = 128;
const MAX_MODEL_NAME_BYTES: usize = 256;
const MAX_ENDPOINT_BYTES: usize = 2_048;
const MAX_API_KEY_BYTES: usize = 4 * 1024;
const MAX_LOCAL_MODEL_PATH_BYTES: usize = 4 * 1024;
const MAX_SYSTEM_PROMPT_BYTES: usize = 64 * 1024;
const MAX_PERSONA_NAME_BYTES: usize = 128;

pub const MAX_OUTPUT_TOKENS_MIN: u32 = 1;
pub const MAX_OUTPUT_TOKENS_MAX: u32 = 1_000_000;
pub const MODEL_CONTEXT_TOKENS_MIN: u32 = 256;
pub const MODEL_CONTEXT_TOKENS_MAX: u32 = 10_000_000;
pub const REASONING_BUDGET_MIN: u32 = 0;
pub const REASONING_BUDGET_MAX: u32 = 1_000_000;
pub const TEMPERATURE_MIN: f64 = 0.0;
pub const TEMPERATURE_MAX: f64 = 2.0;
pub const TOP_P_MIN: f64 = 0.0;
pub const TOP_P_MAX: f64 = 1.0;
pub const DEFAULT_MAX_OUTPUT_TOKENS: u32 = 4_096;
pub const DEFAULT_MODEL_CONTEXT_TOKENS: u32 = 128_000;
pub const DEFAULT_REASONING_BUDGET: u32 = 8_192;
pub const DEFAULT_TEMPERATURE: f64 = 1.0;
pub const DEFAULT_TOP_P: f64 = 1.0;
pub const MODEL_CONTEXT_RESERVE_TOKENS: u32 = 512;

pub const DEFAULT_PERSONA_ID: &str = "default";
pub const CONTEXT_MESSAGES_MIN: u32 = 2;
pub const CONTEXT_MESSAGES_MAX: u32 = 512;
pub const CONTEXT_TOKENS_MIN: u32 = 256;
pub const CONTEXT_TOKENS_MAX: u32 = 1_050_624;
pub const DEFAULT_CONTEXT_MESSAGES: u32 = 64;
pub const DEFAULT_CONTEXT_TOKENS: u32 = 65_792;

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

/// LunaMate 配置和界面按此顺序支持 Provider；类型本身直接复用 `genai` adapter 标识。
pub const LLM_PROVIDERS: [LlmProvider; 26] = [
    LlmProvider::OpenAI,
    LlmProvider::OpenAIResp,
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
    LlmProvider::GithubCopilot,
    LlmProvider::OpenCodeGo,
    LlmProvider::BedrockApi,
    LlmProvider::OpenRouter,
    LlmProvider::MiniMax,
];

/// 模型条目提供的专业能力；稳定 ID 同时用于配置文件与 UI 分组。
#[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq)]
pub enum ModelKind {
    #[default]
    ChatCompletions,
    SpeechSynthesis,
    Transcription,
}

impl ModelKind {
    pub const fn id(self) -> &'static str {
        match self {
            Self::ChatCompletions => "chat-completions",
            Self::SpeechSynthesis => "speech-synthesis",
            Self::Transcription => "transcription",
        }
    }

    pub fn from_id(id: &str) -> Option<Self> {
        match id {
            "chat-completions" => Some(Self::ChatCompletions),
            "speech-synthesis" => Some(Self::SpeechSynthesis),
            "transcription" => Some(Self::Transcription),
            _ => None,
        }
    }
}

/// 一个模型条目的执行后端。远端对话继续直接复用 `genai` 的 Provider 标识。
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum ModelProvider {
    Genai(LlmProvider),
    Doubao,
    LocalWhisper,
}

/// whisper.cpp 当前支持的目标语言代码；空值表示让模型自动识别语种。
pub const WHISPER_LANGUAGE_CODES: [&str; 100] = [
    "en", "zh", "de", "es", "ru", "ko", "fr", "ja", "pt", "tr", "pl", "ca", "nl", "ar", "sv", "it",
    "id", "hi", "fi", "vi", "he", "uk", "el", "ms", "cs", "ro", "da", "hu", "ta", "no", "th", "ur",
    "hr", "bg", "lt", "la", "mi", "ml", "cy", "sk", "te", "fa", "lv", "bn", "sr", "az", "sl", "kn",
    "et", "mk", "br", "eu", "is", "hy", "ne", "mn", "bs", "kk", "sq", "sw", "gl", "mr", "pa", "si",
    "km", "sn", "yo", "so", "af", "oc", "ka", "be", "tg", "sd", "gu", "am", "yi", "lo", "uz", "fo",
    "ht", "ps", "tk", "nn", "mt", "sa", "lb", "my", "bo", "tl", "mg", "as", "tt", "haw", "ln",
    "ha", "ba", "jw", "su", "yue",
];

/// 与 [`WHISPER_LANGUAGE_CODES`] 顺序对应的英文名称，仅用于设置界面展示。
pub const WHISPER_LANGUAGE_NAMES: [&str; 100] = [
    "English",
    "Chinese",
    "German",
    "Spanish",
    "Russian",
    "Korean",
    "French",
    "Japanese",
    "Portuguese",
    "Turkish",
    "Polish",
    "Catalan",
    "Dutch",
    "Arabic",
    "Swedish",
    "Italian",
    "Indonesian",
    "Hindi",
    "Finnish",
    "Vietnamese",
    "Hebrew",
    "Ukrainian",
    "Greek",
    "Malay",
    "Czech",
    "Romanian",
    "Danish",
    "Hungarian",
    "Tamil",
    "Norwegian",
    "Thai",
    "Urdu",
    "Croatian",
    "Bulgarian",
    "Lithuanian",
    "Latin",
    "Maori",
    "Malayalam",
    "Welsh",
    "Slovak",
    "Telugu",
    "Persian",
    "Latvian",
    "Bengali",
    "Serbian",
    "Azerbaijani",
    "Slovenian",
    "Kannada",
    "Estonian",
    "Macedonian",
    "Breton",
    "Basque",
    "Icelandic",
    "Armenian",
    "Nepali",
    "Mongolian",
    "Bosnian",
    "Kazakh",
    "Albanian",
    "Swahili",
    "Galician",
    "Marathi",
    "Punjabi",
    "Sinhala",
    "Khmer",
    "Shona",
    "Yoruba",
    "Somali",
    "Afrikaans",
    "Occitan",
    "Georgian",
    "Belarusian",
    "Tajik",
    "Sindhi",
    "Gujarati",
    "Amharic",
    "Yiddish",
    "Lao",
    "Uzbek",
    "Faroese",
    "Haitian Creole",
    "Pashto",
    "Turkmen",
    "Nynorsk",
    "Maltese",
    "Sanskrit",
    "Luxembourgish",
    "Myanmar",
    "Tibetan",
    "Tagalog",
    "Malagasy",
    "Assamese",
    "Tatar",
    "Hawaiian",
    "Lingala",
    "Hausa",
    "Bashkir",
    "Javanese",
    "Sundanese",
    "Cantonese",
];

impl Default for ModelProvider {
    fn default() -> Self {
        Self::Genai(LlmProvider::Ollama)
    }
}

impl From<LlmProvider> for ModelProvider {
    fn from(provider: LlmProvider) -> Self {
        Self::Genai(provider)
    }
}

impl ModelProvider {
    pub const fn id(self) -> &'static str {
        match self {
            Self::Genai(provider) => llm_provider_id(provider),
            Self::Doubao => "doubao",
            Self::LocalWhisper => "local-whisper",
        }
    }

    pub fn from_id(id: &str) -> Option<Self> {
        match id {
            "doubao" => Some(Self::Doubao),
            "local-whisper" => Some(Self::LocalWhisper),
            _ => llm_provider_from_id(id).map(Self::Genai),
        }
    }

    pub const fn genai(self) -> Option<LlmProvider> {
        match self {
            Self::Genai(provider) => Some(provider),
            Self::Doubao | Self::LocalWhisper => None,
        }
    }
}

/// 返回写入 LunaMate 配置文件和资源路径的稳定 Provider ID。
pub const fn llm_provider_id(provider: LlmProvider) -> &'static str {
    match provider {
        LlmProvider::OpenAI => "openai",
        LlmProvider::OpenAIResp => "openai-responses",
        LlmProvider::Gemini => "gemini",
        LlmProvider::Anthropic => "anthropic",
        LlmProvider::Fireworks => "fireworks",
        LlmProvider::Together => "together",
        LlmProvider::Groq => "groq",
        LlmProvider::Aihubmix => "aihubmix",
        LlmProvider::Mimo => "mimo",
        LlmProvider::Moonshot => "moonshot",
        LlmProvider::Nebius => "nebius",
        LlmProvider::Xai => "xai",
        LlmProvider::DeepSeek => "deepseek",
        LlmProvider::Zai => "zai",
        LlmProvider::BigModel => "bigmodel",
        LlmProvider::Aliyun => "aliyun",
        LlmProvider::Baidu => "baidu",
        LlmProvider::Cohere => "cohere",
        LlmProvider::Ollama => "ollama",
        LlmProvider::OllamaCloud => "ollama-cloud",
        LlmProvider::Vertex => "vertex",
        LlmProvider::GithubCopilot => "github-models",
        LlmProvider::OpenCodeGo => "opencode-go",
        LlmProvider::BedrockApi => "bedrock-api-key",
        LlmProvider::OpenRouter => "openrouter",
        LlmProvider::MiniMax => "minimax",
    }
}

/// 从 LunaMate 的稳定配置 ID 恢复 `genai` adapter 标识。
pub fn llm_provider_from_id(id: &str) -> Option<LlmProvider> {
    LLM_PROVIDERS
        .into_iter()
        .find(|provider| llm_provider_id(*provider) == id)
}

pub const REASONING_EFFORT_LEVELS: [ReasoningEffort; 7] = [
    ReasoningEffort::None,
    ReasoningEffort::Minimal,
    ReasoningEffort::Low,
    ReasoningEffort::Medium,
    ReasoningEffort::High,
    ReasoningEffort::XHigh,
    ReasoningEffort::Max,
];

/// 返回写入 LunaMate 配置文件的稳定思考强度 ID。
pub fn reasoning_effort_id(effort: &ReasoningEffort) -> &'static str {
    match effort {
        ReasoningEffort::None => "off",
        _ => effort.variant_name(),
    }
}

/// 从 LunaMate 的稳定配置 ID 与可选预算恢复思考强度。
pub fn reasoning_effort_from_id(id: &str, budget: Option<u32>) -> Option<ReasoningEffort> {
    if id == "off" {
        return Some(ReasoningEffort::None);
    }
    if id == "budget" {
        return Some(ReasoningEffort::Budget(
            budget.unwrap_or(DEFAULT_REASONING_BUDGET),
        ));
    }
    REASONING_EFFORT_LEVELS
        .iter()
        .skip(1)
        .find(|effort| effort.variant_name() == id)
        .cloned()
}

/// 返回自定义思考预算；固定强度没有预算值。
pub const fn reasoning_budget(effort: &ReasoningEffort) -> Option<u32> {
    match effort {
        ReasoningEffort::Budget(tokens) => Some(*tokens),
        _ => None,
    }
}

#[derive(Clone, Debug, Default)]
pub struct LlmAdvancedOptions {
    pub context_window_tokens: Option<u32>,
    pub reasoning_effort: Option<ReasoningEffort>,
    pub max_output_tokens: Option<u32>,
    pub temperature: Option<f64>,
    pub top_p: Option<f64>,
}

impl PartialEq for LlmAdvancedOptions {
    fn eq(&self, other: &Self) -> bool {
        self.context_window_tokens == other.context_window_tokens
            && reasoning_efforts_equal(&self.reasoning_effort, &other.reasoning_effort)
            && self.max_output_tokens == other.max_output_tokens
            && self.temperature == other.temperature
            && self.top_p == other.top_p
    }
}

fn reasoning_efforts_equal(
    left: &Option<ReasoningEffort>,
    right: &Option<ReasoningEffort>,
) -> bool {
    match (left, right) {
        (None, None) => true,
        (Some(ReasoningEffort::Budget(left)), Some(ReasoningEffort::Budget(right))) => {
            left == right
        }
        (Some(left), Some(right)) => left.variant_name() == right.variant_name(),
        _ => false,
    }
}

impl LlmAdvancedOptions {
    fn normalize(&mut self, language: AppLanguage) -> Result<(), AgentConfigError> {
        check_range(
            self.context_window_tokens,
            MODEL_CONTEXT_TOKENS_MIN,
            MODEL_CONTEXT_TOKENS_MAX,
            t!("llm.context_window_tokens", locale = language.id()).as_ref(),
            language,
        )?;
        if let Some(ReasoningEffort::Budget(tokens)) = self.reasoning_effort.as_ref() {
            check_range(
                Some(*tokens),
                REASONING_BUDGET_MIN,
                REASONING_BUDGET_MAX,
                t!("llm.reasoning_budget", locale = language.id()).as_ref(),
                language,
            )?;
        }
        check_range(
            self.max_output_tokens,
            MAX_OUTPUT_TOKENS_MIN,
            MAX_OUTPUT_TOKENS_MAX,
            t!("llm.max_output_tokens", locale = language.id()).as_ref(),
            language,
        )?;
        if let Some(window) = self.context_window_tokens {
            let output = self.max_output_tokens.unwrap_or(DEFAULT_MAX_OUTPUT_TOKENS);
            if output
                .saturating_add(MODEL_CONTEXT_RESERVE_TOKENS)
                .saturating_add(8)
                > window
            {
                return Err(invalid(
                    t!(
                        "llm.error.context_window_output",
                        locale = language.id(),
                        context = window,
                        output = output
                    )
                    .to_string(),
                ));
            }
        }
        check_ratio(
            self.temperature,
            TEMPERATURE_MIN,
            TEMPERATURE_MAX,
            t!("llm.temperature", locale = language.id()).as_ref(),
            language,
        )?;
        check_ratio(
            self.top_p,
            TOP_P_MIN,
            TOP_P_MAX,
            t!("llm.top_p", locale = language.id()).as_ref(),
            language,
        )
    }
}

#[derive(Clone, PartialEq)]
pub struct LlmModelConfig {
    pub id: String,
    pub label: String,
    pub kind: ModelKind,
    pub provider: ModelProvider,
    pub model: String,
    pub endpoint: Option<String>,
    pub api_key: Option<String>,
    pub app_id: Option<String>,
    pub voice: Option<String>,
    pub local_path: Option<PathBuf>,
    pub use_gpu: bool,
    pub whisper_language: Option<String>,
    pub advanced: LlmAdvancedOptions,
}

impl LlmModelConfig {
    pub fn normalized(mut self, language: AppLanguage) -> Result<Self, AgentConfigError> {
        self.id = normalized_safe_id(
            &self.id,
            t!("llm.model_id", locale = language.id()).as_ref(),
            language,
        )?;
        self.label = normalized_required(
            &self.label,
            t!("llm.name", locale = language.id()).as_ref(),
            MAX_LABEL_BYTES,
            language,
        )?;
        if self.provider == ModelProvider::LocalWhisper {
            if self.kind != ModelKind::Transcription {
                return Err(invalid(
                    t!("llm.error.local_whisper_kind", locale = language.id()).to_string(),
                ));
            }
            self.model = "whisper".to_owned();
            self.local_path = normalize_local_model_path(self.local_path, language)?;
            if self.local_path.is_none() {
                return Err(invalid(
                    t!(
                        "llm.error.local_whisper_path_required",
                        locale = language.id()
                    )
                    .to_string(),
                ));
            }
            self.whisper_language = normalize_whisper_language(self.whisper_language, language)?;
        } else {
            self.model = normalized_required(
                &self.model,
                t!("llm.provider_model_id", locale = language.id()).as_ref(),
                MAX_MODEL_NAME_BYTES,
                language,
            )?;
            self.local_path = None;
            self.use_gpu = false;
            self.whisper_language = None;
        }
        self.api_key = normalize_optional(
            &self.api_key,
            MAX_API_KEY_BYTES,
            t!("llm.api_key", locale = language.id()).as_ref(),
            language,
        )?;
        self.app_id = normalize_optional(
            &self.app_id,
            MAX_API_KEY_BYTES,
            t!("llm.app_id", locale = language.id()).as_ref(),
            language,
        )?;
        self.voice = normalize_optional(
            &self.voice,
            MAX_MODEL_NAME_BYTES,
            t!("llm.voice_id", locale = language.id()).as_ref(),
            language,
        )?;
        match (self.kind, self.provider) {
            (ModelKind::ChatCompletions, ModelProvider::Genai(provider)) => {
                self.endpoint = normalize_endpoint(provider, self.endpoint.as_deref(), language)?;
                self.advanced.normalize(language)?;
                self.app_id = None;
                self.voice = None;
            }
            (ModelKind::ChatCompletions, _) => {
                return Err(invalid(
                    t!("llm.error.chat_provider", locale = language.id()).to_string(),
                ));
            }
            (ModelKind::SpeechSynthesis, ModelProvider::Genai(LlmProvider::OpenAI)) => {
                self.normalize_openai_speech(language)?;
                self.voice = Some(normalized_required(
                    self.voice.as_deref().unwrap_or_default(),
                    t!("llm.voice_id", locale = language.id()).as_ref(),
                    MAX_MODEL_NAME_BYTES,
                    language,
                )?);
                self.app_id = None;
                self.advanced = LlmAdvancedOptions::default();
            }
            (ModelKind::SpeechSynthesis, ModelProvider::Doubao) => {
                self.normalize_doubao(language, true)?;
            }
            (ModelKind::Transcription, ModelProvider::Genai(LlmProvider::OpenAI)) => {
                self.normalize_openai_speech(language)?;
                self.app_id = None;
                self.voice = None;
                self.advanced = LlmAdvancedOptions::default();
            }
            (ModelKind::Transcription, ModelProvider::Doubao) => {
                self.normalize_doubao(language, false)?;
            }
            (ModelKind::Transcription, ModelProvider::LocalWhisper) => {
                self.endpoint = None;
                self.api_key = None;
                self.app_id = None;
                self.voice = None;
                self.advanced = LlmAdvancedOptions::default();
            }
            (ModelKind::SpeechSynthesis | ModelKind::Transcription, ModelProvider::Genai(_)) => {
                return Err(invalid(
                    t!("llm.error.speech_provider", locale = language.id()).to_string(),
                ));
            }
            (ModelKind::SpeechSynthesis, ModelProvider::LocalWhisper) => {
                return Err(invalid(
                    t!("llm.error.local_whisper_tts", locale = language.id()).to_string(),
                ));
            }
        }
        Ok(self)
    }

    fn normalize_openai_speech(&mut self, language: AppLanguage) -> Result<(), AgentConfigError> {
        self.endpoint = normalize_speech_endpoint(self.endpoint.as_deref(), language)?;
        self.api_key = Some(normalized_required(
            self.api_key.as_deref().unwrap_or_default(),
            t!("llm.api_key", locale = language.id()).as_ref(),
            MAX_API_KEY_BYTES,
            language,
        )?);
        Ok(())
    }

    fn normalize_doubao(
        &mut self,
        language: AppLanguage,
        requires_voice: bool,
    ) -> Result<(), AgentConfigError> {
        self.endpoint = normalize_doubao_endpoint(self.endpoint.as_deref(), language)?;
        self.app_id = Some(normalized_required(
            self.app_id.as_deref().unwrap_or_default(),
            t!("llm.app_id", locale = language.id()).as_ref(),
            MAX_API_KEY_BYTES,
            language,
        )?);
        self.api_key = Some(normalized_required(
            self.api_key.as_deref().unwrap_or_default(),
            t!("llm.api_key", locale = language.id()).as_ref(),
            MAX_API_KEY_BYTES,
            language,
        )?);
        self.voice = if requires_voice {
            Some(normalized_required(
                self.voice.as_deref().unwrap_or_default(),
                t!("llm.voice_id", locale = language.id()).as_ref(),
                MAX_MODEL_NAME_BYTES,
                language,
            )?)
        } else {
            None
        };
        self.advanced = LlmAdvancedOptions::default();
        Ok(())
    }
}

impl fmt::Debug for LlmModelConfig {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("LlmModelConfig")
            .field("id", &self.id)
            .field("label", &self.label)
            .field("kind", &self.kind)
            .field("provider", &self.provider)
            .field("model", &self.model)
            .field("endpoint", &self.endpoint)
            .field("api_key", &self.api_key.as_ref().map(|_| "[REDACTED]"))
            .field("app_id", &self.app_id.as_ref().map(|_| "[REDACTED]"))
            .field("voice", &self.voice)
            .field("local_path", &self.local_path)
            .field("use_gpu", &self.use_gpu)
            .field("whisper_language", &self.whisper_language)
            .field("advanced", &self.advanced)
            .finish()
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct LlmSettings {
    pub models: Vec<LlmModelConfig>,
    pub selected_model: Option<String>,
    pub selected_transcription_model: Option<String>,
}

impl LlmSettings {
    pub fn selected(&self) -> Option<&LlmModelConfig> {
        let selected = self.selected_model.as_deref()?;
        self.models
            .iter()
            .find(|model| model.id == selected && model.kind == ModelKind::ChatCompletions)
    }

    /// 返回当前选中的 Transcription 模型；语音输入不再维护独立的模型选择。
    pub fn selected_transcription(&self) -> Option<&LlmModelConfig> {
        let selected = self.selected_transcription_model.as_deref()?;
        self.models
            .iter()
            .find(|model| model.id == selected && model.kind == ModelKind::Transcription)
    }

    /// 返回需要在模型列表中标记为当前使用项的稳定 ID。
    pub fn selected_model_id(&self, kind: ModelKind) -> Option<&str> {
        match kind {
            ModelKind::ChatCompletions => self.selected_model.as_deref(),
            ModelKind::Transcription => self.selected_transcription_model.as_deref(),
            ModelKind::SpeechSynthesis => None,
        }
    }

    pub fn model(&self, id: &str) -> Option<&LlmModelConfig> {
        self.models.iter().find(|model| model.id == id)
    }

    pub fn models_of_kind(&self, kind: ModelKind) -> impl Iterator<Item = &LlmModelConfig> {
        self.models.iter().filter(move |model| model.kind == kind)
    }

    pub fn normalized(mut self, language: AppLanguage) -> Result<Self, AgentConfigError> {
        if self.models.len() > MAX_MODELS {
            return Err(invalid(
                t!(
                    "llm.error.max_models",
                    locale = language.id(),
                    max = MAX_MODELS
                )
                .to_string(),
            ));
        }
        let mut ids = HashSet::with_capacity(self.models.len());
        for model in &mut self.models {
            *model = model.clone().normalized(language)?;
            if !ids.insert(model.id.clone()) {
                return Err(invalid(
                    t!(
                        "llm.error.duplicate_id",
                        locale = language.id(),
                        id = &model.id
                    )
                    .to_string(),
                ));
            }
        }
        self.selected_model = normalize_optional(
            &self.selected_model,
            MAX_ID_BYTES,
            t!("llm.selected", locale = language.id()).as_ref(),
            language,
        )?;
        if let Some(selected) = &self.selected_model
            && !self
                .models
                .iter()
                .any(|model| model.id == *selected && model.kind == ModelKind::ChatCompletions)
        {
            return Err(invalid(
                t!(
                    "llm.error.missing_selected",
                    locale = language.id(),
                    id = selected
                )
                .to_string(),
            ));
        }
        self.selected_transcription_model = normalize_optional(
            &self.selected_transcription_model,
            MAX_ID_BYTES,
            t!("llm.selected_transcription", locale = language.id()).as_ref(),
            language,
        )?;
        if let Some(selected) = &self.selected_transcription_model
            && !self
                .models
                .iter()
                .any(|model| model.id == *selected && model.kind == ModelKind::Transcription)
        {
            return Err(invalid(
                t!(
                    "llm.error.missing_selected_transcription",
                    locale = language.id(),
                    id = selected
                )
                .to_string(),
            ));
        }
        Ok(self)
    }
}

pub type SharedLlmSettings = Arc<LlmSettings>;

/// 规范化自定义 Provider endpoint；HTTP 与 HTTPS 均可用于本地或远端服务。
pub fn normalize_endpoint(
    provider: LlmProvider,
    endpoint: Option<&str>,
    language: AppLanguage,
) -> Result<Option<String>, AgentConfigError> {
    let Some(endpoint) = endpoint.map(str::trim).filter(|value| !value.is_empty()) else {
        return Ok(None);
    };
    if endpoint.len() > MAX_ENDPOINT_BYTES {
        return Err(invalid(
            t!(
                "llm.error.too_long",
                locale = language.id(),
                field = t!("llm.endpoint", locale = language.id()),
                max = MAX_ENDPOINT_BYTES
            )
            .to_string(),
        ));
    }
    if matches!(provider, LlmProvider::Zai | LlmProvider::Baidu) {
        let provider_name = match provider {
            LlmProvider::Zai => "ZAI",
            LlmProvider::Baidu => "Baidu",
            _ => llm_provider_id(provider),
        };
        return Err(invalid(
            t!(
                "llm.error.endpoint_unsupported",
                locale = language.id(),
                provider = provider_name
            )
            .to_string(),
        ));
    }
    let mut url = Url::parse(endpoint).map_err(|error| {
        invalid(
            t!(
                "llm.error.endpoint_invalid",
                locale = language.id(),
                error = error
            )
            .to_string(),
        )
    })?;
    if url.host().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err(invalid(
            t!("llm.error.endpoint_requirements", locale = language.id()).to_string(),
        ));
    }
    if !matches!(url.scheme(), "http" | "https") {
        return Err(invalid(
            t!("llm.error.endpoint_scheme", locale = language.id()).to_string(),
        ));
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

fn normalize_speech_endpoint(
    endpoint: Option<&str>,
    language: AppLanguage,
) -> Result<Option<String>, AgentConfigError> {
    normalize_endpoint(LlmProvider::OpenAI, endpoint, language)
}

fn normalize_doubao_endpoint(
    endpoint: Option<&str>,
    language: AppLanguage,
) -> Result<Option<String>, AgentConfigError> {
    let Some(endpoint) = endpoint.map(str::trim).filter(|value| !value.is_empty()) else {
        return Ok(None);
    };
    if endpoint.len() > MAX_ENDPOINT_BYTES {
        return Err(invalid(
            t!(
                "llm.error.too_long",
                locale = language.id(),
                field = t!("llm.endpoint", locale = language.id()),
                max = MAX_ENDPOINT_BYTES
            )
            .to_string(),
        ));
    }
    let url = Url::parse(endpoint).map_err(|error| {
        invalid(
            t!(
                "llm.error.endpoint_invalid",
                locale = language.id(),
                error = error
            )
            .to_string(),
        )
    })?;
    if url.host().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err(invalid(
            t!("llm.error.endpoint_requirements", locale = language.id()).to_string(),
        ));
    }
    if !matches!(url.scheme(), "ws" | "wss") {
        return Err(invalid(
            t!(
                "llm.error.websocket_endpoint_scheme",
                locale = language.id()
            )
            .to_string(),
        ));
    }
    Ok(Some(url.into()))
}

fn normalize_local_model_path(
    path: Option<PathBuf>,
    language: AppLanguage,
) -> Result<Option<PathBuf>, AgentConfigError> {
    let Some(path) = path else {
        return Ok(None);
    };
    let Some(value) = path.to_str() else {
        return Err(invalid(
            t!("llm.error.local_path_utf8", locale = language.id()).to_string(),
        ));
    };
    let value = value.trim();
    if value.is_empty() {
        return Ok(None);
    }
    if value.len() > MAX_LOCAL_MODEL_PATH_BYTES {
        return Err(invalid(
            t!(
                "llm.error.too_long",
                locale = language.id(),
                field = t!("llm.local_model_path", locale = language.id()),
                max = MAX_LOCAL_MODEL_PATH_BYTES
            )
            .to_string(),
        ));
    }
    if value.contains('\0') {
        return Err(invalid(
            t!("llm.error.local_path_invalid", locale = language.id()).to_string(),
        ));
    }
    Ok(Some(PathBuf::from(value)))
}

fn normalize_whisper_language(
    language_code: Option<String>,
    language: AppLanguage,
) -> Result<Option<String>, AgentConfigError> {
    let Some(language_code) = language_code else {
        return Ok(None);
    };
    let language_code = language_code.trim();
    if language_code.is_empty() {
        return Ok(None);
    }
    if !WHISPER_LANGUAGE_CODES.contains(&language_code) {
        return Err(invalid(
            t!(
                "llm.error.whisper_language",
                locale = language.id(),
                language = language_code
            )
            .to_string(),
        ));
    }
    Ok(Some(language_code.to_owned()))
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct PersonaContextLimits {
    pub max_messages: Option<u32>,
    pub max_tokens: Option<u32>,
}

impl PersonaContextLimits {
    pub fn effective_messages(self) -> u32 {
        self.max_messages.unwrap_or(DEFAULT_CONTEXT_MESSAGES)
    }

    pub fn effective_tokens(self) -> u32 {
        self.max_tokens.unwrap_or(DEFAULT_CONTEXT_TOKENS)
    }

    fn normalize(&mut self, language: AppLanguage) -> Result<(), AgentConfigError> {
        check_range(
            self.max_messages,
            CONTEXT_MESSAGES_MIN,
            CONTEXT_MESSAGES_MAX,
            t!("persona.context_messages", locale = language.id()).as_ref(),
            language,
        )?;
        check_range(
            self.max_tokens,
            CONTEXT_TOKENS_MIN,
            CONTEXT_TOKENS_MAX,
            t!("persona.context_tokens", locale = language.id()).as_ref(),
            language,
        )
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PersonaConfig {
    pub id: String,
    pub name: String,
    pub system_prompt: String,
    pub input_prompt: String,
    pub model: Option<String>,
    pub tts_model: Option<String>,
    pub live2d_model: Option<PathBuf>,
    pub context: PersonaContextLimits,
}

impl PersonaConfig {
    pub fn new(id: impl Into<String>, name: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            name: name.into(),
            system_prompt: String::new(),
            input_prompt: String::new(),
            model: None,
            tts_model: None,
            live2d_model: None,
            context: PersonaContextLimits::default(),
        }
    }

    pub fn normalized(mut self, language: AppLanguage) -> Result<Self, AgentConfigError> {
        self.id = normalize_persona_id(&self.id, language)?;
        self.name = normalized_required(
            &self.name,
            t!("persona.name", locale = language.id()).as_ref(),
            MAX_PERSONA_NAME_BYTES,
            language,
        )?;
        if self.system_prompt.len() > MAX_SYSTEM_PROMPT_BYTES {
            return Err(invalid(
                t!(
                    "llm.error.system_prompt_too_long",
                    locale = language.id(),
                    max = MAX_SYSTEM_PROMPT_BYTES
                )
                .to_string(),
            ));
        }
        if self.input_prompt.len() > MAX_SYSTEM_PROMPT_BYTES {
            return Err(invalid(
                t!(
                    "llm.error.too_long",
                    locale = language.id(),
                    field = t!("persona.input_prompt", locale = language.id()),
                    max = MAX_SYSTEM_PROMPT_BYTES
                )
                .to_string(),
            ));
        }
        self.model = self
            .model
            .as_deref()
            .map(str::trim)
            .filter(|model| !model.is_empty())
            .map(str::to_owned);
        if let Some(model) = &self.model
            && model.len() > MAX_ID_BYTES
        {
            return Err(invalid(
                t!(
                    "llm.error.too_long",
                    locale = language.id(),
                    field = t!("persona.provider", locale = language.id()),
                    max = MAX_ID_BYTES
                )
                .to_string(),
            ));
        }
        self.tts_model = normalize_optional(
            &self.tts_model,
            MAX_ID_BYTES,
            "Speech Synthesis 模型",
            language,
        )?;
        self.live2d_model = self
            .live2d_model
            .as_deref()
            .map(|path| validate_relative_path(path, language))
            .transpose()?;
        self.context.normalize(language)?;
        Ok(self)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PersonaSettings {
    pub personas: Vec<PersonaConfig>,
    pub selected: Option<String>,
    pub pending_deletions: Vec<String>,
}

impl Default for PersonaSettings {
    fn default() -> Self {
        Self::default_for(AppLanguage::default())
    }
}

impl PersonaSettings {
    /// 创建使用指定显示语言命名的默认人格。
    pub fn default_for(language: AppLanguage) -> Self {
        Self {
            personas: vec![PersonaConfig::new(
                DEFAULT_PERSONA_ID,
                t!("persona.default_name", locale = language.id()).to_string(),
            )],
            selected: Some(DEFAULT_PERSONA_ID.to_owned()),
            pending_deletions: Vec::new(),
        }
    }

    pub fn active(&self) -> Option<&PersonaConfig> {
        self.selected
            .as_deref()
            .and_then(|selected| self.personas.iter().find(|persona| persona.id == selected))
            .or_else(|| self.personas.first())
    }

    pub fn normalized(mut self, language: AppLanguage) -> Result<Self, AgentConfigError> {
        if self.personas.is_empty() {
            return Err(invalid(
                t!("persona.error.empty", locale = language.id()).to_string(),
            ));
        }
        let mut ids = HashSet::with_capacity(self.personas.len());
        for persona in &mut self.personas {
            *persona = persona.clone().normalized(language)?;
            if !ids.insert(persona.id.clone()) {
                return Err(invalid(
                    t!(
                        "persona.error.duplicate_id",
                        locale = language.id(),
                        id = &persona.id
                    )
                    .to_string(),
                ));
            }
        }
        self.selected = self
            .selected
            .as_deref()
            .map(str::trim)
            .filter(|selected| !selected.is_empty())
            .map(str::to_owned);
        if let Some(selected) = &self.selected
            && !ids.contains(selected)
        {
            return Err(invalid(
                t!(
                    "persona.error.missing_selected",
                    locale = language.id(),
                    id = selected
                )
                .to_string(),
            ));
        }
        let mut pending = HashSet::with_capacity(self.pending_deletions.len());
        let mut normalized = Vec::with_capacity(self.pending_deletions.len());
        for id in self.pending_deletions {
            let id = normalize_persona_id(&id, language)?;
            if ids.contains(&id) {
                return Err(invalid(
                    t!(
                        "persona.error.pending_deletion_conflict",
                        locale = language.id(),
                        id = id
                    )
                    .to_string(),
                ));
            }
            if pending.insert(id.clone()) {
                normalized.push(id);
            }
        }
        self.pending_deletions = normalized;
        Ok(self)
    }
}

pub type SharedPersonaSettings = Arc<PersonaSettings>;

/// 宿主在一个 generation 上发布给 Agent 的完整不可变配置。
#[derive(Clone)]
pub struct AgentConfigSnapshot {
    generation: u64,
    settings: SharedLlmSettings,
    personas: SharedPersonaSettings,
    language: AppLanguage,
}

impl AgentConfigSnapshot {
    /// 规范化并校验 Provider 与人格配置后创建不可变快照。
    ///
    /// # Errors
    ///
    /// 任一配置域不满足当前格式约束时返回错误。
    pub fn try_new(
        generation: u64,
        settings: SharedLlmSettings,
        personas: SharedPersonaSettings,
        language: AppLanguage,
    ) -> Result<Self, AgentConfigError> {
        let settings = Arc::new(settings.as_ref().clone().normalized(language)?);
        let personas = Arc::new(personas.as_ref().clone().normalized(language)?);
        for persona in &personas.personas {
            if let Some(model) = persona.model.as_deref()
                && !settings.models.iter().any(|candidate| {
                    candidate.id == model && candidate.kind == ModelKind::ChatCompletions
                })
            {
                return Err(invalid(
                    t!(
                        "llm.error.persona_model_missing",
                        locale = language.id(),
                        persona = &persona.id,
                        capability = "Chat Completions"
                    )
                    .to_string(),
                ));
            }
            if let Some(model) = persona.tts_model.as_deref()
                && !settings.models.iter().any(|candidate| {
                    candidate.id == model && candidate.kind == ModelKind::SpeechSynthesis
                })
            {
                return Err(invalid(
                    t!(
                        "llm.error.persona_model_missing",
                        locale = language.id(),
                        persona = &persona.id,
                        capability = "Speech Synthesis"
                    )
                    .to_string(),
                ));
            }
        }
        Ok(Self {
            generation,
            settings,
            personas,
            language,
        })
    }

    pub const fn generation(&self) -> u64 {
        self.generation
    }

    pub fn settings(&self) -> &SharedLlmSettings {
        &self.settings
    }

    pub fn personas(&self) -> &SharedPersonaSettings {
        &self.personas
    }

    pub const fn language(&self) -> AppLanguage {
        self.language
    }
}

pub fn normalize_persona_id(id: &str, language: AppLanguage) -> Result<String, AgentConfigError> {
    normalized_safe_id(
        id,
        t!("persona.id", locale = language.id()).as_ref(),
        language,
    )
}

fn normalized_safe_id(
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

fn normalized_required(
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

fn normalize_optional(
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

fn check_range(
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

fn check_ratio(
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

fn validate_relative_path(path: &Path, language: AppLanguage) -> Result<PathBuf, AgentConfigError> {
    if path.as_os_str().is_empty()
        || path.to_str().is_none()
        || path
            .components()
            .any(|component| !matches!(component, Component::Normal(_)))
    {
        return Err(invalid(
            t!(
                "persona.error.live2d_path",
                locale = language.id(),
                path = path.display()
            )
            .to_string(),
        ));
    }
    Ok(path.to_path_buf())
}

fn invalid(message: impl Into<String>) -> AgentConfigError {
    AgentConfigError::new(message)
}
