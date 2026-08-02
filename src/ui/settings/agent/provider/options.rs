//! 维护供应商、语言和高级参数选择项与配置值之间的映射。

use gpui::{Context, Entity, SharedString, Window};
use gpui_component::{
    IndexPath,
    input::{InputState, MaskPattern},
    select::SelectState,
};
use lunamate_agent::config::{
    DEFAULT_MAX_OUTPUT_TOKENS, DEFAULT_MODEL_CONTEXT_TOKENS, DEFAULT_REASONING_BUDGET,
    DEFAULT_TEMPERATURE, DEFAULT_TOP_P, LLM_PROVIDERS, LlmAdvancedOptions, LlmProvider, ModelKind,
    ModelProvider, REASONING_EFFORT_LEVELS, ReasoningEffort, TEMPERATURE_MAX, TEMPERATURE_MIN,
    TOP_P_MAX, TOP_P_MIN, WHISPER_LANGUAGE_CODES, WHISPER_LANGUAGE_NAMES, reasoning_budget,
};
use rust_i18n::t;

use super::{
    super::{provider_display_name, set_input},
    ProviderSettingsView,
};

/// 思考强度选择器第一项表示"沿用 Provider 默认值"，最后一项表示自定义 token 预算。
const REASONING_AUTO_INDEX: usize = 0;
pub(super) const REASONING_BUDGET_INDEX: usize = REASONING_EFFORT_LEVELS.len() + 1;
const REASONING_OPTION_COUNT: usize = REASONING_EFFORT_LEVELS.len() + 2;

impl ProviderSettingsView {
    pub(super) fn toggle_advanced(&mut self, cx: &mut Context<Self>) {
        self.advanced_expanded = !self.advanced_expanded;
        cx.notify();
    }

    pub(super) fn toggle_max_output_tokens(&mut self, cx: &mut Context<Self>) {
        self.max_output_tokens_enabled = !self.max_output_tokens_enabled;
        self.save(cx);
    }

    pub(super) fn toggle_context_window_tokens(&mut self, cx: &mut Context<Self>) {
        self.context_window_tokens_enabled = !self.context_window_tokens_enabled;
        self.save(cx);
    }

    pub(super) fn toggle_temperature(&mut self, cx: &mut Context<Self>) {
        self.temperature_enabled = !self.temperature_enabled;
        self.save(cx);
    }

    pub(super) fn toggle_top_p(&mut self, cx: &mut Context<Self>) {
        self.top_p_enabled = !self.top_p_enabled;
        self.save(cx);
    }

    pub(super) fn toggle_use_gpu(&mut self, cx: &mut Context<Self>) {
        self.use_gpu = !self.use_gpu;
        self.save(cx);
    }

    /// 未启用的高级参数一律回落为 `None`，界面里的预填值只是建议值。
    pub(super) fn capture_advanced(&self, cx: &Context<Self>) -> LlmAdvancedOptions {
        let index = selected_reasoning_index(&self.reasoning_select, cx);
        let reasoning_effort = if index == REASONING_AUTO_INDEX {
            None
        } else if index == REASONING_BUDGET_INDEX {
            Some(ReasoningEffort::Budget(
                parse_u32(self.reasoning_budget_input.read(cx).value().as_ref())
                    .unwrap_or(DEFAULT_REASONING_BUDGET),
            ))
        } else {
            REASONING_EFFORT_LEVELS.get(index - 1).cloned()
        };

        LlmAdvancedOptions {
            context_window_tokens: self
                .context_window_tokens_enabled
                .then(|| parse_u32(self.context_window_tokens_input.read(cx).value().as_ref()))
                .flatten(),
            reasoning_effort,
            max_output_tokens: self
                .max_output_tokens_enabled
                .then(|| parse_u32(self.max_output_tokens_input.read(cx).value().as_ref()))
                .flatten(),
            temperature: self
                .temperature_enabled
                .then(|| parse_ratio(self.temperature_input.read(cx).value().as_ref()))
                .flatten(),
            top_p: self
                .top_p_enabled
                .then(|| parse_ratio(self.top_p_input.read(cx).value().as_ref()))
                .flatten(),
        }
    }

