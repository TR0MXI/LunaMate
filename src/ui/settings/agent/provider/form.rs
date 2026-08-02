//! 构造供应商表单，并维护输入草稿与本地模型文件选择。

use gpui::{AppContext, Context, Entity, PathPromptOptions, SharedString, Subscription, Window};
use gpui_component::{
    IndexPath,
    input::{InputEvent, InputState},
    select::{SelectEvent, SelectState},
};
use rust_i18n::t;

use lunamate_agent::config::{
    DEFAULT_MAX_OUTPUT_TOKENS, DEFAULT_MODEL_CONTEXT_TOKENS, DEFAULT_REASONING_BUDGET,
    DEFAULT_TEMPERATURE, DEFAULT_TOP_P, LlmProvider, LlmSettings, MAX_OUTPUT_TOKENS_MAX,
    MAX_OUTPUT_TOKENS_MIN, MODEL_CONTEXT_TOKENS_MAX, MODEL_CONTEXT_TOKENS_MIN, ModelKind,
    ModelProvider, REASONING_BUDGET_MAX, REASONING_BUDGET_MIN, reasoning_budget,
};

use crate::config::CONFIG;

use super::{
    super::{InputEditSession, non_empty, provider_display_name, set_input},
    ProviderSettingsDraft, ProviderSettingsView,
    options::{
        default_provider, format_ratio, integer_input, model_provider_from_display_name,
        model_provider_options, reasoning_index, reasoning_option_names, selected_whisper_language,
        whisper_language_index, whisper_language_options,
    },
};

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
        let voice_input = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("Voice / Voice Type")
                .default_value(
                    editing_model
                        .and_then(|model| {
                            if model.provider == ModelProvider::Doubao {
                                model.voice_type.as_deref()
                            } else {
                                model.voice.as_deref()
                            }
                        })
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

    pub(super) fn capture_current_form(&mut self, cx: &mut Context<Self>) {
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
        let speech_voice = non_empty(self.voice_input.read(cx).value().as_ref());
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
        if model.provider == ModelProvider::Doubao {
            model.voice = None;
            model.voice_type = speech_voice;
        } else {
            model.voice = speech_voice;
            model.voice_type = None;
        }
        model.advanced = advanced;
    }

    pub(super) fn load_form(
        &mut self,
        index: Option<usize>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
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
            &self.voice_input,
            model
                .and_then(|model| {
                    if model.provider == ModelProvider::Doubao {
                        model.voice_type.as_deref()
                    } else {
                        model.voice.as_deref()
                    }
                })
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

    /// 把当前表单写回草稿，供测试在不触发保存的前提下断言捕获结果。
    #[cfg(test)]
    pub(crate) fn capture_form_for_test(&mut self, cx: &mut Context<Self>) {
        self.capture_current_form(cx);
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
