//! 保存人格设置草稿，处理人格增删与记忆清除，并发布配置变更。
//!
//! 记忆的删除是不可逆操作，因此所有清除入口都先进入 [`PendingConfirm`] 状态，
//! 只有用户在二次确认框中明确同意后才会派发实际删除任务。

use std::{
    cell::Cell,
    collections::HashSet,
    path::{Path, PathBuf},
    rc::Rc,
    sync::Arc,
    time::Duration,
};

use gpui::{
    AppContext, Bounds, ClipboardItem, Context, Entity, EventEmitter, Modifiers, MouseDownEvent,
    MouseMoveEvent, MouseUpEvent, Pixels, Point, ScrollHandle, SharedString, Subscription, Task,
    Window,
};
use gpui_component::{
    IndexPath,
    input::{InputEvent, InputState, MaskPattern},
    select::{SelectEvent, SelectState},
};
use gpui_tokio::Tokio;
use lunamate_agent::config::{
    AppLanguage, CONTEXT_MESSAGES_MAX, CONTEXT_MESSAGES_MIN, CONTEXT_TOKENS_MAX,
    CONTEXT_TOKENS_MIN, PersonaConfig, PersonaContextLimits, PersonaSettings, SharedLlmSettings,
    SharedPersonaSettings,
};
use lunamate_agent::memory::{
    ContextMessage, ContextUsage, PersistentMemoryScope, PersonaMemoryUsage,
};
use lunamate_agent::{AgentMemory, chat_limits};
use lunamate_agent::{ChatRole, MAX_SESSION_TEXT_BYTES, context_message_tokens};
use rust_i18n::t;

use crate::config::CONFIG;

use super::{provider_display_name, set_input};

/// 人格绑定供应商的第一项固定表示"跟随全局默认供应商"。
const BOUND_PROVIDER_INHERIT: &str = "\u{2014}";
/// Live2D 绑定的第一项固定表示跟随全局模型设置。
const BOUND_LIVE2D_INHERIT: &str = "\u{2014}";
const CONTEXT_AUTO_REFRESH_INTERVAL: Duration = Duration::from_millis(750);

/// 具体人格编辑页的五个固定分区。
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(super) enum PersonaPage {
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
    anchor_id: u64,
    base: HashSet<u64>,
    start: Point<Pixels>,
    current: Point<Pixels>,
    moved: bool,
    additive: bool,
}

/// 会话持有者完成单条上下文修改后返回设置页的结果通道。
pub(crate) type ContextMutationCompletion = async_channel::Sender<Result<(), String>>;

/// 设置窗口重建时保留的人格草稿，不向 UI 暴露配置类型。
#[derive(Clone)]
pub(in crate::ui) struct PersonaSettingsDraft {
    settings: SharedPersonaSettings,
    reserved_cleanup: HashSet<String>,
}

impl PersonaSettingsDraft {
    /// 从当前已发布配置创建设置窗口草稿。
    pub(in crate::ui) fn current() -> Self {
        let settings = CONFIG.persona_settings();
        let reserved_cleanup = settings.pending_deletions.iter().cloned().collect();
        Self {
            settings,
            reserved_cleanup,
        }
    }

    #[cfg(test)]
    pub(in crate::ui) fn from_settings_for_test(settings: PersonaSettings) -> Self {
        let reserved_cleanup = settings.pending_deletions.iter().cloned().collect();
        Self {
            settings: Arc::new(settings),
            reserved_cleanup,
        }
    }

    /// 为退出边界准备最新转移草稿的独立写入；配置未变化时无需提交。
    pub(in crate::ui) fn prepare_write(&self) -> Option<PersonaSettingsDraftWrite> {
        (self.settings.as_ref() != CONFIG.persona_settings().as_ref()).then(|| {
            PersonaSettingsDraftWrite {
                settings: self.settings.as_ref().clone(),
                revision: CONFIG.reserve_persona_settings_revision(),
                language: CONFIG.agent_config_snapshot().language(),
            }
        })
    }

    /// 只在转移草稿中移除一个已完成清理的 tombstone。
    pub(in crate::ui) fn finish_persona_cleanup(&mut self, persona: &str) -> bool {
        let mut settings = self.settings.as_ref().clone();
        let previous_len = settings.pending_deletions.len();
        settings
            .pending_deletions
            .retain(|pending| pending != persona);
        if settings.pending_deletions.len() == previous_len {
            return false;
        }
        self.settings = Arc::new(settings);
        true
    }

    /// tombstone 已发布移除后才释放转移草稿中的 ID 保留。
    pub(in crate::ui) fn persona_cleanup_was_published(&mut self, persona: &str) {
        let _ = self.finish_persona_cleanup(persona);
        self.reserved_cleanup.remove(persona);
    }
}

/// 不暴露人格配置类型的退出边界写入。
pub(in crate::ui) struct PersonaSettingsDraftWrite {
    settings: PersonaSettings,
    revision: u64,
    language: AppLanguage,
}

impl PersonaSettingsDraftWrite {
    pub(in crate::ui) fn persist(self) -> Result<bool, String> {
        CONFIG
            .set_persona_settings_at_revision(self.settings, self.revision, self.language)
            .map(|published| published.is_some())
            .map_err(|error| error.to_string())
    }
}

/// 人格设置向设置窗口发布的变更。
#[derive(Clone, Debug)]
pub(in crate::ui) enum PersonaSettingsEvent {
    /// 人格配置已发布，桌宠视图应重新读取人格、供应商与上下文。
    Saved,
    /// 配置与删除清理任务均已结束；只用于释放关闭窗口后保留的编辑器实体。
    SaveFinished,
    /// 关闭窗口后保留的旧编辑器完成了删除清理，当前编辑器应移除对应 tombstone。
    CleanupFinished { persona: String },
    /// 指定人格的短期上下文需要由持有会话的视图清除。
    ClearContext {
        persona: String,
        completion: Option<ContextMutationCompletion>,
    },
    /// 由持有会话的视图修改指定消息，避免设置页直接写会话文档。
    EditContextMessage {
        persona: String,
        message_id: u64,
        content: String,
        completion: Option<ContextMutationCompletion>,
    },
    /// 由持有会话的视图原子删除指定消息。
    DeleteContextMessages {
        persona: String,
        message_ids: Vec<u64>,
        completion: Option<ContextMutationCompletion>,
    },
}

/// 等待二次确认的危险操作。
#[derive(Clone, Debug, Eq, PartialEq)]
enum PendingConfirm {
    /// 清除指定人格的某一类或全部记忆。
    ClearMemory { persona: String, scope: MemoryScope },
    /// 删除指定人格，并同步删除其绑定的全部记忆。
    DeletePersona { persona: String },
    /// 删除指定人格当前上下文中的一组消息。
    DeleteContextMessages {
        persona: String,
        message_ids: Vec<u64>,
    },
}

/// 设置界面的完整记忆清理范围；短期上下文由活动会话持有者单独执行。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(in crate::ui) enum MemoryScope {
    Context,
    All,
}

impl MemoryScope {
    const fn id(self) -> &'static str {
        match self {
            Self::Context => "context",
            Self::All => "all",
        }
    }

    const fn persistent(self) -> Option<PersistentMemoryScope> {
        match self {
            Self::Context => None,
            Self::All => Some(PersistentMemoryScope::All),
        }
    }
}

