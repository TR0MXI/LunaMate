//! 保存供应商设置草稿，处理条目编辑动作并发布配置变更。

use std::{sync::Arc, time::Duration};

use gpui::{
    AppContext, Context, Entity, EventEmitter, PathPromptOptions, SharedString, Subscription, Task,
    Window,
};
use gpui_component::{
    IndexPath,
    input::{InputEvent, InputState},
    select::{SelectEvent, SelectState},
};
use rust_i18n::t;

use lunamate_agent::config::{
    DEFAULT_MAX_OUTPUT_TOKENS, DEFAULT_MODEL_CONTEXT_TOKENS, DEFAULT_REASONING_BUDGET,
    DEFAULT_TEMPERATURE, DEFAULT_TOP_P, LLM_PROVIDERS, LlmAdvancedOptions, LlmModelConfig,
    LlmProvider, LlmSettings, MAX_OUTPUT_TOKENS_MAX, MAX_OUTPUT_TOKENS_MIN,
    MODEL_CONTEXT_TOKENS_MAX, MODEL_CONTEXT_TOKENS_MIN, ModelKind, ModelProvider,
    REASONING_BUDGET_MAX, REASONING_BUDGET_MIN, REASONING_EFFORT_LEVELS, ReasoningEffort,
    SharedLlmSettings, TEMPERATURE_MAX, TEMPERATURE_MIN, TOP_P_MAX, TOP_P_MIN,
    WHISPER_LANGUAGE_CODES, WHISPER_LANGUAGE_NAMES, reasoning_budget,
};

use crate::config::CONFIG;

use super::{InputEditSession, non_empty, provider_display_name, set_input};

/// 思考强度选择器第一项表示"沿用 Provider 默认值"，最后一项表示自定义 token 预算。
const REASONING_AUTO_INDEX: usize = 0;
const REASONING_BUDGET_INDEX: usize = REASONING_EFFORT_LEVELS.len() + 1;
const REASONING_OPTION_COUNT: usize = REASONING_EFFORT_LEVELS.len() + 2;

/// 设置窗口重建时保留的供应商草稿，不向 UI 暴露 Provider 配置类型。
#[derive(Clone)]
pub(in crate::ui) struct ProviderSettingsDraft {
    settings: SharedLlmSettings,
}

impl ProviderSettingsDraft {
    /// 从当前已发布配置创建设置窗口草稿。
    pub(in crate::ui) fn current() -> Self {
        Self {
            settings: CONFIG.llm_settings(),
        }
    }

    #[cfg(test)]
    pub(in crate::ui) fn from_settings_for_test(settings: LlmSettings) -> Self {
        Self {
            settings: Arc::new(settings),
        }
    }
}

/// 供应商设置写入完成后通知设置窗口；只有 `Saved` 需要刷新桌宠运行时。
#[derive(Clone, Copy, Debug)]
pub(in crate::ui) enum ProviderSettingsEvent {
    Saved,
    SaveFinished,
}

/// 设置窗口中的供应商编辑器。
pub(in crate::ui) struct ProviderSettingsView {
    draft: LlmSettings,
    active_kind: ModelKind,
    editing_index: Option<usize>,
    label_input: Entity<InputState>,
    model_input: Entity<InputState>,
    endpoint_input: Entity<InputState>,
    api_key_input: Entity<InputState>,
    app_id_input: Entity<InputState>,
    voice_input: Entity<InputState>,
    local_path_input: Entity<InputState>,
    whisper_language_select: Entity<SelectState<Vec<SharedString>>>,
    provider_select: Entity<SelectState<Vec<SharedString>>>,
    reasoning_select: Entity<SelectState<Vec<SharedString>>>,
    reasoning_budget_input: Entity<InputState>,
    context_window_tokens_input: Entity<InputState>,
    max_output_tokens_input: Entity<InputState>,
    temperature_input: Entity<InputState>,
    top_p_input: Entity<InputState>,
    context_window_tokens_enabled: bool,
    max_output_tokens_enabled: bool,
    temperature_enabled: bool,
    top_p_enabled: bool,
    use_gpu: bool,
    pub(super) advanced_expanded: bool,
    status: Option<String>,
    loading_form: bool,
    input_edit: Option<InputEditSession>,
    submitted_draft: LlmSettings,
    save_revision: u64,
    config_writes_in_flight: usize,
    toast_revision: u64,
    toast_task: Option<Task<()>>,
    picker_revision: u64,
    picker_task: Option<Task<()>>,
    form_subscriptions: Vec<Subscription>,
    write_tasks: Vec<Task<()>>,
}