    pub(super) fn load_advanced_form(
        &mut self,
        advanced: LlmAdvancedOptions,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let index = reasoning_index(advanced.reasoning_effort.as_ref());
        self.reasoning_select.update(cx, |select, cx| {
            select.set_selected_index(Some(IndexPath::new(index)), window, cx);
        });
        set_input(
            &self.reasoning_budget_input,
            &advanced
                .reasoning_effort
                .as_ref()
                .and_then(reasoning_budget)
                .unwrap_or(DEFAULT_REASONING_BUDGET)
                .to_string(),
            window,
            cx,
        );
        set_input(
            &self.context_window_tokens_input,
            &advanced
                .context_window_tokens
                .unwrap_or(DEFAULT_MODEL_CONTEXT_TOKENS)
                .to_string(),
            window,
            cx,
        );
        set_input(
            &self.max_output_tokens_input,
            &advanced
                .max_output_tokens
                .unwrap_or(DEFAULT_MAX_OUTPUT_TOKENS)
                .to_string(),
            window,
            cx,
        );
        set_input(
            &self.temperature_input,
            &format_ratio(advanced.temperature.unwrap_or(DEFAULT_TEMPERATURE)),
            window,
            cx,
        );
        set_input(
            &self.top_p_input,
            &format_ratio(advanced.top_p.unwrap_or(DEFAULT_TOP_P)),
            window,
            cx,
        );
        self.context_window_tokens_enabled = advanced.context_window_tokens.is_some();
        self.max_output_tokens_enabled = advanced.max_output_tokens.is_some();
        self.temperature_enabled = advanced.temperature.is_some();
        self.top_p_enabled = advanced.top_p.is_some();
    }

    /// 返回指定草稿条目的高级参数，供测试断言表单往返一致。
    #[cfg(test)]
    pub(crate) fn advanced_options_for_test(&self, index: usize) -> Option<LlmAdvancedOptions> {
        self.draft
            .models
            .get(index)
            .map(|model| model.advanced.clone())
    }

    /// 切换某个高级参数是否随请求发送。
    #[cfg(test)]
    pub(crate) fn set_advanced_enabled_for_test(
        &mut self,
        max_output_tokens: bool,
        temperature: bool,
        top_p: bool,
    ) {
        self.max_output_tokens_enabled = max_output_tokens;
        self.temperature_enabled = temperature;
        self.top_p_enabled = top_p;
    }

    /// 切换本地模型上下文窗口是否生效。
    #[cfg(test)]
    pub(crate) fn set_context_window_enabled_for_test(&mut self, enabled: bool) {
        self.context_window_tokens_enabled = enabled;
    }
}

pub(super) fn integer_input(
    window: &mut Window,
    cx: &mut Context<InputState>,
    min: u32,
    max: u32,
) -> InputState {
    InputState::new(window, cx)
        .mask_pattern(MaskPattern::Number {
            separator: None,
            fraction: Some(0),
        })
        .step(1.0)
        .min(f64::from(min))
        .max(f64::from(max))
}

pub(super) fn reasoning_option_names() -> Vec<SharedString> {
    let mut names = Vec::with_capacity(REASONING_OPTION_COUNT);
    names.push(SharedString::from(t!("llm.reasoning_auto").to_string()));
    for level in &REASONING_EFFORT_LEVELS {
        names.push(SharedString::from(reasoning_level_name(level)));
    }
    names.push(SharedString::from(
        t!("llm.reasoning_budget_option").to_string(),
    ));
    names
}

fn reasoning_level_name(level: &ReasoningEffort) -> String {
    match level {
        ReasoningEffort::None => t!("llm.reasoning_off").to_string(),
        ReasoningEffort::Minimal => t!("llm.reasoning_minimal").to_string(),
        ReasoningEffort::Low => t!("llm.reasoning_low").to_string(),
        ReasoningEffort::Medium => t!("llm.reasoning_medium").to_string(),
        ReasoningEffort::High => t!("llm.reasoning_high").to_string(),
        ReasoningEffort::XHigh => t!("llm.reasoning_xhigh").to_string(),
        ReasoningEffort::Max => t!("llm.reasoning_max").to_string(),
        ReasoningEffort::Budget(_) => t!("llm.reasoning_budget_option").to_string(),
    }
}

pub(super) fn reasoning_index(effort: Option<&ReasoningEffort>) -> usize {
    match effort {
        None => REASONING_AUTO_INDEX,
        Some(ReasoningEffort::Budget(_)) => REASONING_BUDGET_INDEX,
        Some(effort) => REASONING_EFFORT_LEVELS
            .iter()
            .position(|level| level.variant_name() == effort.variant_name())
            .map_or(REASONING_AUTO_INDEX, |index| index + 1),
    }
}

