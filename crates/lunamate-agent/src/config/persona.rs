//! 定义人格配置、上下文限制与人格-模型关系校验。

use std::{
    collections::HashSet,
    path::{Component, Path, PathBuf},
    sync::Arc,
};

use rust_i18n::t;

use super::{
    AgentConfigError, AppLanguage, LlmSettings, MAX_ID_BYTES, MAX_SYSTEM_PROMPT_BYTES, ModelKind,
    invalid, normalize_optional, normalized_required, normalized_safe_id,
};

const MAX_PERSONA_NAME_BYTES: usize = 128;

pub const DEFAULT_PERSONA_ID: &str = "default";
pub const CONTEXT_MESSAGES_MIN: u32 = 2;
pub const CONTEXT_MESSAGES_MAX: u32 = 512;
pub const CONTEXT_TOKENS_MIN: u32 = 256;
pub const CONTEXT_TOKENS_MAX: u32 = 1_050_624;
pub const DEFAULT_CONTEXT_MESSAGES: u32 = 64;
pub const DEFAULT_CONTEXT_TOKENS: u32 = 65_792;

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
        super::check_range(
            self.max_messages,
            CONTEXT_MESSAGES_MIN,
            CONTEXT_MESSAGES_MAX,
            t!("persona.context_messages", locale = language.id()).as_ref(),
            language,
        )?;
        super::check_range(
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

/// 校验人格绑定的模型存在且具有相应能力。
///
/// 该关系由 Agent 配置领域定义，但配置发布时机和 generation 由宿主负责。
pub fn validate_persona_model_bindings(
    settings: &LlmSettings,
    personas: &PersonaSettings,
    language: AppLanguage,
) -> Result<(), AgentConfigError> {
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
    Ok(())
}

pub fn normalize_persona_id(id: &str, language: AppLanguage) -> Result<String, AgentConfigError> {
    normalized_safe_id(
        id,
        t!("persona.id", locale = language.id()).as_ref(),
        language,
    )
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
