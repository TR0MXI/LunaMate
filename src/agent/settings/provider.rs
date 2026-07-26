//! 保存供应商设置草稿，处理条目编辑动作并发布配置变更。

use std::{sync::Arc, time::Duration};

use gpui::{AppContext, Context, Entity, EventEmitter, SharedString, Task, Window};
use gpui_component::{IndexPath, input::InputState, select::SelectState};
use rust_i18n::t;

use crate::config::{
    CONFIG, DEFAULT_MAX_OUTPUT_TOKENS, DEFAULT_REASONING_BUDGET, DEFAULT_TEMPERATURE,
    DEFAULT_TOP_P, LLM_PROVIDERS, LlmAdvancedOptions, LlmModelConfig, LlmProvider, LlmSettings,
    MAX_OUTPUT_TOKENS_MAX, MAX_OUTPUT_TOKENS_MIN, REASONING_BUDGET_MAX, REASONING_BUDGET_MIN,
    REASONING_EFFORT_LEVELS, ReasoningEffort, SharedLlmSettings, TEMPERATURE_MAX, TEMPERATURE_MIN,
    TOP_P_MAX, TOP_P_MIN,
};

use super::{non_empty, provider_display_name, set_input};

/// 思考强度选择器第一项表示"沿用 Provider 默认值"，最后一项表示自定义 token 预算。
const REASONING_AUTO_INDEX: usize = 0;
const REASONING_BUDGET_INDEX: usize = REASONING_EFFORT_LEVELS.len() + 1;
const REASONING_OPTION_COUNT: usize = REASONING_EFFORT_LEVELS.len() + 2;

/// 设置窗口重建时保留的供应商草稿，不向 UI 暴露 Provider 配置类型。
#[derive(Clone)]
pub(crate) struct AgentSettingsDraft(SharedLlmSettings);

impl AgentSettingsDraft {
    /// 从当前已发布配置创建设置窗口草稿。
    pub(crate) fn current() -> Self {
        Self(CONFIG.llm_settings())
    }
}

/// 供应商设置成功发布后通知设置窗口和桌宠视图。
#[derive(Clone, Copy, Debug)]
pub(crate) struct AgentSettingsEvent;

/// 设置窗口中的供应商编辑器。
pub(crate) struct AgentSettingsView {
    draft: LlmSettings,
    editing_index: Option<usize>,
    label_input: Entity<InputState>,
    model_input: Entity<InputState>,
    endpoint_input: Entity<InputState>,
    api_key_input: Entity<InputState>,
    provider_select: Entity<SelectState<Vec<SharedString>>>,
    reasoning_select: Entity<SelectState<Vec<SharedString>>>,
    reasoning_budget_input: Entity<InputState>,
    max_output_tokens_input: Entity<InputState>,
    temperature_input: Entity<InputState>,
    top_p_input: Entity<InputState>,
    max_output_tokens_enabled: bool,
    temperature_enabled: bool,
    top_p_enabled: bool,
    pub(super) advanced_expanded: bool,
    status: Option<String>,
    is_saving: bool,
    toast_revision: u64,
    toast_task: Option<Task<()>>,
    write_tasks: Vec<Task<()>>,
}

