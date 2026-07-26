//! 组合供应商与人格两个设置编辑器，并共享它们的表单基元与发布事件。

mod components;
mod persona;
mod persona_render;
mod provider;
mod provider_render;

use gpui::{Context, Entity, Window};
use gpui_component::input::InputState;

use crate::config::LlmProvider;

pub(crate) use persona::{PersonaSettingsDraft, PersonaSettingsEvent, PersonaSettingsView};
pub(crate) use provider::{AgentSettingsDraft, AgentSettingsEvent, AgentSettingsView};

/// 返回供应商图标资源路径；文件名使用与配置一致的稳定 Provider ID。
pub(super) fn provider_icon(provider: LlmProvider) -> String {
    format!("icons/providers/{}.svg", provider.id())
}

/// 表单可选字段的空白归一化规则：仅空白等同于未设置。
fn non_empty(value: &str) -> Option<String> {
    let value = value.trim();
    (!value.is_empty()).then(|| value.to_owned())
}

fn set_input<V: 'static>(
    input: &Entity<InputState>,
    value: &str,
    window: &mut Window,
    cx: &mut Context<V>,
) {
    input.update(cx, |input, cx| input.set_value(value, window, cx));
}

/// 暴露表单可选字段的空白归一化规则，供测试断言"仅空白等同未设置"。
#[cfg(test)]
pub(in crate::agent) fn non_empty_for_test(value: &str) -> Option<String> {
    non_empty(value)
}

/// 暴露供应商图标路径规则，供测试断言每个 Provider 都有对应资源。
#[cfg(test)]
pub(in crate::agent) fn provider_icon_for_test(provider: LlmProvider) -> String {
    provider_icon(provider)
}

pub(super) const fn provider_display_name(provider: LlmProvider) -> &'static str {
    match provider {
        LlmProvider::OpenAi => "OpenAI",
        LlmProvider::OpenAiResponses => "OpenAI Responses",
        LlmProvider::Gemini => "Gemini",
        LlmProvider::Anthropic => "Anthropic",
        LlmProvider::Fireworks => "Fireworks",
        LlmProvider::Together => "Together",
        LlmProvider::Groq => "Groq",
        LlmProvider::Aihubmix => "AIHubMix",
        LlmProvider::Mimo => "Mimo",
        LlmProvider::Moonshot => "Moonshot",
        LlmProvider::Nebius => "Nebius",
        LlmProvider::Xai => "xAI",
        LlmProvider::DeepSeek => "DeepSeek",
        LlmProvider::Zai => "ZAI",
        LlmProvider::BigModel => "BigModel",
        LlmProvider::Aliyun => "Aliyun",
        LlmProvider::Baidu => "Baidu",
        LlmProvider::Cohere => "Cohere",
        LlmProvider::Ollama => "Ollama",
        LlmProvider::OllamaCloud => "Ollama Cloud",
        LlmProvider::Vertex => "Google Vertex",
        LlmProvider::GithubModels => "GitHub Models",
        LlmProvider::OpenCodeGo => "OpenCode Go",
        LlmProvider::BedrockApi => "Bedrock API Key",
        LlmProvider::OpenRouter => "OpenRouter",
        LlmProvider::Minimax => "MiniMax",
    }
}

/// 暴露 Provider 展示名，供测试断言目录内名称唯一。
#[cfg(test)]
pub(in crate::agent) const fn provider_display_name_for_test(
    provider: LlmProvider,
) -> &'static str {
    provider_display_name(provider)
}

#[cfg(test)]
pub(in crate::agent) use persona::{next_persona_id_for_test, provider_option_index_for_test};
#[cfg(test)]
pub(in crate::agent) use provider::{
    next_model_id_for_test, provider_from_display_name_for_test, reasoning_index_for_test,
    reasoning_option_count_for_test,
};
