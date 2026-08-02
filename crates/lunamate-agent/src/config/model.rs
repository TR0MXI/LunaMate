//! 定义模型条目、高级调用参数和模型目录校验。

use std::{collections::HashSet, fmt, path::PathBuf, sync::Arc};

use rust_i18n::t;

use super::{
    AgentConfigError, AppLanguage, LlmProvider, MAX_ID_BYTES, MAX_LABEL_BYTES, ModelKind,
    ModelProvider, ReasoningEffort, check_range, check_ratio,
    endpoint::{
        normalize_doubao_endpoint, normalize_local_model_path, normalize_speech_endpoint,
        normalize_whisper_language,
    },
    invalid, normalize_endpoint, normalize_optional, normalized_required, normalized_safe_id,
};

const MAX_MODELS: usize = 64;
const MAX_MODEL_NAME_BYTES: usize = 256;
const MAX_API_KEY_BYTES: usize = 4 * 1024;

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
    pub voice: Option<String>,
    pub voice_type: Option<String>,
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
        self.voice = normalize_optional(
            &self.voice,
            MAX_MODEL_NAME_BYTES,
            t!("llm.voice", locale = language.id()).as_ref(),
            language,
        )?;
        self.voice_type = normalize_optional(
            &self.voice_type,
            MAX_MODEL_NAME_BYTES,
            t!("llm.voice_type", locale = language.id()).as_ref(),
            language,
        )?;
        match (self.kind, self.provider) {
            (ModelKind::ChatCompletions, ModelProvider::Genai(provider)) => {
                self.endpoint = normalize_endpoint(provider, self.endpoint.as_deref(), language)?;
                self.advanced.normalize(language)?;
                self.voice = None;
                self.voice_type = None;
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
                    t!("llm.voice", locale = language.id()).as_ref(),
                    MAX_MODEL_NAME_BYTES,
                    language,
                )?);
                self.voice_type = None;
                self.advanced = LlmAdvancedOptions::default();
            }
            (ModelKind::SpeechSynthesis, ModelProvider::Doubao) => {
                self.normalize_doubao(language, true)?;
            }
            (ModelKind::Transcription, ModelProvider::Genai(LlmProvider::OpenAI)) => {
                self.normalize_openai_speech(language)?;
                self.voice = None;
                self.voice_type = None;
                self.advanced = LlmAdvancedOptions::default();
            }
            (ModelKind::Transcription, ModelProvider::Doubao) => {
                self.normalize_doubao(language, false)?;
            }
            (ModelKind::Transcription, ModelProvider::LocalWhisper) => {
                self.endpoint = None;
                self.api_key = None;
                self.voice = None;
                self.voice_type = None;
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
        self.api_key = Some(normalized_required(
            self.api_key.as_deref().unwrap_or_default(),
            t!("llm.api_key", locale = language.id()).as_ref(),
            MAX_API_KEY_BYTES,
            language,
        )?);
        self.voice = None;
        self.voice_type = if requires_voice {
            Some(normalized_required(
                self.voice_type.as_deref().unwrap_or_default(),
                t!("llm.voice_type", locale = language.id()).as_ref(),
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
            .field("voice", &self.voice)
            .field("voice_type", &self.voice_type)
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
