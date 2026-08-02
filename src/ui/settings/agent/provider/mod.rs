//! 组合供应商设置草稿、表单、选择、持久化、选项与渲染职责。

mod form;
mod options;
mod persistence;
mod render;
mod selection;

use gpui::{Entity, EventEmitter, SharedString, Subscription, Task};
use gpui_component::{input::InputState, select::SelectState};
use lunamate_agent::config::{LlmSettings, ModelKind};

use super::InputEditSession;

pub(in crate::ui) use persistence::{ProviderSettingsDraft, ProviderSettingsEvent};

#[cfg(test)]
pub(crate) use options::{
    provider_from_display_name_for_test, reasoning_index_for_test, reasoning_option_count_for_test,
};
#[cfg(test)]
pub(crate) use selection::next_model_id_for_test;

/// 设置窗口中的供应商编辑器。
pub(in crate::ui) struct ProviderSettingsView {
    draft: LlmSettings,
    active_kind: ModelKind,
    editing_index: Option<usize>,
    label_input: Entity<InputState>,
    model_input: Entity<InputState>,
    endpoint_input: Entity<InputState>,
    api_key_input: Entity<InputState>,
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
    advanced_expanded: bool,
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

impl EventEmitter<ProviderSettingsEvent> for ProviderSettingsView {}
