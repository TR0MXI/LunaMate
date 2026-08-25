//! 定义宿主发布给运行时和设置界面的完整 Agent 配置快照。

use std::sync::Arc;

use lunamate_agent::config::{
    AgentConfigError, AppLanguage, SharedLlmSettings, SharedPersonaSettings,
    validate_persona_model_bindings,
};

/// 一个 generation 上已规范化、已校验且不可变的 Agent 配置发布单元。
#[derive(Clone)]
pub struct AgentConfigSnapshot {
    generation: u64,
    settings: SharedLlmSettings,
    personas: SharedPersonaSettings,
    language: AppLanguage,
}

impl AgentConfigSnapshot {
    /// 规范化配置域并校验其交叉引用后创建新的发布单元。
    pub fn try_new(
        generation: u64,
        settings: SharedLlmSettings,
        personas: SharedPersonaSettings,
        language: AppLanguage,
    ) -> Result<Self, AgentConfigError> {
        let settings = Arc::new(settings.as_ref().clone().normalized(language)?);
        let personas = Arc::new(personas.as_ref().clone().normalized(language)?);
        validate_persona_model_bindings(&settings, &personas, language)?;
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