/// 当前上下文中一条可直接编辑的消息。
pub(super) struct ContextMessageEditor {
    pub(super) id: u64,
    pub(super) role: ChatRole,
    pub(super) input: Entity<InputState>,
    saved_content: String,
    tokens: usize,
    fixed_tokens: usize,
}

/// 设置窗口中的人格编辑器。
pub(in crate::ui) struct PersonaSettingsView {
    draft: PersonaSettings,
    providers: SharedLlmSettings,
    editing_index: Option<usize>,
    active_page: PersonaPage,
    name_input: Entity<InputState>,
    system_prompt_input: Entity<InputState>,
    input_prompt_input: Entity<InputState>,
    provider_select: Entity<SelectState<Vec<SharedString>>>,
    live2d_select: Entity<SelectState<Vec<SharedString>>>,
    live2d_models: Vec<Live2dModelOption>,
    missing_live2d_model: Option<PathBuf>,
    context_messages_input: Entity<InputState>,
    context_tokens_input: Entity<InputState>,
    context_editors: Vec<ContextMessageEditor>,
    context_subscriptions: Vec<Subscription>,
    form_subscriptions: Vec<Subscription>,
    context_selected: HashSet<u64>,
    context_selection_drag: Option<ContextSelectionDrag>,
    context_view_bounds: Rc<Cell<Bounds<Pixels>>>,
    context_editing: Option<u64>,
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
                provider_option_names(&providers),
                Some(IndexPath::new(provider_option_index(
                    &providers,
                    editing.and_then(|persona| persona.model.as_deref()),
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
            context_view_bounds: Rc::new(Cell::new(Bounds::default())),
            context_editing: None,
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
            subscribe_form_input(&view.name_input, window, cx),
            subscribe_form_input(&view.system_prompt_input, window, cx),
            subscribe_form_input(&view.input_prompt_input, window, cx),
            subscribe_form_input(&view.context_messages_input, window, cx),
            subscribe_form_input(&view.context_tokens_input, window, cx),
            cx.subscribe(
                &view.provider_select,
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

    /// 保存窗口草稿并转移尚未结束的写任务，供关闭后重新创建编辑器。
    pub(in crate::ui) fn take_window_state(
        &mut self,
        cx: &mut Context<Self>,
    ) -> (PersonaSettingsDraft, Vec<Task<()>>, bool) {
        self.stop_context_auto_refresh();
        self.save(cx);
        self.window_transferred = true;
        let retain_editor =
            self.config_writes_in_flight != 0 || !self.persona_cleanup_in_flight.is_empty();
        (
            PersonaSettingsDraft {
                settings: Arc::new(self.draft.clone()),
                reserved_cleanup: self.pending_persona_cleanup.clone(),
            },
            std::mem::take(&mut self.write_tasks),
            retain_editor,
        )
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
        let names = provider_option_names(&self.providers);
        let index = provider_option_index(&self.providers, bound.as_deref());
        self.loading_form = true;
        self.provider_select.update(cx, |select, cx| {
            select.set_items(names, window, cx);
            select.set_selected_index(Some(IndexPath::new(index)), window, cx);
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
        let bound = self.selected_provider_id(cx);
        let live2d_model = self.selected_live2d_model(cx);
        let context = self.capture_context_limits(cx);
        let Some(persona) = self.draft.personas.get_mut(index) else {
            return;
        };
        persona.name = self.name_input.read(cx).value().to_string();
        persona.system_prompt = self.system_prompt_input.read(cx).value().to_string();
        persona.input_prompt = self.input_prompt_input.read(cx).value().to_string();
        persona.model = bound;
        persona.live2d_model = live2d_model;
        persona.context = context;
    }

    fn capture_context_limits(&self, cx: &Context<Self>) -> PersonaContextLimits {
        PersonaContextLimits {
            max_messages: parse_u32(self.context_messages_input.read(cx).value().as_ref()),
            max_tokens: parse_u32(self.context_tokens_input.read(cx).value().as_ref()),
        }
    }

    fn selected_provider_id(&self, cx: &Context<Self>) -> Option<String> {
        let row = self
            .provider_select
            .read(cx)
            .selected_index(cx)
            .map_or(0, |index| index.row);
        row.checked_sub(1)
            .and_then(|index| self.providers.models.get(index))
            .map(|model| model.id.clone())
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

    fn load_form(&mut self, index: Option<usize>, window: &mut Window, cx: &mut Context<Self>) {
        self.loading_form = true;
        self.editing_index = index;
        let persona = index.and_then(|index| self.draft.personas.get(index));
        let context = persona.map(|persona| persona.context).unwrap_or_default();
        let bound = persona.and_then(|persona| persona.model.clone());
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
        let provider_index = provider_option_index(&self.providers, bound.as_deref());
        self.provider_select.update(cx, |select, cx| {
            select.set_selected_index(Some(IndexPath::new(provider_index)), window, cx);
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

    /// 请求删除指定人格；实际删除在二次确认后执行。
    pub(super) fn request_delete_persona(&mut self, persona_id: String, cx: &mut Context<Self>) {
        let Some(persona) = self
            .draft
            .personas
            .iter()
            .find(|persona| persona.id == persona_id)
        else {
            return;
        };
        // 人格 ID 同时是记忆的归属键，必须始终保留一个可管理的入口。
        if self.draft.personas.len() <= 1 {
            self.set_status(t!("persona.error.empty").to_string(), cx);
            return;
        }
        self.pending_confirm = Some(PendingConfirm::DeletePersona {
            persona: persona.id.clone(),
        });
        cx.notify();
    }

    /// 请求清除某一类记忆；实际删除在二次确认后执行。
    pub(super) fn request_clear_memory(&mut self, scope: MemoryScope, cx: &mut Context<Self>) {
        let Some(persona) = self
            .editing_index
            .and_then(|index| self.draft.personas.get(index))
        else {
            return;
        };
        self.pending_confirm = Some(PendingConfirm::ClearMemory {
            persona: persona.id.clone(),
            scope,
        });
        cx.notify();
    }

    pub(super) fn cancel_confirm(&mut self, cx: &mut Context<Self>) {
        self.pending_confirm = None;
        cx.notify();
    }

    pub(super) fn accept_confirm(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(pending) = self.pending_confirm.take() else {
            return;
        };
        match pending {
            PendingConfirm::ClearMemory { persona, scope } => {
                log::info!("用户已确认清除人格记忆：scope={}", scope.id());
                self.clear_memory(persona, scope, false, cx);
            }
            PendingConfirm::DeletePersona { persona } => {
                log::info!("用户已确认删除人格及其记忆");
                self.delete_persona(persona, window, cx);
            }
            PendingConfirm::DeleteContextMessages {
                persona,
                message_ids,
            } => {
                log::info!("用户已确认删除上下文消息：count={}", message_ids.len());
                self.delete_context_messages(persona, message_ids, window, cx);
            }
        }
    }

    fn clear_memory(
        &mut self,
        persona: String,
        scope: MemoryScope,
        deletion_cleanup: bool,
        cx: &mut Context<Self>,
    ) {
        let context_result = if matches!(scope, MemoryScope::Context | MemoryScope::All) {
            // 会话文档只有持有会话的视图会写入，清除也必须由它执行才能避免竞争。
            let (sender, receiver) = async_channel::bounded(1);
            cx.emit(PersonaSettingsEvent::ClearContext {
                persona: persona.clone(),
                completion: Some(sender),
            });
            Some(receiver)
        } else {
            None
        };
        let memory = self.memory.persona(&persona);
        let persistent_scope = scope.persistent();
        let memory_access = self.memory.clone();
        let cleanup_persona = persona.clone();
        let task = persistent_scope
            .map(|scope| Tokio::spawn(cx, async move { memory.clear(scope).await }));
        self.set_status(t!("persona.memory_clearing").to_string(), cx);
        let track = cx.spawn(async move |this, cx| {
            let stored_result = match task {
                Some(task) => match task.await {
                    Ok(Ok(())) => Ok(()),
                    Ok(Err(error)) => Err(error.to_string()),
                    Err(error) => Err(error.to_string()),
                },
                None => Ok(()),
            };
            let context_result = match context_result {
                Some(receiver) => receiver
                    .recv()
                    .await
                    .unwrap_or_else(|_| Err(t!("persona.context_owner_unavailable").to_string())),
                None => Ok(()),
            };
            let result = match (context_result, stored_result) {
                (Ok(()), Ok(())) => Ok(()),
                (Err(context), Ok(())) => Err(context),
                (Ok(()), Err(stored)) => Err(stored),
                (Err(context), Err(stored)) => Err(format!("{context}; {stored}")),
            };
            if deletion_cleanup {
                if result.is_ok() {
                    memory_access.complete_deleted_persona_cleanup(&cleanup_persona);
                } else {
                    memory_access.fail_deleted_persona_cleanup(&cleanup_persona);
                }
            }
            if result.is_ok() {
                log::info!("人格记忆清除完成：scope={}", scope.id());
            } else {
                log::error!("人格记忆清除失败：scope={}", scope.id());
            }
            let _ = this.update(cx, |this, cx| {
                let is_current = this
                    .editing_index
                    .and_then(|index| this.draft.personas.get(index))
                    .is_some_and(|current| current.id == persona);
                this.persona_cleanup_in_flight.remove(&persona);
                if result.is_ok()
                    && is_current
                    && matches!(scope, MemoryScope::Context | MemoryScope::All)
                {
                    this.context_revision = this.context_revision.wrapping_add(1).max(1);
                    this.context_task = None;
                    this.context_loading = false;
                    this.context_error = None;
                    this.context_editors.clear();
                    this.context_subscriptions.clear();
                    this.context_selected.clear();
                    this.context_selection_drag = None;
                    if let Some(usage) = &mut this.usage {
                        usage.context.messages = 0;
                        usage.context.tokens = 0;
                    }
                }
                let completed_persona_deletion = result.is_ok()
                    && scope == MemoryScope::All
                    && !this
                        .draft
                        .personas
                        .iter()
                        .any(|current| current.id == persona);
                if completed_persona_deletion && !this.window_transferred {
                    this.draft
                        .pending_deletions
                        .retain(|pending| pending != &persona);
                }
                let status = match result {
                    Ok(()) => t!("persona.memory_cleared").to_string(),
                    Err(error) => t!("persona.memory_clear_failed", error = error).to_string(),
                };
                this.set_status(status, cx);
                if is_current && this.active_page == PersonaPage::Context {
                    this.refresh_usage(cx);
                }
                if completed_persona_deletion && this.window_transferred {
                    cx.emit(PersonaSettingsEvent::CleanupFinished {
                        persona: persona.clone(),
                    });
                    this.emit_save_finished_if_idle(cx);
                } else if completed_persona_deletion {
                    // 第二次配置提交只移除 tombstone；失败时磁盘记录会在下次打开设置后重试。
                    this.save(cx);
                } else {
                    this.emit_save_finished_if_idle(cx);
                }
            });
        });
        self.track_write_task(track);
        cx.notify();
    }

    fn delete_persona(&mut self, persona: String, window: &mut Window, cx: &mut Context<Self>) {
        self.delete_persona_inner(persona, true, window, cx);
    }

    fn delete_persona_inner(
        &mut self,
        persona: String,
        persist: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.commit_context_edits(cx);
        self.capture_current_form(cx);
        let Some(index) = self
            .draft
            .personas
            .iter()
            .position(|item| item.id == persona)
        else {
            return;
        };
        if self.draft.personas.len() <= 1 {
            self.set_status(t!("persona.error.empty").to_string(), cx);
            return;
        }
        let editing_id = self
            .editing_index
            .and_then(|editing| self.draft.personas.get(editing))
            .map(|persona| persona.id.clone());
        let deleted_was_selected = self.draft.selected.as_deref() == Some(persona.as_str());
        self.draft.personas.remove(index);
        self.pending_persona_cleanup.insert(persona.clone());
        if !self.draft.pending_deletions.contains(&persona) {
            self.draft.pending_deletions.push(persona);
        }
        if deleted_was_selected {
            let next_index = index.min(self.draft.personas.len() - 1);
            self.draft.selected = self
                .draft
                .personas
                .get(next_index)
                .map(|persona| persona.id.clone());
        }
        let next_editing = editing_id
            .as_deref()
            .and_then(|editing| {
                self.draft
                    .personas
                    .iter()
                    .position(|persona| persona.id == editing)
            })
            .or_else(|| {
                self.draft.selected.as_deref().and_then(|selected| {
                    self.draft
                        .personas
                        .iter()
                        .position(|persona| persona.id == selected)
                })
            })
            .or(Some(0));
        self.load_form(next_editing, window, cx);
        // 只有删除结果已经发布后才能清理该 ID 的记忆，避免配置失败造成孤立人格。
        if persist {
            self.save(cx);
        }
    }

    fn refresh_usage(&mut self, cx: &mut Context<Self>) {
        self.usage_revision = self.usage_revision.wrapping_add(1).max(1);
        let revision = self.usage_revision;
        self.usage_task = None;
        let Some(persona) = self
            .editing_index
            .and_then(|index| self.draft.personas.get(index))
        else {
            self.usage = None;
            self.usage_error = None;
            return;
        };
        if !self.memory.is_available() {
            self.usage = None;
            self.usage_error = Some(t!("persona.memory_unavailable").to_string());
            return;
        }

        let limits = context_limit_usage(persona, &self.providers);
        let memory = self.memory.persona(&persona.id);
        let live = self.memory.live_context_usage();
        let task = Tokio::spawn(cx, async move { memory.usage(live, limits).await });
        self.usage_task = Some(cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |this, cx| {
                // 期间可能又切换了人格；只有最新一次查询可以覆盖统计。
                if this.usage_revision != revision {
                    return;
                }
                this.usage_task = None;
                match result {
                    Ok(Ok(usage)) => {
                        this.usage = Some(usage);
                        this.usage_error = None;
                    }
                    Ok(Err(error)) => {
                        this.usage = None;
                        this.usage_error = Some(error.to_string());
                    }
                    Err(error) => {
                        this.usage = None;
                        this.usage_error = Some(error.to_string());
                    }
                }
                cx.notify();
            });
        }));
    }

    pub(super) fn select_page(
        &mut self,
        page: PersonaPage,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.active_page == page {
            return;
        }
        self.commit_context_edits(cx);
        self.capture_current_form(cx);
        self.save(cx);
        self.active_page = page;
        self.context_selection_drag = None;
        self.context_editing = None;
        if page == PersonaPage::Context {
            self.refresh_usage(cx);
            self.refresh_context(window, cx);
            self.start_context_auto_refresh(window, cx);
        } else {
            self.stop_context_auto_refresh();
            self.context_revision = self.context_revision.wrapping_add(1).max(1);
            self.context_task = None;
            self.context_loading = false;
        }
        cx.notify();
    }

    fn start_context_auto_refresh(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.context_auto_refresh_revision =
            self.context_auto_refresh_revision.wrapping_add(1).max(1);
        self.observed_live_context_revision = self
            .editing_index
            .and_then(|index| self.draft.personas.get(index))
            .and_then(|persona| self.memory.live_context_usage().revision_for(&persona.id));
        self.schedule_context_auto_refresh(window, cx);
    }

    fn stop_context_auto_refresh(&mut self) {
        self.context_auto_refresh_revision =
            self.context_auto_refresh_revision.wrapping_add(1).max(1);
        self.context_auto_refresh_task = None;
    }

    fn schedule_context_auto_refresh(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let generation = self.context_auto_refresh_revision;
        let live = self.memory.live_context_usage();
        let persona = self
            .editing_index
            .and_then(|index| self.draft.personas.get(index))
            .map(|persona| persona.id.clone());
        let background = cx.background_executor().clone();
        self.context_auto_refresh_task = Some(cx.spawn_in(window, async move |this, cx| {
            background.timer(CONTEXT_AUTO_REFRESH_INTERVAL).await;
            let live_revision = persona
                .as_deref()
                .and_then(|persona| live.revision_for(persona));
            let _ = cx.update(|window, app| {
                let _ = this.update(app, |this, cx| {
                    if this.active_page != PersonaPage::Context
                        || this.context_auto_refresh_revision != generation
                    {
                        return;
                    }
                    if live_revision.is_some()
                        && live_revision != this.observed_live_context_revision
                        && this.context_editing.is_none()
                        && this.context_selection_drag.is_none()
                        && this.context_selected.is_empty()
                    {
                        this.observed_live_context_revision = live_revision;
                        this.refresh_usage(cx);
                        this.refresh_context(window, cx);
                    }
                    this.schedule_context_auto_refresh(window, cx);
                });
            });
        }));
    }

    fn refresh_context(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.context_revision = self.context_revision.wrapping_add(1).max(1);
        let revision = self.context_revision;
        self.context_task = None;
        self.context_loading = true;
        self.context_editors.clear();
        self.context_subscriptions.clear();
        self.context_selected.clear();
        let Some(persona) = self
            .editing_index
            .and_then(|index| self.draft.personas.get(index))
        else {
            self.context_editors.clear();
            self.context_subscriptions.clear();
            self.context_selected.clear();
            self.context_loading = false;
            self.context_error = None;
            return;
        };
        let limits = context_limit_usage(persona, &self.providers);
        let memory = self.memory.persona(&persona.id);
        let live = self.memory.live_context_usage();
        let load = Tokio::spawn(
            cx,
            async move { memory.context_messages(live, limits).await },
        );
        self.context_error = None;
        self.context_task = Some(cx.spawn_in(window, async move |this, cx| {
            let result = load.await;
            let _ = cx.update(|window, app| {
                let _ = this.update(app, |this, cx| {
                    if this.context_revision != revision {
                        return;
                    }
                    this.context_task = None;
                    this.context_loading = false;
                    match result {
                        Ok(Ok(messages)) => {
                            this.replace_context_editors(messages, window, cx);
                            this.context_error = None;
                        }
                        Ok(Err(error)) => {
                            this.context_editors.clear();
                            this.context_subscriptions.clear();
                            this.context_selected.clear();
                            this.context_error = Some(error.to_string());
                        }
                        Err(error) => {
                            this.context_editors.clear();
                            this.context_subscriptions.clear();
                            this.context_selected.clear();
                            this.context_error = Some(error.to_string());
                        }
                    }
                    cx.notify();
                });
            });
        }));
        cx.notify();
    }

    fn replace_context_editors(
        &mut self,
        messages: Vec<ContextMessage>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.context_subscriptions.clear();
        self.context_selected.clear();
        let mut editors = Vec::with_capacity(messages.len());
        let mut subscriptions = Vec::with_capacity(messages.len());
        for message in messages {
            let input = cx.new(|cx| {
                InputState::new(window, cx)
                    .auto_grow(1, 12)
                    .default_value(message.content.clone())
            });
            let message_id = message.id;
            subscriptions.push(cx.subscribe_in(
                &input,
                window,
                move |this, input, event: &InputEvent, window, cx| match event {
                    InputEvent::Change => {
                        this.enforce_context_message_size(message_id, input.clone(), window, cx);
                    }
                    InputEvent::Blur => {
                        this.commit_context_message(message_id, input.clone(), Some(window), cx);
                        if this.context_editing == Some(message_id) {
                            this.context_editing = None;
                            cx.notify();
                        }
                    }
                    InputEvent::Focus | InputEvent::PressEnter { .. } => {}
                },
            ));
            editors.push(ContextMessageEditor {
                id: message.id,
                role: message.role,
                input,
                saved_content: message.content,
                tokens: message.tokens,
                fixed_tokens: message.fixed_tokens,
            });
        }
        self.context_editors = editors;
        self.context_subscriptions = subscriptions;
        self.context_scroll.scroll_to_bottom();
    }

    fn enforce_context_message_size(
        &mut self,
        message_id: u64,
        input: Entity<InputState>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if input.read(cx).value().len() <= MAX_SESSION_TEXT_BYTES {
            return;
        }
        let Some(saved) = self
            .context_editors
            .iter()
            .find(|editor| editor.id == message_id)
            .map(|editor| editor.saved_content.clone())
        else {
            return;
        };
        set_input(&input, &saved, window, cx);
        self.set_status(t!("persona.context_message_file_too_large").to_string(), cx);
    }

    fn commit_context_edits(&mut self, cx: &mut Context<Self>) {
        let pending = self
            .context_editors
            .iter()
            .map(|editor| (editor.id, editor.input.clone()))
            .collect::<Vec<_>>();
        for (message_id, input) in pending {
            self.commit_context_message(message_id, input, None, cx);
        }
    }

    fn commit_context_message(
        &mut self,
        message_id: u64,
        input: Entity<InputState>,
        window: Option<&mut Window>,
        cx: &mut Context<Self>,
    ) {
        if self.context_loading {
            return;
        }
        let value = input.read(cx).value().trim().to_owned();
        let Some(index) = self
            .context_editors
            .iter()
            .position(|editor| editor.id == message_id)
        else {
            return;
        };
        let saved = self.context_editors[index].saved_content.clone();
        if value == saved {
            return;
        }
        if value.is_empty() {
            if let Some(window) = window {
                set_input(&input, &saved, window, cx);
            }
            self.set_status(t!("persona.context_message_empty").to_string(), cx);
            return;
        }
        let new_tokens = context_message_tokens(&value, self.context_editors[index].fixed_tokens);
        let current_tokens = self.usage.map_or_else(
            || {
                self.context_editors
                    .iter()
                    .map(|editor| editor.tokens)
                    .sum()
            },
            |usage| usage.context.tokens,
        );
        let max_tokens = self
            .editing_index
            .and_then(|index| self.draft.personas.get(index))
            .cloned()
            .map(|mut persona| {
                persona.context = self.capture_context_limits(cx);
                persona.model = self.selected_provider_id(cx);
                context_limit_usage(&persona, &self.providers).max_tokens
            })
            .unwrap_or_default();
        let next_tokens = current_tokens
            .saturating_sub(self.context_editors[index].tokens)
            .saturating_add(new_tokens);
        if next_tokens > max_tokens {
            if let Some(window) = window {
                set_input(&input, &saved, window, cx);
            }
            self.set_status(t!("persona.context_message_too_large").to_string(), cx);
            return;
        }
        self.context_editors[index].saved_content = value.clone();
        self.context_editors[index].tokens = new_tokens;
        self.usage_revision = self.usage_revision.wrapping_add(1).max(1);
        self.usage_task = None;
        if let Some(usage) = &mut self.usage {
            usage.context.tokens = next_tokens;
        }
        let Some(persona) = self
            .editing_index
            .and_then(|index| self.draft.personas.get(index))
            .map(|persona| persona.id.clone())
        else {
            return;
        };
        let completion = Some(match window {
            Some(window) => self.watch_context_mutation(persona.clone(), None, window, cx),
            None => self.watch_context_mutation_without_window(persona.clone(), cx),
        });
        cx.emit(PersonaSettingsEvent::EditContextMessage {
            persona,
            message_id,
            content: value,
            completion,
        });
        cx.notify();
    }

    fn delete_context_messages(
        &mut self,
        persona: String,
        message_ids: Vec<u64>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let selected = message_ids.iter().copied().collect::<HashSet<_>>();
        if selected.is_empty() {
            return;
        }
        let editors = std::mem::take(&mut self.context_editors);
        let subscriptions = std::mem::take(&mut self.context_subscriptions);
        let mut removed_messages = 0_usize;
        let mut removed_tokens = 0_usize;
        for (editor, subscription) in editors.into_iter().zip(subscriptions) {
            if selected.contains(&editor.id) {
                removed_messages = removed_messages.saturating_add(1);
                removed_tokens = removed_tokens.saturating_add(editor.tokens);
            } else {
                self.context_editors.push(editor);
                self.context_subscriptions.push(subscription);
            }
        }
        if removed_messages == 0 {
            return;
        }
        self.usage_revision = self.usage_revision.wrapping_add(1).max(1);
        self.usage_task = None;
        self.context_selected.retain(|id| !selected.contains(id));
        if let Some(usage) = &mut self.usage {
            usage.context.messages = usage.context.messages.saturating_sub(removed_messages);
            usage.context.tokens = usage.context.tokens.saturating_sub(removed_tokens);
        }
        let completion = self.watch_context_mutation(
            persona.clone(),
            Some(t!("persona.context_messages_deleted", count = removed_messages).to_string()),
            window,
            cx,
        );
        cx.emit(PersonaSettingsEvent::DeleteContextMessages {
            persona,
            message_ids,
            completion: Some(completion),
        });
        cx.notify();
    }

    fn watch_context_mutation(
        &mut self,
        persona: String,
        success: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> ContextMutationCompletion {
        let (sender, receiver) = async_channel::bounded::<Result<(), String>>(1);
        cx.spawn_in(window, async move |this, cx| {
            let Ok(result) = receiver.recv().await else {
                return;
            };
            let _ = cx.update(|window, app| {
                let _ = this.update(app, |this, cx| {
                    match result {
                        Ok(()) => {
                            if let Some(success) = success {
                                this.set_status(success, cx);
                            }
                        }
                        Err(error) => {
                            let is_current = this
                                .editing_index
                                .and_then(|index| this.draft.personas.get(index))
                                .is_some_and(|current| current.id == persona);
                            if is_current {
                                this.refresh_usage(cx);
                                if this.active_page == PersonaPage::Context {
                                    this.refresh_context(window, cx);
                                }
                            }
                            this.set_status(
                                t!("persona.context_message_save_failed", error = error)
                                    .to_string(),
                                cx,
                            );
                        }
                    }
                    cx.notify();
                });
            });
        })
        .detach();
        sender
    }

    fn watch_context_mutation_without_window(
        &mut self,
        persona: String,
        cx: &mut Context<Self>,
    ) -> ContextMutationCompletion {
        let (sender, receiver) = async_channel::bounded::<Result<(), String>>(1);
        let task = cx.spawn(async move |this, cx| {
            let Ok(result) = receiver.recv().await else {
                return;
            };
            let _ = this.update(cx, |this, cx| {
                if let Err(error) = result {
                    let is_current = this
                        .editing_index
                        .and_then(|index| this.draft.personas.get(index))
                        .is_some_and(|current| current.id == persona);
                    if is_current {
                        this.context_error = Some(error.clone());
                        this.refresh_usage(cx);
                    }
                    this.set_status(
                        t!("persona.context_message_save_failed", error = error).to_string(),
                        cx,
                    );
                }
            });
        });
        self.track_write_task(task);
        sender
    }

    pub(super) fn save(&mut self, cx: &mut Context<Self>) {
        if self.loading_form {
            return;
        }
        self.commit_context_edits(cx);
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
            self.retry_pending_persona_cleanup(cx);
            return;
        }
        self.draft = normalized.clone();
        self.submitted_draft = normalized.clone();
        if self.active_page == PersonaPage::Context {
            self.refresh_usage(cx);
        }
        self.save_revision = self.save_revision.wrapping_add(1).max(1);
        let ui_revision = self.save_revision;
        self.config_writes_in_flight = self.config_writes_in_flight.saturating_add(1);
        let config_revision = CONFIG.reserve_persona_settings_revision();
        self.set_status(t!("persona.saving").to_string(), cx);
        let background = cx.background_executor().clone();

        let task = cx.spawn(async move |this, cx| {
            let result = background
                .spawn(async move {
                    CONFIG.set_persona_settings_at_revision(normalized, config_revision, language)
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                let latest = this.save_revision == ui_revision;
                this.config_writes_in_flight = this.config_writes_in_flight.saturating_sub(1);
                match result {
                    Ok(Some(published)) => {
                        cx.emit(PersonaSettingsEvent::Saved);
                        if latest {
                            this.set_status(t!("persona.saved").to_string(), cx);
                        }
                        for id in &this.pending_persona_cleanup {
                            if !published.pending_deletions.contains(id) {
                                this.memory.release_deleted_persona_cleanup(id);
                            }
                        }
                        this.pending_persona_cleanup.retain(|id| {
                            published.pending_deletions.contains(id)
                                || this.persona_cleanup_in_flight.contains(id)
                        });
                        this.retry_pending_persona_cleanup(cx);
                    }
                    Ok(None) => {
                        if latest {
                            this.submitted_draft = CONFIG.persona_settings().as_ref().clone();
                            this.set_status(t!("persona.save_replaced").to_string(), cx);
                        }
                    }
                    Err(error) => {
                        if latest {
                            this.submitted_draft = CONFIG.persona_settings().as_ref().clone();
                            this.set_status(
                                t!("persona.save_failed", error = error.to_string()).to_string(),
                                cx,
                            );
                        }
                    }
                }
                this.emit_save_finished_if_idle(cx);
            });
        });
        self.track_write_task(task);
    }

    fn retry_pending_persona_cleanup(&mut self, cx: &mut Context<Self>) {
        let published = CONFIG.persona_settings();
        self.pending_persona_cleanup
            .extend(published.pending_deletions.iter().cloned());
        self.pending_persona_cleanup.retain(|id| {
            published.pending_deletions.contains(id)
                || self.draft.pending_deletions.contains(id)
                || self.persona_cleanup_in_flight.contains(id)
        });
        let cleanup = self
            .draft
            .pending_deletions
            .iter()
            .filter(|id| {
                !self.persona_cleanup_in_flight.contains(*id)
                    && published.pending_deletions.contains(*id)
            })
            .cloned()
            .collect::<Vec<_>>();
        for persona in cleanup {
            if !self.memory.claim_deleted_persona_cleanup(&persona) {
                continue;
            }
            self.persona_cleanup_in_flight.insert(persona.clone());
            self.clear_memory(persona, MemoryScope::All, true, cx);
        }
    }

    /// 设置窗口重建后重试尚未发布的草稿和持久化 tombstone 对应的清理。
    pub(in crate::ui) fn resume_pending_work(&mut self, cx: &mut Context<Self>) {
        self.save(cx);
    }

    /// 合并旧窗口完成的清理结果，只修改当前草稿中的 tombstone。
    pub(in crate::ui) fn finish_persona_cleanup(&mut self, persona: &str, cx: &mut Context<Self>) {
        let previous_len = self.draft.pending_deletions.len();
        self.draft
            .pending_deletions
            .retain(|pending| pending != persona);
        if self.draft.pending_deletions.len() != previous_len {
            self.save(cx);
        }
    }

    /// 外部协调器已发布 tombstone 移除，只同步本地草稿与 ID 保留状态。
    pub(in crate::ui) fn persona_cleanup_was_published(
        &mut self,
        persona: &str,
        cx: &mut Context<Self>,
    ) {
        self.draft
            .pending_deletions
            .retain(|pending| pending != persona);
        self.submitted_draft
            .pending_deletions
            .retain(|pending| pending != persona);
        self.pending_persona_cleanup.remove(persona);
        cx.notify();
    }

    fn emit_save_finished_if_idle(&self, cx: &mut Context<Self>) {
        if self.config_writes_in_flight == 0 && self.persona_cleanup_in_flight.is_empty() {
            cx.emit(PersonaSettingsEvent::SaveFinished);
        }
    }

    fn track_write_task(&mut self, task: Task<()>) {
        // 只保留仍在执行的任务，避免长期打开设置窗口时无界累积句柄。
        self.write_tasks.retain(|task| !task.is_ready());
        self.write_tasks.push(task);
    }

    pub(super) fn draft(&self) -> &PersonaSettings {
        &self.draft
    }

    pub(super) fn providers(&self) -> &SharedLlmSettings {
        &self.providers
    }

    pub(super) fn editing_index(&self) -> Option<usize> {
        self.editing_index
    }

    pub(super) fn status(&self) -> Option<&str> {
        self.status.as_deref()
    }

    pub(super) fn usage(&self) -> Option<PersonaMemoryUsage> {
        self.usage
    }

    pub(super) fn usage_error(&self) -> Option<&str> {
        self.usage_error.as_deref()
    }

    pub(super) fn confirm_prompt(&self) -> Option<ConfirmPrompt> {
        let pending = self.pending_confirm.as_ref()?;
        let name = |id: &str| {
            self.draft
                .personas
                .iter()
                .find(|persona| persona.id == id)
                .map_or_else(|| id.to_owned(), |persona| persona.name.clone())
        };
        Some(match pending {
            PendingConfirm::ClearMemory { persona, scope } => ConfirmPrompt {
                title: t!("persona.confirm_clear_title").to_string(),
                message: t!(
                    "persona.confirm_clear_message",
                    persona = name(persona),
                    scope = memory_scope_name(*scope)
                )
                .to_string(),
                confirm: t!("persona.confirm_clear").to_string(),
            },
            PendingConfirm::DeletePersona { persona } => ConfirmPrompt {
                title: t!("persona.confirm_delete_title").to_string(),
                message: t!(
                    "persona.confirm_delete_persona_message",
                    persona = name(persona)
                )
                .to_string(),
                confirm: t!("persona.confirm_delete").to_string(),
            },
            PendingConfirm::DeleteContextMessages {
                persona,
                message_ids,
            } => ConfirmPrompt {
                title: t!("persona.confirm_delete_message_title").to_string(),
                message: t!(
                    "persona.confirm_delete_context_messages",
                    persona = name(persona),
                    count = message_ids.len()
                )
                .to_string(),
                confirm: t!(
                    "persona.confirm_delete_message_action",
                    count = message_ids.len()
                )
                .to_string(),
            },
        })
    }

    pub(super) fn form(&self) -> PersonaFormInputs<'_> {
        PersonaFormInputs {
            name: &self.name_input,
            system_prompt: &self.system_prompt_input,
            input_prompt: &self.input_prompt_input,
            provider: &self.provider_select,
            live2d: &self.live2d_select,
            context_messages: &self.context_messages_input,
            context_tokens: &self.context_tokens_input,
        }
    }

    pub(super) fn context_editors(&self) -> &[ContextMessageEditor] {
        &self.context_editors
    }

    pub(super) const fn context_scroll(&self) -> &ScrollHandle {
        &self.context_scroll
    }

    pub(super) fn context_message_selected(&self, message_id: u64) -> bool {
        self.context_selected.contains(&message_id)
    }

    pub(super) fn selected_context_messages(&self) -> usize {
        self.context_selected.len()
    }

    pub(super) fn context_loading(&self) -> bool {
        self.context_loading
    }

    pub(super) fn context_error(&self) -> Option<&str> {
        self.context_error.as_deref()
    }

    pub(super) fn active_page(&self) -> PersonaPage {
        self.active_page
    }

    pub(super) fn context_message_editing(&self, message_id: u64) -> bool {
        self.context_editing == Some(message_id)
    }

    pub(super) fn context_view_bounds(&self) -> Rc<Cell<Bounds<Pixels>>> {
        self.context_view_bounds.clone()
    }

    pub(super) fn context_selection_rect(&self) -> Option<(Point<Pixels>, Point<Pixels>)> {
        self.context_selection_drag
            .as_ref()
            .filter(|drag| drag.moved)
            .map(|drag| (drag.start, drag.current))
    }

    pub(super) fn begin_context_message_edit(
        &mut self,
        message_id: u64,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(input) = self
            .context_editors
            .iter()
            .find(|message| message.id == message_id)
            .map(|message| message.input.clone())
        else {
            return;
        };
        self.context_editing = Some(message_id);
        input.update(cx, |input, cx| input.focus(window, cx));
        cx.notify();
    }

    pub(super) fn prepare_context_menu_selection(
        &mut self,
        message_id: u64,
        cx: &mut Context<Self>,
    ) {
        self.context_selection_drag = None;
        if !self.context_selected.contains(&message_id) {
            self.context_selected.clear();
            self.context_selected.insert(message_id);
        }
        cx.notify();
    }

    pub(super) fn copy_selected_context_messages(&mut self, cx: &mut Context<Self>) {
        let text = self
            .context_editors
            .iter()
            .filter(|message| self.context_selected.contains(&message.id))
            .map(|message| message.input.read(cx).value().to_string())
            .collect::<Vec<_>>()
            .join("\n");
        if text.is_empty() {
            return;
        }
        cx.write_to_clipboard(ClipboardItem::new_string(text));
        self.set_status(t!("persona.context_copied").to_string(), cx);
    }

    pub(super) fn start_context_selection_drag(
        &mut self,
        message_id: u64,
        event: &MouseDownEvent,
        cx: &mut Context<Self>,
    ) {
        if self.context_loading {
            return;
        }
        self.context_selection_drag = Some(ContextSelectionDrag {
            anchor_id: message_id,
            base: self.context_selected.clone(),
            start: event.position,
            current: event.position,
            moved: false,
            additive: selection_is_additive(event.modifiers),
        });
        cx.notify();
    }

    pub(super) fn update_context_selection_drag(
        &mut self,
        message_id: u64,
        event: &MouseMoveEvent,
        cx: &mut Context<Self>,
    ) {
        let Some(drag) = &mut self.context_selection_drag else {
            return;
        };
        if !event.dragging() {
            self.context_selection_drag = None;
            cx.notify();
            return;
        }
        drag.current = event.position;
        let dx = f32::from(event.position.x - drag.start.x).abs();
        let dy = f32::from(event.position.y - drag.start.y).abs();
        drag.moved |= dx >= 3.0 || dy >= 3.0;
        if !drag.moved {
            cx.notify();
            return;
        }
        let anchor_id = drag.anchor_id;
        let additive = drag.additive;
        let base = drag.base.clone();
        let Some(anchor) = self
            .context_editors
            .iter()
            .position(|message| message.id == anchor_id)
        else {
            return;
        };
        let Some(current) = self
            .context_editors
            .iter()
            .position(|message| message.id == message_id)
        else {
            return;
        };
        let (start, end) = if anchor <= current {
            (anchor, current)
        } else {
            (current, anchor)
        };
        let mut selected = if additive { base } else { HashSet::new() };
        selected.extend(
            self.context_editors[start..=end]
                .iter()
                .map(|message| message.id),
        );
        self.context_selected = selected;
        cx.notify();
    }

    pub(super) fn update_context_selection_position(
        &mut self,
        event: &MouseMoveEvent,
        cx: &mut Context<Self>,
    ) {
        if let Some(drag) = &mut self.context_selection_drag
            && event.dragging()
        {
            drag.current = event.position;
            cx.notify();
        }
    }

    pub(super) fn finish_context_selection_drag(
        &mut self,
        _event: &MouseUpEvent,
        cx: &mut Context<Self>,
    ) {
        let Some(drag) = self.context_selection_drag.take() else {
            return;
        };
        if !drag.moved {
            self.context_selected = drag.base;
            if !self.context_selected.insert(drag.anchor_id) {
                self.context_selected.remove(&drag.anchor_id);
            }
        }
        cx.notify();
    }

    pub(super) fn request_delete_selected_context_messages(&mut self, cx: &mut Context<Self>) {
        let message_ids = self
            .context_editors
            .iter()
            .filter(|message| self.context_selected.contains(&message.id))
            .map(|message| message.id)
            .collect::<Vec<_>>();
        self.request_delete_context_messages(message_ids, cx);
    }

    pub(super) fn request_delete_context_messages(
        &mut self,
        message_ids: Vec<u64>,
        cx: &mut Context<Self>,
    ) {
        if self.context_loading || message_ids.is_empty() {
            return;
        }
        let Some(persona) = self
            .editing_index
            .and_then(|index| self.draft.personas.get(index))
        else {
            return;
        };
        let mut seen = HashSet::with_capacity(message_ids.len());
        let message_ids = message_ids
            .into_iter()
            .filter(|message_id| {
                seen.insert(*message_id)
                    && self
                        .context_editors
                        .iter()
                        .any(|message| message.id == *message_id)
            })
            .collect::<Vec<_>>();
        if message_ids.is_empty() {
            return;
        }
        self.pending_confirm = Some(PendingConfirm::DeleteContextMessages {
            persona: persona.id.clone(),
            message_ids,
        });
        cx.notify();
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

    /// 返回当前等待二次确认的操作描述，供测试断言危险操作不会立即执行。
    #[cfg(test)]
    pub(crate) fn pending_confirm_for_test(&self) -> Option<(String, Option<MemoryScope>)> {
        self.pending_confirm.as_ref().map(|pending| match pending {
            PendingConfirm::ClearMemory { persona, scope } => (persona.clone(), Some(*scope)),
            PendingConfirm::DeletePersona { persona } => (persona.clone(), None),
            PendingConfirm::DeleteContextMessages { persona, .. } => (persona.clone(), None),
        })
    }

    /// 返回当前确认框正文，供测试区分删除人格与删除消息的危险范围。
    #[cfg(test)]
    pub(crate) fn confirm_message_for_test(&self) -> Option<String> {
        self.confirm_prompt().map(|prompt| prompt.message)
    }

    /// 直接创建单条上下文删除确认，跳过异步上下文加载。
    #[cfg(test)]
    pub(crate) fn request_delete_context_confirmation_for_test(
        &mut self,
        message_id: u64,
        cx: &mut Context<Self>,
    ) {
        let Some(persona) = self
            .editing_index
            .and_then(|index| self.draft.personas.get(index))
        else {
            return;
        };
        self.pending_confirm = Some(PendingConfirm::DeleteContextMessages {
            persona: persona.id.clone(),
            message_ids: vec![message_id],
        });
        cx.notify();
    }

    /// 追加一个新人格条目。
    #[cfg(test)]
    pub(crate) fn add_persona_for_test(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.add_persona_inner(false, window, cx);
    }

    /// 请求删除当前人格，但不进行确认。
    #[cfg(test)]
    pub(crate) fn request_delete_persona_for_test(&mut self, cx: &mut Context<Self>) {
        let Some(persona) = self
            .editing_index
            .and_then(|index| self.draft.personas.get(index))
            .map(|persona| persona.id.clone())
        else {
            return;
        };
        self.request_delete_persona(persona, cx);
    }

    /// 直接执行测试删除但不写用户配置，供测试检查 tombstone 草稿。
    #[cfg(test)]
    pub(crate) fn delete_persona_for_test(
        &mut self,
        persona: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.delete_persona_inner(persona.to_owned(), false, window, cx);
    }

    #[cfg(test)]
    pub(crate) fn pending_deletions_for_test(&self) -> &[String] {
        &self.draft.pending_deletions
    }

    /// 请求清除指定范围的记忆，但不进行确认。
    #[cfg(test)]
    pub(crate) fn request_clear_memory_for_test(
        &mut self,
        scope: MemoryScope,
        cx: &mut Context<Self>,
    ) {
        self.request_clear_memory(scope, cx);
    }

    /// 取消等待中的二次确认。
    #[cfg(test)]
    pub(crate) fn cancel_confirm_for_test(&mut self, cx: &mut Context<Self>) {
        self.cancel_confirm(cx);
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

    /// 直接装载可编辑消息，供无头测试覆盖自适应输入构造而不依赖 Tokio 唤醒。
    #[cfg(test)]
    pub(crate) fn load_context_messages_for_test(
        &mut self,
        messages: Vec<ContextMessage>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.replace_context_editors(messages, window, cx);
        cx.notify();
    }

    /// 直接切换上下文页，避免无头测试启动数据库查询和配置写入。
    #[cfg(test)]
    pub(crate) fn show_context_for_test(&mut self, cx: &mut Context<Self>) {
        self.active_page = PersonaPage::Context;
        cx.notify();
    }

    /// 返回当前已构造的消息编辑器数量。
    #[cfg(test)]
    pub(crate) fn context_editor_count_for_test(&self) -> usize {
        self.context_editors.len()
    }

    /// 返回当前卡片顺序，供测试断言拖拽使用稳定消息 ID。
    #[cfg(test)]
    pub(crate) fn context_message_ids_for_test(&self) -> Vec<u64> {
        self.context_editors
            .iter()
            .map(|message| message.id)
            .collect()
    }

    /// 切换一条上下文消息的多选状态。
    #[cfg(test)]
    pub(crate) fn toggle_context_message_selected_for_test(
        &mut self,
        message_id: u64,
        cx: &mut Context<Self>,
    ) {
        if !self.context_selected.insert(message_id) {
            self.context_selected.remove(&message_id);
        }
        cx.notify();
    }

    /// 请求删除当前多选集合，但不进行确认。
    #[cfg(test)]
    pub(crate) fn request_delete_selected_context_messages_for_test(
        &mut self,
        cx: &mut Context<Self>,
    ) {
        self.request_delete_selected_context_messages(cx);
    }

    /// 返回当前多选消息数量。
    #[cfg(test)]
    pub(crate) fn selected_context_messages_for_test(&self) -> usize {
        self.context_selected.len()
    }

    /// 复制当前选择，供无头测试验证多选顺序与剪贴板内容。
    #[cfg(test)]
    pub(crate) fn copy_selected_context_messages_for_test(&mut self, cx: &mut Context<Self>) {
        self.copy_selected_context_messages(cx);
    }

    /// 返回上下文列表滚动偏移与最大偏移，供无头渲染验证默认置底。
    #[cfg(test)]
    pub(crate) fn context_scroll_for_test(&self) -> (gpui::Pixels, gpui::Pixels) {
        (
            self.context_scroll.offset().y,
            self.context_scroll.max_offset().y,
        )
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

/// 二次确认框需要展示的文案。
pub(super) struct ConfirmPrompt {
    pub(super) title: String,
    pub(super) message: String,
    pub(super) confirm: String,
}

/// 渲染层需要的全部表单实体引用。
pub(super) struct PersonaFormInputs<'a> {
    pub(super) name: &'a Entity<InputState>,
    pub(super) system_prompt: &'a Entity<InputState>,
    pub(super) input_prompt: &'a Entity<InputState>,
    pub(super) provider: &'a Entity<SelectState<Vec<SharedString>>>,
    pub(super) live2d: &'a Entity<SelectState<Vec<SharedString>>>,
    pub(super) context_messages: &'a Entity<InputState>,
    pub(super) context_tokens: &'a Entity<InputState>,
}

impl EventEmitter<PersonaSettingsEvent> for PersonaSettingsView {}

/// 把人格的上下文限制翻译为只包含上限的占用结构，供未加载人格的统计回填。
fn context_limit_usage(persona: &PersonaConfig, providers: &SharedLlmSettings) -> ContextUsage {
    let limits = chat_limits(persona, providers);
    ContextUsage {
        messages: 0,
        max_messages: limits.max_messages,
        tokens: 0,
        max_tokens: limits.max_tokens,
    }
}

pub(super) fn memory_scope_name(scope: MemoryScope) -> String {
    match scope {
        MemoryScope::Context => t!("persona.memory_context").to_string(),
        MemoryScope::All => t!("persona.memory_all").to_string(),
    }
}

fn provider_option_names(providers: &SharedLlmSettings) -> Vec<SharedString> {
    let mut names = Vec::with_capacity(providers.models.len() + 1);
    names.push(SharedString::from(format!(
        "{BOUND_PROVIDER_INHERIT} {}",
        t!("persona.provider_inherit")
    )));
    for model in &providers.models {
        names.push(SharedString::from(format!(
            "{} · {}",
            model.label,
            provider_display_name(model.provider)
        )));
    }
    names
}

fn provider_option_index(providers: &SharedLlmSettings, bound: Option<&str>) -> usize {
    bound
        .and_then(|id| providers.models.iter().position(|model| model.id == id))
        .map_or(0, |index| index + 1)
}

fn live2d_option_state(
    models: &[Live2dModelOption],
    bound: Option<&Path>,
) -> (Vec<SharedString>, usize, Option<PathBuf>) {
    let mut names = Vec::with_capacity(models.len() + 2);
    names.push(SharedString::from(format!(
        "{BOUND_LIVE2D_INHERIT} {}",
        t!("persona.live2d_inherit")
    )));
    names.extend(
        models
            .iter()
            .map(|model| SharedString::from(model.label.clone())),
    );
    let Some(bound) = bound else {
        return (names, 0, None);
    };
    if let Some(index) = models.iter().position(|model| model.path == bound) {
        return (names, index + 1, None);
    }
    names.push(SharedString::from(
        t!(
            "persona.live2d_missing_option",
            path = bound.to_string_lossy().into_owned()
        )
        .to_string(),
    ));
    (names, models.len() + 1, Some(bound.to_path_buf()))
}

fn subscribe_form_input(
    input: &Entity<InputState>,
    window: &mut Window,
    cx: &mut Context<PersonaSettingsView>,
) -> Subscription {
    cx.subscribe_in(input, window, |this, _, event: &InputEvent, _, cx| {
        if matches!(event, InputEvent::Blur) && !this.loading_form {
            this.save(cx);
        }
    })
}

fn selection_is_additive(modifiers: Modifiers) -> bool {
    modifiers.shift || modifiers.secondary()
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

fn next_persona_id(settings: &PersonaSettings, reserved: &HashSet<String>) -> String {
    for index in 1_u64.. {
        let id = format!("persona-{index}");
        if !reserved.contains(&id)
            && !settings.pending_deletions.contains(&id)
            && !settings.personas.iter().any(|persona| persona.id == id)
        {
            return id;
        }
    }
    unreachable!("u64 人格 ID 空间不可能被现有配置耗尽")
}

/// 暴露新人格 ID 分配规则，供测试断言不会与既有条目冲突。
#[cfg(test)]
pub(crate) fn next_persona_id_for_test(settings: &PersonaSettings) -> String {
    next_persona_id(settings, &HashSet::new())
}

/// 暴露绑定供应商选择项与配置 ID 的双向映射，供测试断言往返一致。
#[cfg(test)]
pub(crate) fn provider_option_index_for_test(
    providers: &SharedLlmSettings,
    bound: Option<&str>,
) -> usize {
    provider_option_index(providers, bound)
}