impl AgentSettingsView {
    /// 从当前运行时配置创建可丢弃的设置草稿。
    pub(crate) fn new(
        draft: AgentSettingsDraft,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        // 设置实体可能随窗口一起释放；已提交的写任务不能因为句柄销毁而被取消。
        cx.on_release(|this, _| {
            for task in std::mem::take(&mut this.write_tasks) {
                task.detach();
            }
        })
        .detach();
        let draft = draft.0.as_ref().clone();
        let editing_index = draft
            .selected_model
            .as_deref()
            .and_then(|selected| draft.models.iter().position(|model| model.id == selected))
            .or_else(|| (!draft.models.is_empty()).then_some(0));
        let editing_model = editing_index.and_then(|index| draft.models.get(index));
        let provider = editing_model
            .map(|model| model.provider)
            .unwrap_or(LlmProvider::Ollama);
        let advanced = editing_model
            .map(|model| model.advanced)
            .unwrap_or_default();
        let provider_names = LLM_PROVIDERS
            .into_iter()
            .map(|provider| SharedString::from(provider_display_name(provider)))
            .collect::<Vec<_>>();
        let provider_index = LLM_PROVIDERS
            .iter()
            .position(|candidate| *candidate == provider)
            .map(IndexPath::new);

        let label_input = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder(t!("llm.model_name_placeholder").to_string())
                .default_value(
                    editing_model
                        .map(|model| model.label.as_str())
                        .unwrap_or_default(),
                )
        });
        let model_input = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder(t!("llm.provider_model_id").to_string())
                .default_value(
                    editing_model
                        .map(|model| model.model.as_str())
                        .unwrap_or_default(),
                )
        });
        let endpoint_input = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder(t!("llm.default_endpoint").to_string())
                .default_value(
                    editing_model
                        .and_then(|model| model.endpoint.as_deref())
                        .unwrap_or_default(),
                )
        });
        let api_key_input = cx.new(|cx| {
            InputState::new(window, cx)
                .masked(true)
                .placeholder(t!("llm.api_key_placeholder").to_string())
                .default_value(
                    editing_model
                        .and_then(|model| model.api_key.as_deref())
                        .unwrap_or_default(),
                )
        });
        let provider_select = cx.new(|cx| {
            SelectState::new(provider_names, provider_index, window, cx).searchable(true)
        });
        let reasoning_select = cx.new(|cx| {
            SelectState::new(
                reasoning_option_names(),
                Some(IndexPath::new(reasoning_index(advanced.reasoning_effort))),
                window,
                cx,
            )
        });
        let reasoning_budget_input = cx.new(|cx| {
            integer_input(window, cx, REASONING_BUDGET_MIN, REASONING_BUDGET_MAX).default_value(
                advanced
                    .reasoning_effort
                    .and_then(ReasoningEffort::budget)
                    .unwrap_or(DEFAULT_REASONING_BUDGET)
                    .to_string(),
            )
        });
        let max_output_tokens_input = cx.new(|cx| {
            integer_input(window, cx, MAX_OUTPUT_TOKENS_MIN, MAX_OUTPUT_TOKENS_MAX).default_value(
                advanced
                    .max_output_tokens
                    .unwrap_or(DEFAULT_MAX_OUTPUT_TOKENS)
                    .to_string(),
            )
        });
        let temperature_input = cx.new(|cx| {
            InputState::new(window, cx).default_value(format_ratio(
                advanced.temperature.unwrap_or(DEFAULT_TEMPERATURE),
            ))
        });
        let top_p_input = cx.new(|cx| {
            InputState::new(window, cx)
                .default_value(format_ratio(advanced.top_p.unwrap_or(DEFAULT_TOP_P)))
        });

        Self {
            draft,
            editing_index,
            label_input,
            model_input,
            endpoint_input,
            api_key_input,
            provider_select,
            reasoning_select,
            reasoning_budget_input,
            max_output_tokens_input,
            temperature_input,
            top_p_input,
            max_output_tokens_enabled: advanced.max_output_tokens.is_some(),
            temperature_enabled: advanced.temperature.is_some(),
            top_p_enabled: advanced.top_p.is_some(),
            advanced_expanded: false,
            status: None,
            is_saving: false,
            toast_revision: 0,
            toast_task: None,
            write_tasks: Vec::new(),
        }
    }

    /// 返回当前草稿中的模型 ID 列表，供测试断言增删与选择行为。
    #[cfg(test)]
    pub(in crate::agent) fn model_ids_for_test(&self) -> Vec<String> {
        self.draft
            .models
            .iter()
            .map(|model| model.id.clone())
            .collect()
    }

    /// 返回当前正在编辑的模型索引。
    #[cfg(test)]
    pub(in crate::agent) fn editing_index_for_test(&self) -> Option<usize> {
        self.editing_index
    }

    /// 返回草稿中当前选中的模型 ID。
    #[cfg(test)]
    pub(in crate::agent) fn selected_model_for_test(&self) -> Option<&str> {
        self.draft.selected_model.as_deref()
    }

    /// 返回指定草稿条目的高级参数，供测试断言表单往返一致。
    #[cfg(test)]
    pub(in crate::agent) fn advanced_options_for_test(
        &self,
        index: usize,
    ) -> Option<LlmAdvancedOptions> {
        self.draft.models.get(index).map(|model| model.advanced)
    }

    /// 把当前表单写回草稿，供测试在不触发保存的前提下断言捕获结果。
    #[cfg(test)]
    pub(in crate::agent) fn capture_form_for_test(&mut self, cx: &mut Context<Self>) {
        self.capture_current_form(cx);
    }

    /// 切换某个高级参数是否随请求发送。
    #[cfg(test)]
    pub(in crate::agent) fn set_advanced_enabled_for_test(
        &mut self,
        max_output_tokens: bool,
        temperature: bool,
        top_p: bool,
    ) {
        self.max_output_tokens_enabled = max_output_tokens;
        self.temperature_enabled = temperature;
        self.top_p_enabled = top_p;
    }

    /// 追加一个新模型条目。
    #[cfg(test)]
    pub(in crate::agent) fn add_model_for_test(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.add_model(window, cx);
    }

    /// 删除当前编辑中的模型条目。
    #[cfg(test)]
    pub(in crate::agent) fn delete_model_for_test(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.delete_model(window, cx);
    }

    /// 切换到指定索引的模型条目。
    #[cfg(test)]
    pub(in crate::agent) fn select_model_for_test(
        &mut self,
        index: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.select_model(index, window, cx);
    }

    /// 保存窗口草稿并转移尚未结束的写任务，供关闭后重新创建编辑器。
    pub(crate) fn take_window_state(
        &mut self,
        cx: &mut Context<Self>,
    ) -> (AgentSettingsDraft, Vec<Task<()>>) {
        self.capture_current_form(cx);
        (
            AgentSettingsDraft(Arc::new(self.draft.clone())),
            std::mem::take(&mut self.write_tasks),
        )
    }

    fn set_status(&mut self, message: impl Into<String>, cx: &mut Context<Self>) {
        const TOAST_LIFETIME: Duration = Duration::from_millis(3_000);

        self.toast_revision = self.toast_revision.wrapping_add(1).max(1);
        let revision = self.toast_revision;
        self.status = Some(message.into());
        let background = cx.background_executor().clone();
        self.toast_task = Some(cx.spawn(async move |this, cx| {
            background.timer(TOAST_LIFETIME).await;
            let _ = this.update(cx, |this, cx| {
                if this.toast_revision == revision {
                    this.status = None;
                    cx.notify();
                }
            });
        }));
        cx.notify();
    }

    pub(super) fn toggle_advanced(&mut self, cx: &mut Context<Self>) {
        self.advanced_expanded = !self.advanced_expanded;
        cx.notify();
    }

    pub(super) fn toggle_max_output_tokens(&mut self, cx: &mut Context<Self>) {
        self.max_output_tokens_enabled = !self.max_output_tokens_enabled;
        cx.notify();
    }

    pub(super) fn toggle_temperature(&mut self, cx: &mut Context<Self>) {
        self.temperature_enabled = !self.temperature_enabled;
        cx.notify();
    }

    pub(super) fn toggle_top_p(&mut self, cx: &mut Context<Self>) {
        self.top_p_enabled = !self.top_p_enabled;
        cx.notify();
    }

    /// 返回当前思考强度选择项索引，供渲染层决定是否展示预算输入框。
    pub(super) fn reasoning_is_budget(&self, cx: &Context<Self>) -> bool {
        selected_reasoning_index(&self.reasoning_select, cx) == REASONING_BUDGET_INDEX
    }

    fn capture_current_form(&mut self, cx: &mut Context<Self>) {
        let Some(index) = self.editing_index else {
            return;
        };
        let advanced = self.capture_advanced(cx);
        let Some(model) = self.draft.models.get_mut(index) else {
            return;
        };
        model.label = self.label_input.read(cx).value().to_string();
        model.model = self.model_input.read(cx).value().to_string();
        model.endpoint = non_empty(self.endpoint_input.read(cx).value().as_ref());
        model.api_key = non_empty(self.api_key_input.read(cx).value().as_ref());
        model.provider = self
            .provider_select
            .read(cx)
            .selected_value()
            .and_then(|value| provider_from_display_name(value.as_ref()))
            .unwrap_or(LlmProvider::Ollama);
        model.advanced = advanced;
    }

    /// 未启用的高级参数一律回落为 `None`，界面里的预填值只是建议值。
    fn capture_advanced(&self, cx: &Context<Self>) -> LlmAdvancedOptions {
        let index = selected_reasoning_index(&self.reasoning_select, cx);
        let reasoning_effort = if index == REASONING_AUTO_INDEX {
            None
        } else if index == REASONING_BUDGET_INDEX {
            Some(ReasoningEffort::Budget(
                parse_u32(self.reasoning_budget_input.read(cx).value().as_ref())
                    .unwrap_or(DEFAULT_REASONING_BUDGET),
            ))
        } else {
            REASONING_EFFORT_LEVELS.get(index - 1).copied()
        };

        LlmAdvancedOptions {
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

    fn load_form(&mut self, index: Option<usize>, window: &mut Window, cx: &mut Context<Self>) {
        self.editing_index = index;
        let model = index.and_then(|index| self.draft.models.get(index));
        let provider = model
            .map(|model| model.provider)
            .unwrap_or(LlmProvider::Ollama);
        let advanced = model.map(|model| model.advanced).unwrap_or_default();
        set_input(
            &self.label_input,
            model.map(|model| model.label.as_str()).unwrap_or_default(),
            window,
            cx,
        );
        set_input(
            &self.model_input,
            model.map(|model| model.model.as_str()).unwrap_or_default(),
            window,
            cx,
        );
        set_input(
            &self.endpoint_input,
            model
                .and_then(|model| model.endpoint.as_deref())
                .unwrap_or_default(),
            window,
            cx,
        );
        set_input(
            &self.api_key_input,
            model
                .and_then(|model| model.api_key.as_deref())
                .unwrap_or_default(),
            window,
            cx,
        );
        self.provider_select.update(cx, |select, cx| {
            let value = SharedString::from(provider_display_name(provider));
            select.set_selected_value(&value, window, cx);
        });
        self.load_advanced_form(advanced, window, cx);
        cx.notify();
    }

    fn load_advanced_form(
        &mut self,
        advanced: LlmAdvancedOptions,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let index = reasoning_index(advanced.reasoning_effort);
        self.reasoning_select.update(cx, |select, cx| {
            select.set_selected_index(Some(IndexPath::new(index)), window, cx);
        });
        set_input(
            &self.reasoning_budget_input,
            &advanced
                .reasoning_effort
                .and_then(ReasoningEffort::budget)
                .unwrap_or(DEFAULT_REASONING_BUDGET)
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
        self.max_output_tokens_enabled = advanced.max_output_tokens.is_some();
        self.temperature_enabled = advanced.temperature.is_some();
        self.top_p_enabled = advanced.top_p.is_some();
    }

    pub(super) fn select_model(
        &mut self,
        index: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.is_saving {
            return;
        }
        self.capture_current_form(cx);
        let Some(model) = self.draft.models.get(index) else {
            return;
        };
        self.draft.selected_model = Some(model.id.clone());
        self.load_form(Some(index), window, cx);
        cx.notify();
    }

    pub(super) fn add_model(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.is_saving {
            return;
        }
        self.capture_current_form(cx);
        let id = next_model_id(&self.draft);
        let model = LlmModelConfig {
            id: id.clone(),
            label: t!("llm.new_model").to_string(),
            provider: LlmProvider::Ollama,
            model: String::new(),
            endpoint: Some("http://localhost:11434/".to_owned()),
            api_key: None,
            advanced: LlmAdvancedOptions::default(),
        };
        self.draft.models.push(model);
        self.draft.selected_model = Some(id);
        self.load_form(self.draft.models.len().checked_sub(1), window, cx);
        cx.notify();
    }

    pub(super) fn delete_model(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.is_saving {
            return;
        }
        let Some(index) = self.editing_index else {
            return;
        };
        if index >= self.draft.models.len() {
            return;
        }
        self.draft.models.remove(index);
        let next_index =
            (!self.draft.models.is_empty()).then(|| index.min(self.draft.models.len() - 1));
        self.draft.selected_model = next_index
            .and_then(|index| self.draft.models.get(index))
            .map(|model| model.id.clone());
        self.load_form(next_index, window, cx);
        cx.notify();
    }

    pub(super) fn save(&mut self, cx: &mut Context<Self>) {
        if self.is_saving {
            return;
        }
        self.capture_current_form(cx);
        let normalized = match self.draft.clone().normalized() {
            Ok(settings) => settings,
            Err(error) => {
                self.set_status(error.to_string(), cx);
                return;
            }
        };
        self.draft = normalized.clone();
        self.is_saving = true;
        let revision = CONFIG.reserve_llm_settings_revision();
        self.set_status(t!("llm.saving").to_string(), cx);
        let background = cx.background_executor().clone();

        let task = cx.spawn(async move |this, cx| {
            let result = background
                .spawn(async move { CONFIG.set_llm_settings_at_revision(normalized, revision) })
                .await;
            let _ = this.update(cx, |this, cx| {
                this.is_saving = false;
                let status = match result {
                    Ok(Some(_)) => {
                        cx.emit(AgentSettingsEvent);
                        t!("llm.saved").to_string()
                    }
                    Ok(None) => t!("llm.save_replaced").to_string(),
                    Err(error) => t!("llm.save_failed", error = error.to_string()).to_string(),
                };
                this.set_status(status, cx);
            });
        });
        // 只保留仍在执行的写任务，避免长期打开设置窗口时无界累积句柄。
        self.write_tasks.retain(|task| !task.is_ready());
        self.write_tasks.push(task);
    }

    pub(super) fn draft(&self) -> &LlmSettings {
        &self.draft
    }

    pub(super) fn editing_index(&self) -> Option<usize> {
        self.editing_index
    }

    pub(super) fn is_saving(&self) -> bool {
        self.is_saving
    }

    pub(super) fn status(&self) -> Option<&str> {
        self.status.as_deref()
    }

    pub(super) fn inputs(&self) -> ProviderFormInputs<'_> {
        ProviderFormInputs {
            label: &self.label_input,
            model: &self.model_input,
            endpoint: &self.endpoint_input,
            api_key: &self.api_key_input,
            provider: &self.provider_select,
            reasoning: &self.reasoning_select,
            reasoning_budget: &self.reasoning_budget_input,
            max_output_tokens: &self.max_output_tokens_input,
            temperature: &self.temperature_input,
            top_p: &self.top_p_input,
        }
    }

    pub(super) const fn advanced_toggles(&self) -> [bool; 3] {
        [
            self.max_output_tokens_enabled,
            self.temperature_enabled,
            self.top_p_enabled,
        ]
    }
}

/// 渲染层需要的全部表单实体引用，避免逐个字段暴露可变状态。
pub(super) struct ProviderFormInputs<'a> {
    pub(super) label: &'a Entity<InputState>,
    pub(super) model: &'a Entity<InputState>,
    pub(super) endpoint: &'a Entity<InputState>,
    pub(super) api_key: &'a Entity<InputState>,
    pub(super) provider: &'a Entity<SelectState<Vec<SharedString>>>,
    pub(super) reasoning: &'a Entity<SelectState<Vec<SharedString>>>,
    pub(super) reasoning_budget: &'a Entity<InputState>,
    pub(super) max_output_tokens: &'a Entity<InputState>,
    pub(super) temperature: &'a Entity<InputState>,
    pub(super) top_p: &'a Entity<InputState>,
}

