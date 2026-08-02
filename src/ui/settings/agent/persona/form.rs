//! 构造人格表单，并维护人格切换、模型绑定和输入草稿。

use std::{cell::Cell, collections::HashSet, path::PathBuf, rc::Rc};

use gpui::{AppContext, Bounds, Context, Entity, ScrollHandle, SharedString, Subscription, Window};
use gpui_component::{
    IndexPath,
    input::{InputEvent, InputState, MaskPattern},
    select::{SelectEvent, SelectState},
};
use lunamate_agent::AgentMemory;
use lunamate_agent::config::{
    CONTEXT_MESSAGES_MAX, CONTEXT_MESSAGES_MIN, CONTEXT_TOKENS_MAX, CONTEXT_TOKENS_MIN, ModelKind,
    PersonaConfig, PersonaContextLimits, PersonaSettings, SharedLlmSettings,
};
use rust_i18n::t;

use crate::config::CONFIG;

use super::{
    super::{InputEditSession, set_input},
    Live2dModelOption, PersonaPage, PersonaSettingsDraft, PersonaSettingsView,
    options::{
        live2d_option_state, model_option_id, model_option_index, model_option_names,
        next_persona_id,
    },
};

impl PersonaSettingsView {
    /// 从当前运行时配置创建可丢弃的人格草稿。
    pub(in crate::ui) fn new(
        draft: PersonaSettingsDraft,
        memory: AgentMemory,
        live2d_models: Vec<(String, PathBuf)>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let providers = CONFIG.llm_settings();
        let submitted_draft = CONFIG.persona_settings().as_ref().clone();
        Self::new_with_snapshots(
            draft,
            memory,
            providers,
            submitted_draft,
            live2d_models,
            window,
            cx,
        )
    }

    #[cfg(test)]
    pub(in crate::ui) fn new_for_test(
        draft: PersonaSettingsDraft,
        memory: AgentMemory,
        providers: SharedLlmSettings,
        live2d_models: Vec<(String, PathBuf)>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let submitted_draft = draft.settings.as_ref().clone();
        Self::new_with_snapshots(
            draft,
            memory,
            providers,
            submitted_draft,
            live2d_models,
            window,
            cx,
        )
    }

