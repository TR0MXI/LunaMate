//! 组合人格设置草稿、表单、上下文、记忆、持久化与渲染职责。

mod context;
mod form;
mod memory;
mod options;
mod persistence;
mod render;
mod selection;

use std::{cell::Cell, collections::HashSet, path::PathBuf, rc::Rc};

use gpui::{
    Bounds, Entity, EventEmitter, FocusHandle, Pixels, Point, ScrollHandle, Subscription, Task,
};
use gpui_component::{
    input::{InputState, TextareaState},
    select::SelectState,
};
use lunamate_agent::config::{AppLanguage, PersonaSettings, SharedLlmSettings};
use lunamate_agent::memory::PersonaMemoryUsage;
use lunamate_agent::{AgentMemory, ChatRole};

use self::memory::PendingConfirm;
use super::InputEditSession;

pub(in crate::ui) use persistence::{
    ContextMutationCompletion, PersonaSettingsDraft, PersonaSettingsEvent,
};

#[cfg(test)]
pub(in crate::ui) use memory::MemoryScope;
#[cfg(test)]
pub(crate) use options::{
    next_persona_id_for_test, provider_option_index_for_test, tts_model_option_index_for_test,
};

/// 具体人格编辑页的五个固定分区。
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
enum PersonaPage {
    #[default]
    Definition,
    Context,
    MediumMemory,
    LongMemory,
    Settings,
}

#[derive(Clone)]
struct Live2dModelOption {
    label: String,
    path: PathBuf,
}

struct ContextSelectionDrag {
    anchor_id: Option<u64>,
    base: HashSet<u64>,
    /// 去除滚动偏移后的内容坐标，滚动时始终锚定原始消息位置。
    start: Point<Pixels>,
    current: Point<Pixels>,
    /// 最新鼠标窗口坐标；滚动时用它推进内容坐标中的选择终点。
    cursor: Point<Pixels>,
    moved: bool,
    additive: bool,
}

#[derive(Clone, Copy, Default)]
struct ContextMessageLayout {
    bounds: Bounds<Pixels>,
    scroll_offset: Point<Pixels>,
}

impl ContextMessageLayout {
    fn new(bounds: Bounds<Pixels>, scroll_offset: Point<Pixels>) -> Self {
        Self {
            bounds,
            scroll_offset,
        }
    }
}

/// 当前上下文中一条可直接编辑的消息。
struct ContextMessageEditor {
    id: u64,
    role: ChatRole,
    input: Entity<TextareaState>,
    layout: Rc<Cell<ContextMessageLayout>>,
    saved_content: String,
    tokens: usize,
    fixed_tokens: usize,
}

/// 不暴露人格配置类型的退出边界写入。
pub(in crate::ui) struct PersonaSettingsDraftWrite {
    pub(super) settings: PersonaSettings,
    pub(super) revision: u64,
    pub(super) language: AppLanguage,
}

/// 设置窗口中的人格编辑器。
pub(in crate::ui) struct PersonaSettingsView {
    draft: PersonaSettings,
    providers: SharedLlmSettings,
    editing_index: Option<usize>,
    active_page: PersonaPage,
    name_input: Entity<InputState>,
    system_prompt_input: Entity<TextareaState>,
    input_prompt_input: Entity<TextareaState>,
    provider_select: Entity<SelectState<Vec<gpui::SharedString>>>,
    tts_select: Entity<SelectState<Vec<gpui::SharedString>>>,
    live2d_select: Entity<SelectState<Vec<gpui::SharedString>>>,
    live2d_models: Vec<Live2dModelOption>,
    missing_live2d_model: Option<PathBuf>,
    context_messages_input: Entity<InputState>,
    context_tokens_input: Entity<InputState>,
    context_editors: Vec<ContextMessageEditor>,
    context_subscriptions: Vec<Subscription>,
    form_subscriptions: Vec<Subscription>,
    context_selected: HashSet<u64>,
    context_selection_drag: Option<ContextSelectionDrag>,
    context_selection_auto_scroll_revision: u64,
    context_selection_auto_scroll_task: Option<Task<()>>,
    context_view_bounds: Rc<Cell<Bounds<Pixels>>>,
    context_editing: Option<u64>,
    context_focus: FocusHandle,
    context_scroll: ScrollHandle,
    context_loading: bool,
    context_error: Option<String>,
    context_revision: u64,
    context_task: Option<Task<()>>,
    context_auto_refresh_revision: u64,
    observed_live_context_revision: Option<u64>,
    context_auto_refresh_task: Option<Task<()>>,
    memory: AgentMemory,
    usage: Option<PersonaMemoryUsage>,
    usage_error: Option<String>,
    usage_revision: u64,
    usage_task: Option<Task<()>>,
    pending_confirm: Option<PendingConfirm>,
    status: Option<String>,
    loading_form: bool,
    input_edit: Option<InputEditSession>,
    submitted_draft: PersonaSettings,
    save_revision: u64,
    config_writes_in_flight: usize,
    window_transferred: bool,
    pending_persona_cleanup: HashSet<String>,
    persona_cleanup_in_flight: HashSet<String>,
    toast_revision: u64,
    toast_task: Option<Task<()>>,
    write_tasks: Vec<Task<()>>,
}

impl EventEmitter<PersonaSettingsEvent> for PersonaSettingsView {}