impl EventEmitter<AgentSettingsEvent> for AgentSettingsView {}

fn integer_input(
    window: &mut Window,
    cx: &mut Context<InputState>,
    min: u32,
    max: u32,
) -> InputState {
    use gpui_component::input::MaskPattern;

    InputState::new(window, cx)
        .mask_pattern(MaskPattern::Number {
            separator: None,
            fraction: Some(0),
        })
        .step(1.0)
        .min(f64::from(min))
        .max(f64::from(max))
}

fn reasoning_option_names() -> Vec<SharedString> {
    let mut names = Vec::with_capacity(REASONING_OPTION_COUNT);
    names.push(SharedString::from(t!("llm.reasoning_auto").to_string()));
    for level in REASONING_EFFORT_LEVELS {
        names.push(SharedString::from(reasoning_level_name(level)));
    }
    names.push(SharedString::from(
        t!("llm.reasoning_budget_option").to_string(),
    ));
    names
}

fn reasoning_level_name(level: ReasoningEffort) -> String {
    match level {
        ReasoningEffort::Off => t!("llm.reasoning_off").to_string(),
        ReasoningEffort::Minimal => t!("llm.reasoning_minimal").to_string(),
        ReasoningEffort::Low => t!("llm.reasoning_low").to_string(),
        ReasoningEffort::Medium => t!("llm.reasoning_medium").to_string(),
        ReasoningEffort::High => t!("llm.reasoning_high").to_string(),
        ReasoningEffort::XHigh => t!("llm.reasoning_xhigh").to_string(),
        ReasoningEffort::Max => t!("llm.reasoning_max").to_string(),
        ReasoningEffort::Budget(_) => t!("llm.reasoning_budget_option").to_string(),
    }
}