impl ProviderSettingsView {
    /// 从当前运行时配置创建可丢弃的设置草稿。
    pub(in crate::ui) fn new(
        draft: ProviderSettingsDraft,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let submitted_draft = CONFIG.llm_settings().as_ref().clone();
        Self::new_with_submitted(draft, submitted_draft, window, cx)
    }

    #[cfg(test)]
    pub(in crate::ui) fn new_for_test(
        draft: ProviderSettingsDraft,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let submitted_draft = draft.settings.as_ref().clone();
        Self::new_with_submitted(draft, submitted_draft, window, cx)
    }

    fn new_with_submitted(
        draft: ProviderSettingsDraft,
        submitted_draft: LlmSettings,
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
        let ProviderSettingsDraft { settings } = draft;
        let draft = settings.as_ref().clone();
        let active_kind = draft
            .selected()
            .map(|model| model.kind)
            .or_else(|| draft.selected_transcription().map(|model| model.kind))
            .unwrap_or(ModelKind::ChatCompletions);
        let editing_index = draft
            .selected_model_id(active_kind)
            .and_then(|selected| {
                draft
                    .models
                    .iter()
                    .position(|model| model.id == selected && model.kind == active_kind)
            })
            .or_else(|| {
                draft
                    .models
                    .iter()
                    .position(|model| model.kind == active_kind)
            });
        let editing_model = editing_index.and_then(|index| draft.models.get(index));
        let provider = editing_model
            .map(|model| model.provider)
            .unwrap_or(ModelProvider::Genai(LlmProvider::Ollama));
        let advanced = editing_model
            .map(|model| model.advanced.clone())
            .unwrap_or_default();
        let use_gpu = editing_model.is_some_and(|model| model.use_gpu);
        let provider_names = model_provider_options(active_kind)
            .into_iter()
            .map(|provider| SharedString::from(provider_display_name(provider)))
            .collect::<Vec<_>>();
        let provider_index = model_provider_options(active_kind)
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
        let app_id_input = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("豆包 App ID")
                .default_value(
                    editing_model
                        .and_then(|model| model.app_id.as_deref())
                        .unwrap_or_default(),
                )
        });
        let voice_input = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("Voice ID")
                .default_value(
                    editing_model
                        .and_then(|model| model.voice.as_deref())
                        .unwrap_or_default(),
                )
        });
        let local_path_input = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("Whisper GGML 模型路径")
                .default_value(
                    editing_model
                        .and_then(|model| model.local_path.as_deref())
                        .map(|path| path.to_string_lossy().into_owned())
                        .unwrap_or_default(),
                )
        });
        let whisper_language_select = cx.new(|cx| {
            SelectState::new(
                whisper_language_options(),
                Some(IndexPath::new(whisper_language_index(
                    editing_model.and_then(|model| model.whisper_language.as_deref()),
                ))),
                window,
                cx,
            )
            .searchable(true)
        });
        let provider_select = cx.new(|cx| {
            SelectState::new(provider_names, provider_index, window, cx).searchable(true)
        });
        let reasoning_select = cx.new(|cx| {
            SelectState::new(
                reasoning_option_names(),
                Some(IndexPath::new(reasoning_index(
                    advanced.reasoning_effort.as_ref(),
                ))),
                window,
                cx,
            )
        });
        let reasoning_budget_input = cx.new(|cx| {
            integer_input(window, cx, REASONING_BUDGET_MIN, REASONING_BUDGET_MAX).default_value(
                advanced
                    .reasoning_effort
                    .as_ref()
                    .and_then(reasoning_budget)
                    .unwrap_or(DEFAULT_REASONING_BUDGET)
                    .to_string(),
            )
        });
        let context_window_tokens_input = cx.new(|cx| {
            integer_input(
                window,
                cx,
                MODEL_CONTEXT_TOKENS_MIN,
                MODEL_CONTEXT_TOKENS_MAX,
            )
            .default_value(
                advanced
                    .context_window_tokens
                    .unwrap_or(DEFAULT_MODEL_CONTEXT_TOKENS)
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

        let mut view = Self {
            draft,
            active_kind,
            editing_index,
            label_input,
            model_input,
            endpoint_input,
            api_key_input,
            app_id_input,
            voice_input,
            local_path_input,
            whisper_language_select,
            provider_select,
            reasoning_select,
            reasoning_budget_input,
            context_window_tokens_input,
            max_output_tokens_input,
            temperature_input,
            top_p_input,
            context_window_tokens_enabled: advanced.context_window_tokens.is_some(),
            max_output_tokens_enabled: advanced.max_output_tokens.is_some(),
            temperature_enabled: advanced.temperature.is_some(),
            top_p_enabled: advanced.top_p.is_some(),
            use_gpu,
            advanced_expanded: false,
            status: None,
            loading_form: false,
            input_edit: None,
            submitted_draft,
            save_revision: 0,
            config_writes_in_flight: 0,
            toast_revision: 0,
            toast_task: None,
            picker_revision: 0,
            picker_task: None,
            form_subscriptions: Vec::new(),
            write_tasks: Vec::new(),
        };
        view.form_subscriptions = vec![
            subscribe_form_input(&view.label_input, window, cx),
            subscribe_form_input(&view.model_input, window, cx),
            subscribe_form_input(&view.endpoint_input, window, cx),
            subscribe_form_input(&view.api_key_input, window, cx),
            subscribe_form_input(&view.app_id_input, window, cx),
            subscribe_form_input(&view.voice_input, window, cx),
            subscribe_form_input(&view.local_path_input, window, cx),
            subscribe_form_input(&view.reasoning_budget_input, window, cx),
            subscribe_form_input(&view.context_window_tokens_input, window, cx),
            subscribe_form_input(&view.max_output_tokens_input, window, cx),
            subscribe_form_input(&view.temperature_input, window, cx),
            subscribe_form_input(&view.top_p_input, window, cx),
            cx.subscribe(
                &view.provider_select,
                |this, _, _: &SelectEvent<Vec<SharedString>>, cx| {
                    if !this.loading_form {
                        this.save(cx);
                    }
                },
            ),
            cx.subscribe(
                &view.reasoning_select,
                |this, _, _: &SelectEvent<Vec<SharedString>>, cx| {
                    if !this.loading_form {
                        this.save(cx);
                    }
                },
            ),
            cx.subscribe(
                &view.whisper_language_select,
                |this, _, _: &SelectEvent<Vec<SharedString>>, cx| {
                    if !this.loading_form {
                        this.save(cx);
                    }
                },
            ),
        ];
        view
    }

    /// 返回当前草稿中的模型 ID 列表，供测试断言增删与选择行为。
    #[cfg(test)]
    pub(crate) fn model_ids_for_test(&self) -> Vec<String> {
        self.draft
            .models
            .iter()
            .map(|model| model.id.clone())
            .collect()
    }

    /// 返回当前正在编辑的模型索引。
    #[cfg(test)]
    pub(crate) fn editing_index_for_test(&self) -> Option<usize> {
        self.editing_index
    }

    /// 返回草稿中当前选中的模型 ID。
    #[cfg(test)]
    pub(crate) fn selected_model_for_test(&self) -> Option<&str> {
        self.draft.selected_model.as_deref()
    }

    #[cfg(test)]
    pub(crate) fn selected_transcription_model_for_test(&self) -> Option<&str> {
        self.draft.selected_transcription_model.as_deref()
    }

    #[cfg(test)]
    pub(crate) fn local_whisper_preferences_for_test(
        &self,
        cx: &Context<Self>,
    ) -> (bool, Option<String>) {
        (
            self.use_gpu,
            selected_whisper_language(&self.whisper_language_select, cx),
        )
    }

    #[cfg(test)]
    pub(crate) const fn active_kind_for_test(&self) -> ModelKind {
        self.active_kind
    }

    #[cfg(test)]
    pub(crate) fn model_kinds_for_test(&self) -> Vec<ModelKind> {
        self.draft.models.iter().map(|model| model.kind).collect()
    }

    /// 返回指定草稿条目的高级参数，供测试断言表单往返一致。
    #[cfg(test)]
    pub(crate) fn advanced_options_for_test(&self, index: usize) -> Option<LlmAdvancedOptions> {
        self.draft
            .models
            .get(index)
            .map(|model| model.advanced.clone())
    }

    /// 把当前表单写回草稿，供测试在不触发保存的前提下断言捕获结果。
    #[cfg(test)]
    pub(crate) fn capture_form_for_test(&mut self, cx: &mut Context<Self>) {
        self.capture_current_form(cx);
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

    /// 追加一个新模型条目。
    #[cfg(test)]
    pub(crate) fn add_model_for_test(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.add_model(window, cx);
    }

    /// 删除当前编辑中的模型条目。
    #[cfg(test)]
    pub(crate) fn delete_model_for_test(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.delete_model_inner(false, window, cx);
    }

    /// 切换到指定索引的模型条目。
    #[cfg(test)]
    pub(crate) fn select_model_for_test(
        &mut self,
        index: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.select_model_inner(index, false, window, cx);
    }

    #[cfg(test)]
    pub(crate) fn select_kind_for_test(
        &mut self,
        kind: ModelKind,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.select_kind_inner(kind, false, window, cx);
    }

    /// 保存窗口草稿并转移尚未结束的写任务，供关闭后重新创建编辑器。
    pub(in crate::ui) fn take_window_state(
        &mut self,
        cx: &mut Context<Self>,
    ) -> (ProviderSettingsDraft, Vec<Task<()>>) {
        self.save(cx);
        (
            ProviderSettingsDraft {
                settings: Arc::new(self.draft.clone()),
            },
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
        model.app_id = non_empty(self.app_id_input.read(cx).value().as_ref());
        model.voice = non_empty(self.voice_input.read(cx).value().as_ref());
        model.local_path = non_empty(self.local_path_input.read(cx).value().as_ref())
            .map(std::path::PathBuf::from);
        model.use_gpu = self.use_gpu;
        model.whisper_language = selected_whisper_language(&self.whisper_language_select, cx);
        model.provider = self
            .provider_select
            .read(cx)
            .selected_value()
            .and_then(|value| model_provider_from_display_name(model.kind, value.as_ref()))
            .unwrap_or_else(|| default_provider(model.kind));
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

    fn load_form(&mut self, index: Option<usize>, window: &mut Window, cx: &mut Context<Self>) {
        self.loading_form = true;
        self.picker_revision = self.picker_revision.wrapping_add(1).max(1);
        self.picker_task = None;
        self.editing_index = index;
        let model = index.and_then(|index| self.draft.models.get(index));
        let provider = model
            .map(|model| model.provider)
            .unwrap_or_else(|| default_provider(self.active_kind));
        let advanced = model
            .map(|model| model.advanced.clone())
            .unwrap_or_default();
        self.use_gpu = model.is_some_and(|model| model.use_gpu);
        set_input(
            &self.label_input,
            model.map(|model| model.label.as_str()).unwrap_or_default(),
            window,
            cx,
        );
        self.whisper_language_select.update(cx, |select, cx| {
            select.set_selected_index(
                Some(IndexPath::new(whisper_language_index(
                    model.and_then(|model| model.whisper_language.as_deref()),
                ))),
                window,
                cx,
            );
        });
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
        set_input(
            &self.app_id_input,
            model
                .and_then(|model| model.app_id.as_deref())
                .unwrap_or_default(),
            window,
            cx,
        );
        set_input(
            &self.voice_input,
            model
                .and_then(|model| model.voice.as_deref())
                .unwrap_or_default(),
            window,
            cx,
        );
        set_input(
            &self.local_path_input,
            &model
                .and_then(|model| model.local_path.as_deref())
                .map(|path| path.to_string_lossy().into_owned())
                .unwrap_or_default(),
            window,
            cx,
        );
        self.provider_select.update(cx, |select, cx| {
            select.set_items(
                model_provider_options(self.active_kind)
                    .into_iter()
                    .map(|provider| SharedString::from(provider_display_name(provider)))
                    .collect(),
                window,
                cx,
            );
            let value = SharedString::from(provider_display_name(provider));
            select.set_selected_value(&value, window, cx);
        });
        self.load_advanced_form(advanced, window, cx);
        self.loading_form = false;
        cx.notify();
    }

    fn load_advanced_form(
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

    pub(super) fn select_model(
        &mut self,
        index: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.select_model_inner(index, true, window, cx);
    }

    fn select_model_inner(
        &mut self,
        index: usize,
        persist: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.capture_current_form(cx);
        let Some(model) = self.draft.models.get(index) else {
            return;
        };
        if model.kind == ModelKind::ChatCompletions {
            self.draft.selected_model = Some(model.id.clone());
        } else if model.kind == ModelKind::Transcription {
            self.draft.selected_transcription_model = Some(model.id.clone());
        }
        self.load_form(Some(index), window, cx);
        if persist {
            self.save(cx);
        }
    }

    pub(super) fn add_model(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.capture_current_form(cx);
        let id = next_model_id(&self.draft);
        let model = LlmModelConfig {
            id: id.clone(),
            label: t!("llm.new_model").to_string(),
            kind: self.active_kind,
            provider: default_provider(self.active_kind),
            model: String::new(),
            endpoint: (self.active_kind == ModelKind::ChatCompletions)
                .then(|| "http://localhost:11434/".to_owned()),
            api_key: None,
            app_id: None,
            voice: (self.active_kind == ModelKind::SpeechSynthesis).then(|| "alloy".to_owned()),
            local_path: None,
            use_gpu: false,
            whisper_language: None,
            advanced: LlmAdvancedOptions::default(),
        };
        self.draft.models.push(model);
        if self.active_kind == ModelKind::ChatCompletions {
            self.draft.selected_model = Some(id);
        } else if self.active_kind == ModelKind::Transcription {
            self.draft.selected_transcription_model = Some(id);
        }
        self.load_form(self.draft.models.len().checked_sub(1), window, cx);
        cx.notify();
    }

    pub(super) fn delete_model(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.delete_model_inner(true, window, cx);
    }

    fn delete_model_inner(&mut self, persist: bool, window: &mut Window, cx: &mut Context<Self>) {
        let Some(index) = self.editing_index else {
            return;
        };
        if index >= self.draft.models.len() {
            return;
        }
        let removed = self.draft.models.remove(index);
        let visible_indices = self
            .draft
            .models
            .iter()
            .enumerate()
            .filter_map(|(index, model)| (model.kind == self.active_kind).then_some(index))
            .collect::<Vec<_>>();
        let next_index = visible_indices
            .iter()
            .copied()
            .find(|candidate| *candidate >= index)
            .or_else(|| visible_indices.last().copied());
        if self.draft.selected_model.as_deref() == Some(removed.id.as_str()) {
            self.draft.selected_model = next_index
                .and_then(|index| self.draft.models.get(index))
                .filter(|model| model.kind == ModelKind::ChatCompletions)
                .map(|model| model.id.clone());
        }
        if self.draft.selected_transcription_model.as_deref() == Some(removed.id.as_str()) {
            self.draft.selected_transcription_model = next_index
                .and_then(|index| self.draft.models.get(index))
                .filter(|model| model.kind == ModelKind::Transcription)
                .map(|model| model.id.clone());
        }
        self.load_form(next_index, window, cx);
        if persist {
            self.save(cx);
        }
    }

    pub(super) fn save(&mut self, cx: &mut Context<Self>) {
        if self.loading_form {
            return;
        }
        self.capture_current_form(cx);
        let language = CONFIG.agent_config_snapshot().language();
        let normalized = match self.draft.clone().normalized(language) {
            Ok(settings) => settings,
            Err(error) => {
                self.set_status(error.to_string(), cx);
                return;
            }
        };
        if normalized == self.submitted_draft {
            return;
        }
        self.draft = normalized.clone();
        self.submitted_draft = normalized.clone();
        self.save_revision = self.save_revision.wrapping_add(1).max(1);
        let ui_revision = self.save_revision;
        self.config_writes_in_flight = self.config_writes_in_flight.saturating_add(1);
        let config_revision = CONFIG.reserve_llm_settings_revision();
        self.set_status(t!("llm.saving").to_string(), cx);
        let background = cx.background_executor().clone();

        let task = cx.spawn(async move |this, cx| {
            let result = background
                .spawn(async move {
                    CONFIG.set_llm_settings_at_revision(normalized, config_revision, language)
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                let latest = this.save_revision == ui_revision;
                this.config_writes_in_flight = this.config_writes_in_flight.saturating_sub(1);
                match result {
                    Ok(Some(_)) => {
                        cx.emit(ProviderSettingsEvent::Saved);
                        if latest {
                            this.set_status(t!("llm.saved").to_string(), cx);
                        }
                    }
                    Ok(None) => {
                        if latest {
                            this.submitted_draft = CONFIG.llm_settings().as_ref().clone();
                            this.set_status(t!("llm.save_replaced").to_string(), cx);
                        }
                    }
                    Err(error) => {
                        if latest {
                            this.submitted_draft = CONFIG.llm_settings().as_ref().clone();
                            this.set_status(
                                t!("llm.save_failed", error = error.to_string()).to_string(),
                                cx,
                            );
                        }
                    }
                }
                if this.config_writes_in_flight == 0 {
                    cx.emit(ProviderSettingsEvent::SaveFinished);
                }
            });
        });
        // 只保留仍在执行的写任务，避免长期打开设置窗口时无界累积句柄。
        self.write_tasks.retain(|task| !task.is_ready());
        self.write_tasks.push(task);
    }

    pub(super) fn draft(&self) -> &LlmSettings {
        &self.draft
    }

    pub(super) const fn active_kind(&self) -> ModelKind {
        self.active_kind
    }

    pub(super) fn select_kind(
        &mut self,
        kind: ModelKind,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.select_kind_inner(kind, true, window, cx);
    }

    fn select_kind_inner(
        &mut self,
        kind: ModelKind,
        persist: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.active_kind == kind {
            return;
        }
        self.capture_current_form(cx);
        self.active_kind = kind;
        let selected = self.draft.selected_model_id(kind);
        let index = selected
            .and_then(|selected| {
                self.draft
                    .models
                    .iter()
                    .position(|model| model.id == selected && model.kind == kind)
            })
            .or_else(|| {
                self.draft
                    .models
                    .iter()
                    .position(|model| model.kind == kind)
            });
        self.load_form(index, window, cx);
        if persist {
            self.save(cx);
        }
    }

    pub(super) fn choose_local_model(&mut self, cx: &mut Context<Self>) {
        self.picker_revision = self.picker_revision.wrapping_add(1).max(1);
        let revision = self.picker_revision;
        let paths = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: Some("选择 Whisper GGML 模型".into()),
        });
        let input = self.local_path_input.clone();
        self.picker_task = Some(cx.spawn(async move |this, cx| {
            let Ok(Ok(Some(paths))) = paths.await else {
                return;
            };
            let Some(path) = paths.into_iter().next() else {
                return;
            };
            let _ = this.update_in(cx, |this, window, cx| {
                if this.picker_revision != revision {
                    return;
                }
                input.update(cx, |input, cx| {
                    input.set_value(path.to_string_lossy().into_owned(), window, cx)
                });
                this.capture_current_form(cx);
                this.save(cx);
            });
        }));
    }

    pub(super) fn editing_index(&self) -> Option<usize> {
        self.editing_index
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
            app_id: &self.app_id_input,
            voice: &self.voice_input,
            local_path: &self.local_path_input,
            whisper_language: &self.whisper_language_select,
            provider: &self.provider_select,
            reasoning: &self.reasoning_select,
            reasoning_budget: &self.reasoning_budget_input,
            context_window_tokens: &self.context_window_tokens_input,
            max_output_tokens: &self.max_output_tokens_input,
            temperature: &self.temperature_input,
            top_p: &self.top_p_input,
        }
    }

    pub(super) fn selected_provider(&self, cx: &Context<Self>) -> ModelProvider {
        self.provider_select
            .read(cx)
            .selected_value()
            .and_then(|value| model_provider_from_display_name(self.active_kind, value.as_ref()))
            .unwrap_or_else(|| default_provider(self.active_kind))
    }

    pub(super) const fn advanced_toggles(&self) -> [bool; 4] {
        [
            self.context_window_tokens_enabled,
            self.max_output_tokens_enabled,
            self.temperature_enabled,
            self.top_p_enabled,
        ]
    }

    pub(super) const fn use_gpu(&self) -> bool {
        self.use_gpu
    }

    pub(super) fn cancel_input_edit(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some(edit) = self.input_edit.take() else {
            return false;
        };
        self.loading_form = true;
        edit.restore(window, cx);
        self.loading_form = false;
        true
    }
}

fn subscribe_form_input(
    input: &Entity<InputState>,
    window: &mut Window,
    cx: &mut Context<ProviderSettingsView>,
) -> Subscription {
    cx.subscribe_in(
        input,
        window,
        |this, input, event: &InputEvent, window, cx| match event {
            InputEvent::Focus => {
                if !this.loading_form {
                    this.input_edit = Some(InputEditSession::begin(input, cx));
                }
            }
            InputEvent::PressEnter { .. } => {
                if !this.loading_form {
                    this.save(cx);
                    window.blur();
                }
            }
            InputEvent::Blur => {
                if this
                    .input_edit
                    .as_ref()
                    .is_some_and(|edit| edit.belongs_to(input))
                {
                    this.input_edit = None;
                }
                if !this.loading_form {
                    this.save(cx);
                }
            }
            InputEvent::Change => {}
        },
    )
}

/// 渲染层需要的全部表单实体引用，避免逐个字段暴露可变状态。
pub(super) struct ProviderFormInputs<'a> {
    pub(super) label: &'a Entity<InputState>,
    pub(super) model: &'a Entity<InputState>,
    pub(super) endpoint: &'a Entity<InputState>,
    pub(super) api_key: &'a Entity<InputState>,
    pub(super) app_id: &'a Entity<InputState>,
    pub(super) voice: &'a Entity<InputState>,
    pub(super) local_path: &'a Entity<InputState>,
    pub(super) whisper_language: &'a Entity<SelectState<Vec<SharedString>>>,
    pub(super) provider: &'a Entity<SelectState<Vec<SharedString>>>,
    pub(super) reasoning: &'a Entity<SelectState<Vec<SharedString>>>,
    pub(super) reasoning_budget: &'a Entity<InputState>,
    pub(super) context_window_tokens: &'a Entity<InputState>,
    pub(super) max_output_tokens: &'a Entity<InputState>,
    pub(super) temperature: &'a Entity<InputState>,
    pub(super) top_p: &'a Entity<InputState>,
}

impl EventEmitter<ProviderSettingsEvent> for ProviderSettingsView {}

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

fn reasoning_index(effort: Option<&ReasoningEffort>) -> usize {
    match effort {
        None => REASONING_AUTO_INDEX,
        Some(ReasoningEffort::Budget(_)) => REASONING_BUDGET_INDEX,
        Some(effort) => REASONING_EFFORT_LEVELS
            .iter()
            .position(|level| level.variant_name() == effort.variant_name())
            .map_or(REASONING_AUTO_INDEX, |index| index + 1),
    }
}

fn selected_reasoning_index(
    select: &Entity<SelectState<Vec<SharedString>>>,
    cx: &Context<ProviderSettingsView>,
) -> usize {
    select
        .read(cx)
        .selected_index(cx)
        .map_or(REASONING_AUTO_INDEX, |index| index.row)
}

fn whisper_language_options() -> Vec<SharedString> {
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

fn whisper_language_index(language: Option<&str>) -> usize {
    language
        .and_then(|language| {
            WHISPER_LANGUAGE_CODES
                .iter()
                .position(|candidate| *candidate == language)
        })
        .map_or(0, |index| index + 1)
}

fn selected_whisper_language(
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

#[cfg(test)]
fn provider_from_display_name(name: &str) -> Option<LlmProvider> {
    LLM_PROVIDERS
        .into_iter()
        .find(|provider| provider_display_name(*provider) == name)
}

fn model_provider_options(kind: ModelKind) -> Vec<ModelProvider> {
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

fn default_provider(kind: ModelKind) -> ModelProvider {
    match kind {
        ModelKind::ChatCompletions => ModelProvider::Genai(LlmProvider::Ollama),
        ModelKind::SpeechSynthesis | ModelKind::Transcription => {
            ModelProvider::Genai(LlmProvider::OpenAI)
        }
    }
}

fn model_provider_from_display_name(kind: ModelKind, name: &str) -> Option<ModelProvider> {
    model_provider_options(kind)
        .into_iter()
        .find(|provider| provider_display_name(*provider) == name)
}

/// 暴露新模型 ID 分配规则，供测试断言不会与既有条目冲突。
#[cfg(test)]
pub(crate) fn next_model_id_for_test(settings: &LlmSettings) -> String {
    next_model_id(settings)
}

/// 暴露展示名到 Provider 的反向映射，供测试断言选择器往返一致。
#[cfg(test)]
pub(crate) fn provider_from_display_name_for_test(name: &str) -> Option<LlmProvider> {
    provider_from_display_name(name)
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
