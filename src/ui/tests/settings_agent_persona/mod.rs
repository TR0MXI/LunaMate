//! 在无头 GPUI TestAppContext 中验证 Agent 人格设置编辑器的草稿与危险操作确认流程。
//!
//! 记忆的实际读写需要嵌入式数据库；这里使用不可用的记忆句柄，只覆盖草稿状态、
//! 供应商绑定映射与"删除必须先确认"的约束。真实删除路径在数据库层单独验证。

use gpui::{
    Entity, Modifiers, MouseButton, ScrollDelta, ScrollWheelEvent, TestAppContext, TouchPhase,
    VisualTestContext, point, prelude::*, px,
};
use lunamate_agent::AgentMemory;
use lunamate_agent::config::{
    LlmAdvancedOptions, LlmModelConfig, LlmProvider, LlmSettings, ModelKind, ModelProvider,
    PersonaConfig, PersonaContextLimits, PersonaSettings,
};
use lunamate_agent::{
    ChatRole,
    memory::{ContextMessage, ContextUsage, LiveContextUsage},
};
use std::{path::PathBuf, sync::Arc};

use crate::ui::settings::{
    MemoryScope, PersonaSettingsDraft, PersonaSettingsView, next_persona_id_for_test,
    provider_option_index_for_test, tts_model_option_index_for_test,
};

mod context;
mod draft;
mod memory;
mod render;

fn provider(id: &str) -> LlmModelConfig {
    LlmModelConfig {
        id: id.to_owned(),
        label: format!("Provider {id}"),
        kind: ModelKind::ChatCompletions,
        provider: ModelProvider::Genai(LlmProvider::Ollama),
        model: "qwen3:8b".to_owned(),
        endpoint: Some("http://localhost:11434/".to_owned()),
        api_key: None,
        voice: None,
        voice_type: None,
        local_path: None,
        use_gpu: false,
        whisper_language: None,
        advanced: LlmAdvancedOptions::default(),
    }
}

fn tts_model(id: &str) -> LlmModelConfig {
    LlmModelConfig {
        id: id.to_owned(),
        label: format!("TTS {id}"),
        kind: ModelKind::SpeechSynthesis,
        provider: ModelProvider::Genai(LlmProvider::OpenAI),
        model: "gpt-4o-mini-tts".to_owned(),
        endpoint: None,
        api_key: Some("test-key".to_owned()),
        voice: Some("alloy".to_owned()),
        voice_type: None,
        local_path: None,
        use_gpu: false,
        whisper_language: None,
        advanced: LlmAdvancedOptions::default(),
    }
}

fn persona(id: &str, bound: Option<&str>) -> PersonaConfig {
    let mut persona = PersonaConfig::new(id, format!("人格 {id}"));
    persona.model = bound.map(str::to_owned);
    persona
}

fn mount(
    cx: &mut TestAppContext,
    providers: LlmSettings,
    personas: PersonaSettings,
) -> (Entity<PersonaSettingsView>, &mut VisualTestContext) {
    mount_with_models(cx, providers, personas, Vec::new())
}

fn mount_with_models(
    cx: &mut TestAppContext,
    providers: LlmSettings,
    personas: PersonaSettings,
    models: Vec<(String, PathBuf)>,
) -> (Entity<PersonaSettingsView>, &mut VisualTestContext) {
    cx.update(|cx| {
        gpui_component::init(cx);
        gpui_tokio::init(cx);
    });
    let memory = AgentMemory::unavailable();
    let draft = PersonaSettingsDraft::from_settings_for_test(personas);
    // 数据库不可用的句柄让统计立即失败，避免测试依赖测试线程之外的唤醒。
    cx.add_window_view(|window, cx| {
        PersonaSettingsView::new_for_test(draft, memory, Arc::new(providers), models, window, cx)
    })
}