pub(super) fn selected_reasoning_index(
    select: &Entity<SelectState<Vec<SharedString>>>,
    cx: &Context<ProviderSettingsView>,
) -> usize {
    select
        .read(cx)
        .selected_index(cx)
        .map_or(REASONING_AUTO_INDEX, |index| index.row)
}

pub(super) fn whisper_language_options() -> Vec<SharedString> {
    let mut names = Vec::with_capacity(WHISPER_LANGUAGE_CODES.len() + 1);
    names.push(SharedString::from(
        t!("llm.whisper_language_default").to_string(),
    ));
    names.extend(
        WHISPER_LANGUAGE_CODES
            .iter()
            .zip(WHISPER_LANGUAGE_NAMES)
            .map(|(code, name)| SharedString::from(format!("{name} ({code})"))),
    );
    names
}

pub(super) fn whisper_language_index(language: Option<&str>) -> usize {
    language
        .and_then(|language| {
            WHISPER_LANGUAGE_CODES
                .iter()
                .position(|candidate| *candidate == language)
        })
        .map_or(0, |index| index + 1)
}

pub(super) fn selected_whisper_language(
    select: &Entity<SelectState<Vec<SharedString>>>,
    cx: &Context<ProviderSettingsView>,
) -> Option<String> {
    let row = select
        .read(cx)
        .selected_index(cx)
        .map_or(0, |index| index.row);
    row.checked_sub(1)
        .and_then(|index| WHISPER_LANGUAGE_CODES.get(index))
        .map(|language| (*language).to_owned())
}

fn parse_u32(value: &str) -> Option<u32> {
    let value = value.trim();
    (!value.is_empty()).then(|| value.parse().ok()).flatten()
}

/// 表单里的比率参数用浮点文本表示；越界值直接夹到合法区间，避免整份草稿被拒绝。
fn parse_ratio(value: &str) -> Option<f64> {
    let value = value.trim();
    value
        .parse::<f64>()
        .ok()
        .filter(|value| value.is_finite())
        .map(|value| {
            value.clamp(
                TEMPERATURE_MIN.min(TOP_P_MIN),
                TEMPERATURE_MAX.max(TOP_P_MAX),
            )
        })
}

pub(super) fn format_ratio(value: f64) -> String {
    format!("{value}")
}

pub(super) fn model_provider_options(kind: ModelKind) -> Vec<ModelProvider> {
    match kind {
        ModelKind::ChatCompletions => LLM_PROVIDERS
            .into_iter()
            .map(ModelProvider::Genai)
            .collect(),
        ModelKind::SpeechSynthesis => vec![
            ModelProvider::Genai(LlmProvider::OpenAI),
            ModelProvider::Doubao,
        ],
        ModelKind::Transcription => vec![
            ModelProvider::Genai(LlmProvider::OpenAI),
            ModelProvider::Doubao,
            ModelProvider::LocalWhisper,
        ],
    }
}

pub(super) fn default_provider(kind: ModelKind) -> ModelProvider {
    match kind {
        ModelKind::ChatCompletions => ModelProvider::Genai(LlmProvider::Ollama),
        ModelKind::SpeechSynthesis | ModelKind::Transcription => {
            ModelProvider::Genai(LlmProvider::OpenAI)
        }
    }
}

pub(super) fn model_provider_from_display_name(
    kind: ModelKind,
    name: &str,
) -> Option<ModelProvider> {
    model_provider_options(kind)
        .into_iter()
        .find(|provider| provider_display_name(*provider) == name)
}

/// 暴露展示名到 Provider 的反向映射，供测试断言选择器往返一致。
#[cfg(test)]
pub(crate) fn provider_from_display_name_for_test(name: &str) -> Option<LlmProvider> {
    LLM_PROVIDERS
        .into_iter()
        .find(|provider| provider_display_name(*provider) == name)
}

/// 暴露思考强度档位与选择项索引的双向映射，供测试断言往返一致。
#[cfg(test)]
pub(crate) fn reasoning_index_for_test(effort: Option<ReasoningEffort>) -> usize {
    reasoning_index(effort.as_ref())
}

/// 暴露思考强度选择项总数，供测试断言选择器覆盖全部档位。
#[cfg(test)]
pub(crate) const fn reasoning_option_count_for_test() -> usize {
    REASONING_OPTION_COUNT
}
