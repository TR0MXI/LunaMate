//! 保存语言模型设置草稿，处理模型编辑动作并显式发布完整快照。

mod render;

use std::{sync::Arc, time::Duration};

use gpui::{AppContext, Context, Entity, EventEmitter, SharedString, Task, Window};
use gpui_component::{IndexPath, input::InputState, select::SelectState};
use rust_i18n::t;

use super::{CONFIG, LLM_PROVIDERS, LlmModelConfig, LlmProvider, LlmSettings, SharedLlmSettings};

/// 语言模型配置成功发布后通知配置窗口和聊天视图。
#[derive(Clone, Debug)]
pub(super) struct LlmViewEvent(pub(super) SharedLlmSettings);

/// 设置窗口中的语言模型编辑器。
pub(super) struct LlmSettingsView {
    draft: LlmSettings,
    editing_index: Option<usize>,
    label_input: Entity<InputState>,
    model_input: Entity<InputState>,
    endpoint_input: Entity<InputState>,
    api_key_input: Entity<InputState>,
    system_prompt_input: Entity<InputState>,
    provider_select: Entity<SelectState<Vec<SharedString>>>,
    status: Option<String>,
    is_saving: bool,
    toast_revision: u64,
    toast_task: Option<Task<()>>,
    write_tasks: Vec<Task<()>>,
}

impl LlmSettingsView {
    /// 从当前运行时配置创建可丢弃的设置草稿。
    pub(super) fn new(
        settings: SharedLlmSettings,
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
        let draft = settings.as_ref().clone();
        let editing_index = draft
            .selected_model
            .as_deref()
            .and_then(|selected| draft.models.iter().position(|model| model.id == selected))
            .or_else(|| (!draft.models.is_empty()).then_some(0));
        let editing_model = editing_index.and_then(|index| draft.models.get(index));
        let provider = editing_model
            .map(|model| model.provider)
            .unwrap_or(LlmProvider::Ollama);
        let provider_names = LLM_PROVIDERS
            .into_iter()
            .map(|provider| SharedString::from(provider.display_name()))
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
        let system_prompt_input = cx.new(|cx| {
            InputState::new(window, cx)
                .multi_line(true)
                .rows(8)
                .placeholder(t!("llm.system_prompt").to_string())
                .default_value(draft.system_prompt.clone())
        });
        let provider_select = cx.new(|cx| {
            SelectState::new(provider_names, provider_index, window, cx).searchable(true)
        });

        Self {
            draft,
            editing_index,
            label_input,
            model_input,
            endpoint_input,
            api_key_input,
            system_prompt_input,
            provider_select,
            status: None,
            is_saving: false,
            toast_revision: 0,
            toast_task: None,
            write_tasks: Vec::new(),
        }
    }

    /// 保存窗口草稿并转移尚未结束的写任务，供关闭后重新创建编辑器。
    pub(super) fn take_window_state(
        &mut self,
        cx: &mut Context<Self>,
    ) -> (SharedLlmSettings, Vec<Task<()>>) {
        self.capture_current_form(cx);
        self.draft.system_prompt = self.system_prompt_input.read(cx).value().to_string();
        (
            Arc::new(self.draft.clone()),
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

    fn capture_current_form(&mut self, cx: &mut Context<Self>) {
        let Some(index) = self.editing_index else {
            return;
        };
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
            .and_then(|value| LlmProvider::from_display_name(value.as_ref()))
            .unwrap_or(LlmProvider::Ollama);
    }

    fn load_form(&mut self, index: Option<usize>, window: &mut Window, cx: &mut Context<Self>) {
        self.editing_index = index;
        let model = index.and_then(|index| self.draft.models.get(index));
        let provider = model
            .map(|model| model.provider)
            .unwrap_or(LlmProvider::Ollama);
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
            let value = SharedString::from(provider.display_name());
            select.set_selected_value(&value, window, cx);
        });
        cx.notify();
    }

    fn select_model(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
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

    fn add_model(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.is_saving {
            return;
        }
        self.capture_current_form(cx);
        let id = next_model_id(&self.draft);
        let mut model = LlmModelConfig::draft(id.clone());
        model.label = t!("llm.new_model").to_string();
        self.draft.models.push(model);
        self.draft.selected_model = Some(id);
        self.load_form(self.draft.models.len().checked_sub(1), window, cx);
        cx.notify();
    }

    fn delete_model(&mut self, window: &mut Window, cx: &mut Context<Self>) {
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

    fn save(&mut self, cx: &mut Context<Self>) {
        if self.is_saving {
            return;
        }
        self.capture_current_form(cx);
        self.draft.system_prompt = self.system_prompt_input.read(cx).value().to_string();
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
                    Ok(Some(settings)) => {
                        cx.emit(LlmViewEvent(settings));
                        t!("llm.saved").to_string()
                    }
                    Ok(None) => t!("llm.save_replaced").to_string(),
                    Err(error) => t!("llm.save_failed", error = error.to_string()).to_string(),
                };
                this.set_status(status, cx);
            });
        });
        self.write_tasks.push(task);
    }
}

impl EventEmitter<LlmViewEvent> for LlmSettingsView {}

fn set_input(
    input: &Entity<InputState>,
    value: &str,
    window: &mut Window,
    cx: &mut Context<LlmSettingsView>,
) {
    input.update(cx, |input, cx| input.set_value(value, window, cx));
}

fn non_empty(value: &str) -> Option<String> {
    let value = value.trim();
    (!value.is_empty()).then(|| value.to_owned())
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
