//! 处理供应商模型条目的增删、选择与能力种类切换。

use gpui::{Context, Window};
use lunamate_agent::config::{LlmAdvancedOptions, LlmModelConfig, LlmSettings, ModelKind};
use rust_i18n::t;

use super::{ProviderSettingsView, options::default_provider};

impl ProviderSettingsView {
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
            voice: (self.active_kind == ModelKind::SpeechSynthesis).then(|| "alloy".to_owned()),
            voice_type: None,
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
    pub(crate) const fn active_kind_for_test(&self) -> ModelKind {
        self.active_kind
    }

    #[cfg(test)]
    pub(crate) fn model_kinds_for_test(&self) -> Vec<ModelKind> {
        self.draft.models.iter().map(|model| model.kind).collect()
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

/// 暴露新模型 ID 分配规则，供测试断言不会与既有条目冲突。
#[cfg(test)]
pub(crate) fn next_model_id_for_test(settings: &LlmSettings) -> String {
    next_model_id(settings)
}