    fn new_with_snapshots(
        draft: PersonaSettingsDraft,
        memory: AgentMemory,
        providers: SharedLlmSettings,
        submitted_draft: PersonaSettings,
        live2d_models: Vec<(String, PathBuf)>,
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
        let PersonaSettingsDraft {
            settings,
            mut reserved_cleanup,
        } = draft;
        let draft = settings.as_ref().clone();
        reserved_cleanup.extend(draft.pending_deletions.iter().cloned());
        let pending_persona_cleanup = reserved_cleanup;
        let editing_index = draft
            .selected
            .as_deref()
            .and_then(|selected| draft.personas.iter().position(|item| item.id == selected))
            .or_else(|| (!draft.personas.is_empty()).then_some(0));
        let editing = editing_index.and_then(|index| draft.personas.get(index));
        let context = editing.map(|persona| persona.context).unwrap_or_default();
        let live2d_models = live2d_models
            .into_iter()
            .map(|(label, path)| Live2dModelOption { label, path })
            .collect::<Vec<_>>();
        let bound_live2d = editing.and_then(|persona| persona.live2d_model.as_deref());
        let (live2d_names, live2d_index, missing_live2d_model) =
            live2d_option_state(&live2d_models, bound_live2d);

        let name_input = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder(t!("persona.name_placeholder").to_string())
                .default_value(
                    editing
                        .map(|persona| persona.name.as_str())
                        .unwrap_or_default(),
                )
        });
        let system_prompt_input = cx.new(|cx| {
            InputState::new(window, cx)
                .multi_line(true)
                .rows(10)
                .placeholder(t!("persona.system_prompt_placeholder").to_string())
                .default_value(
                    editing
                        .map(|persona| persona.system_prompt.as_str())
                        .unwrap_or_default(),
                )
        });
        let input_prompt_input = cx.new(|cx| {
            InputState::new(window, cx)
                .multi_line(true)
                .rows(6)
                .placeholder(t!("persona.input_prompt_placeholder").to_string())
                .default_value(
                    editing
                        .map(|persona| persona.input_prompt.as_str())
                        .unwrap_or_default(),
                )
        });
        let provider_select = cx.new(|cx| {
            SelectState::new(
                model_option_names(&providers, ModelKind::ChatCompletions),
                Some(IndexPath::new(model_option_index(
                    &providers,
                    ModelKind::ChatCompletions,
                    editing.and_then(|persona| persona.model.as_deref()),
                ))),
                window,
                cx,
            )
            .searchable(true)
        });
        let tts_select = cx.new(|cx| {
            SelectState::new(
                model_option_names(&providers, ModelKind::SpeechSynthesis),
                Some(IndexPath::new(model_option_index(
                    &providers,
                    ModelKind::SpeechSynthesis,
                    editing.and_then(|persona| persona.tts_model.as_deref()),
                ))),
                window,
                cx,
            )
            .searchable(true)
        });
        let live2d_select = cx.new(|cx| {
            SelectState::new(live2d_names, Some(IndexPath::new(live2d_index)), window, cx)
                .searchable(true)
        });
        let context_messages_input = cx.new(|cx| {
            integer_input(window, cx, CONTEXT_MESSAGES_MIN, CONTEXT_MESSAGES_MAX).default_value(
                context
                    .max_messages
                    .map(|value| value.to_string())
                    .unwrap_or_default(),
            )
        });
        let context_tokens_input = cx.new(|cx| {
            integer_input(window, cx, CONTEXT_TOKENS_MIN, CONTEXT_TOKENS_MAX).default_value(
                context
                    .max_tokens
                    .map(|value| value.to_string())
                    .unwrap_or_default(),
            )
        });

        let mut view = Self {
            draft,
            providers,
            editing_index,
            active_page: PersonaPage::Definition,
            name_input,
            system_prompt_input,
            input_prompt_input,
            provider_select,
            tts_select,
            live2d_select,
            live2d_models,
            missing_live2d_model,
            context_messages_input,
            context_tokens_input,
            context_editors: Vec::new(),
            context_subscriptions: Vec::new(),
            form_subscriptions: Vec::new(),
            context_selected: HashSet::new(),
            context_selection_drag: None,
            context_selection_auto_scroll_revision: 0,
            context_selection_auto_scroll_task: None,
            context_view_bounds: Rc::new(Cell::new(Bounds::default())),
            context_editing: None,
            context_focus: cx.focus_handle(),
            context_scroll: ScrollHandle::new(),
            context_loading: false,
            context_error: None,
            context_revision: 0,
            context_task: None,
            context_auto_refresh_revision: 0,
            observed_live_context_revision: None,
            context_auto_refresh_task: None,
            memory,
            usage: None,
            usage_error: None,
            usage_revision: 0,
            usage_task: None,
            pending_confirm: None,
            status: None,
            loading_form: false,
            input_edit: None,
            submitted_draft,
            save_revision: 0,
            config_writes_in_flight: 0,
            window_transferred: false,
            pending_persona_cleanup,
            persona_cleanup_in_flight: HashSet::new(),
            toast_revision: 0,
            toast_task: None,
            write_tasks: Vec::new(),
        };
        view.form_subscriptions = vec![
            subscribe_form_input(&view.name_input, true, window, cx),
            subscribe_form_input(&view.system_prompt_input, false, window, cx),
            subscribe_form_input(&view.input_prompt_input, false, window, cx),
            subscribe_form_input(&view.context_messages_input, true, window, cx),
            subscribe_form_input(&view.context_tokens_input, true, window, cx),
            cx.subscribe(
                &view.provider_select,
                |this, _, _: &SelectEvent<Vec<SharedString>>, cx| {
                    this.save(cx);
                },
            ),
            cx.subscribe(
                &view.tts_select,
                |this, _, _: &SelectEvent<Vec<SharedString>>, cx| {
                    this.save(cx);
                },
            ),
            cx.subscribe(
                &view.live2d_select,
                |this, _, _: &SelectEvent<Vec<SharedString>>, cx| {
                    this.save(cx);
                },
            ),
        ];
        view
    }

    /// 供应商目录变化后刷新绑定选择器的候选项。
    pub(in crate::ui) fn refresh_providers(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.commit_context_edits(cx);
        self.capture_current_form(cx);
        self.providers = CONFIG.llm_settings();
        let bound = self
            .editing_index
            .and_then(|index| self.draft.personas.get(index))
            .and_then(|persona| persona.model.clone());
        let tts_bound = self
            .editing_index
            .and_then(|index| self.draft.personas.get(index))
            .and_then(|persona| persona.tts_model.clone());
        let names = model_option_names(&self.providers, ModelKind::ChatCompletions);
        let index = model_option_index(
            &self.providers,
            ModelKind::ChatCompletions,
            bound.as_deref(),
        );
        let tts_names = model_option_names(&self.providers, ModelKind::SpeechSynthesis);
        let tts_index = model_option_index(
            &self.providers,
            ModelKind::SpeechSynthesis,
            tts_bound.as_deref(),
        );
        self.loading_form = true;
        self.provider_select.update(cx, |select, cx| {
            select.set_items(names, window, cx);
            select.set_selected_index(Some(IndexPath::new(index)), window, cx);
        });
        self.tts_select.update(cx, |select, cx| {
            select.set_items(tts_names, window, cx);
            select.set_selected_index(Some(IndexPath::new(tts_index)), window, cx);
        });
        self.loading_form = false;
        if self.active_page == PersonaPage::Context {
            self.refresh_usage(cx);
            self.refresh_context(window, cx);
            self.start_context_auto_refresh(window, cx);
        }
        cx.notify();
    }

    /// 模型目录扫描完成后更新 Live2D 候选，并保留不存在的既有绑定供用户修复。
    pub(in crate::ui) fn refresh_live2d_models(
        &mut self,
        models: Vec<(String, PathBuf)>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.capture_current_form(cx);
        self.live2d_models = models
            .into_iter()
            .map(|(label, path)| Live2dModelOption { label, path })
            .collect();
        let bound = self
            .editing_index
            .and_then(|index| self.draft.personas.get(index))
            .and_then(|persona| persona.live2d_model.as_deref());
        let (names, index, missing) = live2d_option_state(&self.live2d_models, bound);
        self.missing_live2d_model = missing;
        self.loading_form = true;
        self.live2d_select.update(cx, |select, cx| {
            select.set_items(names, window, cx);
            select.set_selected_index(Some(IndexPath::new(index)), window, cx);
        });
        self.loading_form = false;
        cx.notify();
    }

    pub(super) fn capture_current_form(&mut self, cx: &mut Context<Self>) {
        let Some(index) = self.editing_index else {
            return;
        };
        let bound = self.selected_provider_id(cx);
        let tts_model = self.selected_tts_model_id(cx);
        let live2d_model = self.selected_live2d_model(cx);
        let context = self.capture_context_limits(cx);
        let Some(persona) = self.draft.personas.get_mut(index) else {
            return;
        };
        persona.name = self.name_input.read(cx).value().to_string();
        persona.system_prompt = self.system_prompt_input.read(cx).value().to_string();
        persona.input_prompt = self.input_prompt_input.read(cx).value().to_string();
        persona.model = bound;
        persona.tts_model = tts_model;
        persona.live2d_model = live2d_model;
        persona.context = context;
    }

    pub(super) fn capture_context_limits(&self, cx: &Context<Self>) -> PersonaContextLimits {
        PersonaContextLimits {
            max_messages: parse_u32(self.context_messages_input.read(cx).value().as_ref()),
            max_tokens: parse_u32(self.context_tokens_input.read(cx).value().as_ref()),
        }
    }

    pub(super) fn selected_provider_id(&self, cx: &Context<Self>) -> Option<String> {
        let row = self
            .provider_select
            .read(cx)
            .selected_index(cx)
            .map_or(0, |index| index.row);
        model_option_id(&self.providers, ModelKind::ChatCompletions, row)
    }

    fn selected_tts_model_id(&self, cx: &Context<Self>) -> Option<String> {
        let row = self
            .tts_select
            .read(cx)
            .selected_index(cx)
            .map_or(0, |index| index.row);
        model_option_id(&self.providers, ModelKind::SpeechSynthesis, row)
    }

    fn selected_live2d_model(&self, cx: &Context<Self>) -> Option<PathBuf> {
        let row = self
            .live2d_select
            .read(cx)
            .selected_index(cx)
            .map_or(0, |index| index.row);
        if row == 0 {
            return None;
        }
        self.live2d_models
            .get(row - 1)
            .map(|model| model.path.clone())
            .or_else(|| self.missing_live2d_model.clone())
    }

    pub(super) fn load_form(
        &mut self,
        index: Option<usize>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.loading_form = true;
        self.editing_index = index;
        let persona = index.and_then(|index| self.draft.personas.get(index));
        let context = persona.map(|persona| persona.context).unwrap_or_default();
        let bound = persona.and_then(|persona| persona.model.clone());
        let tts_bound = persona.and_then(|persona| persona.tts_model.clone());
        set_input(
            &self.name_input,
            persona
                .map(|persona| persona.name.as_str())
                .unwrap_or_default(),
            window,
            cx,
        );
        set_input(
            &self.input_prompt_input,
            persona
                .map(|persona| persona.input_prompt.as_str())
                .unwrap_or_default(),
            window,
            cx,
        );
        set_input(
            &self.system_prompt_input,
            persona
                .map(|persona| persona.system_prompt.as_str())
                .unwrap_or_default(),
            window,
            cx,
        );
        let provider_index = model_option_index(
            &self.providers,
            ModelKind::ChatCompletions,
            bound.as_deref(),
        );
        self.provider_select.update(cx, |select, cx| {
            select.set_selected_index(Some(IndexPath::new(provider_index)), window, cx);
        });
        let tts_index = model_option_index(
            &self.providers,
            ModelKind::SpeechSynthesis,
            tts_bound.as_deref(),
        );
        self.tts_select.update(cx, |select, cx| {
            select.set_selected_index(Some(IndexPath::new(tts_index)), window, cx);
        });
        let (live2d_names, live2d_index, missing_live2d_model) = live2d_option_state(
            &self.live2d_models,
            persona.and_then(|persona| persona.live2d_model.as_deref()),
        );
        self.missing_live2d_model = missing_live2d_model;
        self.live2d_select.update(cx, |select, cx| {
            select.set_items(live2d_names, window, cx);
            select.set_selected_index(Some(IndexPath::new(live2d_index)), window, cx);
        });
        set_input(
            &self.context_messages_input,
            &context
                .max_messages
                .map(|value| value.to_string())
                .unwrap_or_default(),
            window,
            cx,
        );
        set_input(
            &self.context_tokens_input,
            &context
                .max_tokens
                .map(|value| value.to_string())
                .unwrap_or_default(),
            window,
            cx,
        );
        self.loading_form = false;
        self.usage = None;
        self.usage_error = None;
        if self.active_page == PersonaPage::Context {
            self.refresh_usage(cx);
            self.refresh_context(window, cx);
            self.start_context_auto_refresh(window, cx);
        }
        cx.notify();
    }

    pub(super) fn select_persona(
        &mut self,
        index: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.select_persona_inner(index, true, window, cx);
    }

    fn select_persona_inner(
        &mut self,
        index: usize,
        persist: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.commit_context_edits(cx);
        self.capture_current_form(cx);
        let Some(persona) = self.draft.personas.get(index) else {
            return;
        };
        self.draft.selected = Some(persona.id.clone());
        self.load_form(Some(index), window, cx);
        if persist {
            self.save(cx);
        }
    }

    pub(super) fn add_persona(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.add_persona_inner(true, window, cx);
    }

    fn add_persona_inner(&mut self, persist: bool, window: &mut Window, cx: &mut Context<Self>) {
        self.commit_context_edits(cx);
        self.capture_current_form(cx);
        let id = next_persona_id(&self.draft, &self.pending_persona_cleanup);
        self.draft.personas.push(PersonaConfig::new(
            id.clone(),
            t!("persona.new_persona").to_string(),
        ));
        self.draft.selected = Some(id);
        self.load_form(self.draft.personas.len().checked_sub(1), window, cx);
        if persist {
            self.save(cx);
        }
    }

    /// 返回草稿中的人格 ID 列表，供测试断言增删与选择行为。
    #[cfg(test)]
    pub(crate) fn persona_ids_for_test(&self) -> Vec<String> {
        self.draft
            .personas
            .iter()
            .map(|persona| persona.id.clone())
            .collect()
    }

    /// 返回当前正在编辑的人格索引。
    #[cfg(test)]
    pub(crate) fn editing_index_for_test(&self) -> Option<usize> {
        self.editing_index
    }

    /// 返回草稿中当前选中的人格 ID。
    #[cfg(test)]
    pub(crate) fn selected_persona_for_test(&self) -> Option<&str> {
        self.draft.selected.as_deref()
    }

    /// 追加一个新人格条目。
    #[cfg(test)]
    pub(crate) fn add_persona_for_test(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.add_persona_inner(false, window, cx);
    }

    /// 切换到指定索引的人格条目。
    #[cfg(test)]
    pub(crate) fn select_persona_for_test(
        &mut self,
        index: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.select_persona_inner(index, false, window, cx);
    }

    /// 返回指定草稿条目当前绑定的供应商 ID。
    #[cfg(test)]
    pub(crate) fn bound_provider_for_test(&self, index: usize) -> Option<String> {
        self.draft
            .personas
            .get(index)
            .and_then(|persona| persona.model.clone())
    }

    /// 返回指定草稿条目当前绑定的 Live2D 相对路径。
    #[cfg(test)]
    pub(crate) fn bound_live2d_for_test(&self, index: usize) -> Option<PathBuf> {
        self.draft
            .personas
            .get(index)
            .and_then(|persona| persona.live2d_model.clone())
    }

    /// 返回两个上限输入的原始内容，供测试确认空值不会被默认值回填。
    #[cfg(test)]
    pub(crate) fn context_limit_inputs_for_test(&self, cx: &Context<Self>) -> [String; 2] {
        [
            self.context_messages_input.read(cx).value().to_string(),
            self.context_tokens_input.read(cx).value().to_string(),
        ]
    }

    /// 按生产路径解析两个上限输入，供测试确认空值沿用默认限制。
    #[cfg(test)]
    pub(crate) fn context_limits_for_test(&self, cx: &Context<Self>) -> PersonaContextLimits {
        self.capture_context_limits(cx)
    }
}

fn subscribe_form_input(
    input: &Entity<InputState>,
    single_line: bool,
    window: &mut Window,
    cx: &mut Context<PersonaSettingsView>,
) -> Subscription {
    cx.subscribe_in(
        input,
        window,
        move |this, input, event: &InputEvent, window, cx| match event {
            InputEvent::Focus => {
                if !this.loading_form {
                    this.input_edit = Some(InputEditSession::begin(input, cx));
                }
            }
            InputEvent::PressEnter { .. } if single_line => {
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
            InputEvent::Change | InputEvent::PressEnter { .. } => {}
        },
    )
}

fn integer_input(
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
        .validate(|value, _| {
            value.chars().all(|character| character.is_ascii_digit()) && parse_u32(value).is_some()
        })
        .step(1.0)
        .min(f64::from(min))
        .max(f64::from(max))
}

fn parse_u32(value: &str) -> Option<u32> {
    let value = value.trim();
    (!value.is_empty()).then(|| value.parse().ok()).flatten()
}