fn reasoning_index(effort: Option<ReasoningEffort>) -> usize {
    match effort {
        None => REASONING_AUTO_INDEX,
        Some(ReasoningEffort::Budget(_)) => REASONING_BUDGET_INDEX,
        Some(effort) => REASONING_EFFORT_LEVELS
            .iter()
            .position(|level| *level == effort)
            .map_or(REASONING_AUTO_INDEX, |index| index + 1),
    }
}

fn selected_reasoning_index(
    select: &Entity<SelectState<Vec<SharedString>>>,
    cx: &Context<AgentSettingsView>,
) -> usize {
    select
        .read(cx)
        .selected_index(cx)
        .map_or(REASONING_AUTO_INDEX, |index| index.row)
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

fn format_ratio(value: f64) -> String {
    format!("{value}")
}

fn next_model_id(settings: &LlmSettings) -> String {
    for index in 1_u64.. {
        let id = format!("model-{index}");
        if !settings.models.iter().any(|model| model.id == id) {
            return id;
        }
    }
    unreachable!("u64 模型 ID 空间不可能被配置上限耗尽")
}

fn provider_from_display_name(name: &str) -> Option<LlmProvider> {
    LLM_PROVIDERS
        .into_iter()
        .find(|provider| provider_display_name(*provider) == name)
}

/// 暴露新模型 ID 分配规则，供测试断言不会与既有条目冲突。
#[cfg(test)]
pub(in crate::agent) fn next_model_id_for_test(settings: &LlmSettings) -> String {
    next_model_id(settings)
}

/// 暴露展示名到 Provider 的反向映射，供测试断言选择器往返一致。
#[cfg(test)]
pub(in crate::agent) fn provider_from_display_name_for_test(name: &str) -> Option<LlmProvider> {
    provider_from_display_name(name)
}

/// 暴露思考强度档位与选择项索引的双向映射，供测试断言往返一致。
#[cfg(test)]
pub(in crate::agent) fn reasoning_index_for_test(effort: Option<ReasoningEffort>) -> usize {
    reasoning_index(effort)
}

/// 暴露思考强度选择项总数，供测试断言选择器覆盖全部档位。
#[cfg(test)]
pub(in crate::agent) const fn reasoning_option_count_for_test() -> usize {
    REASONING_OPTION_COUNT
}
