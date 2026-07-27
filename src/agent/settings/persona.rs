//! 保存人格设置草稿，处理人格增删与记忆清除，并发布配置变更。
//!
//! 记忆的删除是不可逆操作，因此所有清除入口都先进入 [`PendingConfirm`] 状态，
//! 只有用户在二次确认框中明确同意后才会派发实际删除任务。

use std::{sync::Arc, time::Duration};

use gpui::{AppContext, Context, Entity, EventEmitter, SharedString, Task, Window};
use gpui_component::{
    IndexPath,
    input::{InputState, MaskPattern},
    select::SelectState,
};
use gpui_tokio::Tokio;
use rust_i18n::t;

use crate::{
    agent::{
        AgentMemoryAccess, ContextUsage, MemoryScope, PersonaMemoryUsage, chat_limits_from_context,
    },
    config::{
        CONFIG, CONTEXT_KIB_MAX, CONTEXT_KIB_MIN, CONTEXT_MESSAGES_MAX, CONTEXT_MESSAGES_MIN,
        DEFAULT_CONTEXT_KIB, DEFAULT_CONTEXT_MESSAGES, PersonaConfig, PersonaContextLimits,
        PersonaSettings, SharedLlmSettings, SharedPersonaSettings,
    },
};

use super::{provider_display_name, set_input};

/// 人格绑定供应商的第一项固定表示"跟随全局默认供应商"。
const BOUND_PROVIDER_INHERIT: &str = "\u{2014}";

/// 设置窗口重建时保留的人格草稿，不向 UI 暴露配置类型。
#[derive(Clone)]
pub(crate) struct PersonaSettingsDraft(SharedPersonaSettings);

impl PersonaSettingsDraft {
    /// 从当前已发布配置创建设置窗口草稿。
    pub(crate) fn current() -> Self {
        Self(CONFIG.persona_settings())
    }
}

/// 人格设置向设置窗口发布的变更。
#[derive(Clone, Debug)]
pub(crate) enum PersonaSettingsEvent {
    /// 人格配置已发布，桌宠视图应重新读取人格、供应商与上下文。
    Saved,
    /// 写入被替换或失败；只用于释放关闭窗口后保留的编辑器实体。
    SaveFinished,
    /// 指定人格的短期上下文需要由持有会话的视图清除。
    ClearContext(String),
}

/// 等待二次确认的危险操作。
#[derive(Clone, Debug, Eq, PartialEq)]
enum PendingConfirm {
    /// 清除指定人格的某一类或全部记忆。
    ClearMemory { persona: String, scope: MemoryScope },
    /// 删除指定人格，并同步删除其绑定的全部记忆。
    DeletePersona { persona: String },
}

/// 设置窗口中的人格编辑器。
pub(crate) struct PersonaSettingsView {
    draft: PersonaSettings,
    providers: SharedLlmSettings,
    editing_index: Option<usize>,
    name_input: Entity<InputState>,
    system_prompt_input: Entity<InputState>,
    provider_select: Entity<SelectState<Vec<SharedString>>>,
    context_messages_input: Entity<InputState>,
    context_kib_input: Entity<InputState>,
    context_messages_enabled: bool,
    context_kib_enabled: bool,
    pub(super) advanced_expanded: bool,
    memory: AgentMemoryAccess,
    usage: Option<PersonaMemoryUsage>,
    usage_error: Option<String>,
    usage_revision: u64,
    usage_task: Option<Task<()>>,
    pending_confirm: Option<PendingConfirm>,
    status: Option<String>,
    is_saving: bool,
    toast_revision: u64,
    toast_task: Option<Task<()>>,
    write_tasks: Vec<Task<()>>,
}

impl PersonaSettingsView {
    /// 从当前运行时配置创建可丢弃的人格草稿。
    pub(crate) fn new(
        draft: PersonaSettingsDraft,
        memory: AgentMemoryAccess,
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
        let providers = CONFIG.llm_settings();
        let editing_index = draft
            .selected
            .as_deref()
            .and_then(|selected| draft.personas.iter().position(|item| item.id == selected))
            .or_else(|| (!draft.personas.is_empty()).then_some(0));
        let editing = editing_index.and_then(|index| draft.personas.get(index));
        let context = editing.map(|persona| persona.context).unwrap_or_default();

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
        let context_messages_input = cx.new(|cx| {
            integer_input(window, cx, CONTEXT_MESSAGES_MIN, CONTEXT_MESSAGES_MAX).default_value(
                context
                    .max_messages
                    .unwrap_or(DEFAULT_CONTEXT_MESSAGES)
                    .to_string(),
            )
        });
        let context_kib_input = cx.new(|cx| {
            integer_input(window, cx, CONTEXT_KIB_MIN, CONTEXT_KIB_MAX)
                .default_value(context.max_kib.unwrap_or(DEFAULT_CONTEXT_KIB).to_string())
        });

        let mut view = Self {
            draft,
            providers,
            editing_index,
            name_input,
            system_prompt_input,
            provider_select,
            context_messages_input,
            context_kib_input,
            context_messages_enabled: context.max_messages.is_some(),
            context_kib_enabled: context.max_kib.is_some(),
            advanced_expanded: false,
            memory,
            usage: None,
            usage_error: None,
            usage_revision: 0,
            usage_task: None,
            pending_confirm: None,
            status: None,
            is_saving: false,
            toast_revision: 0,
            toast_task: None,
            write_tasks: Vec::new(),
        };
        view.refresh_usage(cx);
        view
    }

    /// 保存窗口草稿并转移尚未结束的写任务，供关闭后重新创建编辑器。
    pub(crate) fn take_window_state(
        &mut self,
        cx: &mut Context<Self>,
    ) -> (PersonaSettingsDraft, Vec<Task<()>>) {
        self.capture_current_form(cx);
        (
            PersonaSettingsDraft(Arc::new(self.draft.clone())),
            std::mem::take(&mut self.write_tasks),
        )
    }

    /// 供应商目录变化后刷新绑定选择器的候选项。
    pub(crate) fn refresh_providers(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.providers = CONFIG.llm_settings();
        let bound = self
            .editing_index
            .and_then(|index| self.draft.personas.get(index))
            .and_then(|persona| persona.model.clone());
        let names = provider_option_names(&self.providers);
        let index = provider_option_index(&self.providers, bound.as_deref());
        self.provider_select.update(cx, |select, cx| {
            select.set_items(names, window, cx);
            select.set_selected_index(Some(IndexPath::new(index)), window, cx);
        });
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
        let context = self.capture_context_limits(cx);
        let Some(persona) = self.draft.personas.get_mut(index) else {
            return;
        };
        persona.name = self.name_input.read(cx).value().to_string();
        persona.system_prompt = self.system_prompt_input.read(cx).value().to_string();
        persona.model = bound;
        persona.context = context;
    }

    fn capture_context_limits(&self, cx: &Context<Self>) -> PersonaContextLimits {
        PersonaContextLimits {
            max_messages: self
                .context_messages_enabled
                .then(|| parse_u32(self.context_messages_input.read(cx).value().as_ref()))
                .flatten(),
            max_kib: self
                .context_kib_enabled
                .then(|| parse_u32(self.context_kib_input.read(cx).value().as_ref()))
                .flatten(),
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

    fn load_form(&mut self, index: Option<usize>, window: &mut Window, cx: &mut Context<Self>) {
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
        set_input(
            &self.context_messages_input,
            &context
                .max_messages
                .unwrap_or(DEFAULT_CONTEXT_MESSAGES)
                .to_string(),
            window,
            cx,
        );
        set_input(
            &self.context_kib_input,
            &context.max_kib.unwrap_or(DEFAULT_CONTEXT_KIB).to_string(),
            window,
            cx,
        );
        self.context_messages_enabled = context.max_messages.is_some();
        self.context_kib_enabled = context.max_kib.is_some();
        self.refresh_usage(cx);
        cx.notify();
    }

    pub(super) fn select_persona(
        &mut self,
        index: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.is_saving {
            return;
        }
        self.capture_current_form(cx);
        let Some(persona) = self.draft.personas.get(index) else {
            return;
        };
        self.draft.selected = Some(persona.id.clone());
        self.load_form(Some(index), window, cx);
    }

    pub(super) fn add_persona(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.is_saving {
            return;
        }
        self.capture_current_form(cx);
        let id = next_persona_id(&self.draft);
        self.draft.personas.push(PersonaConfig::new(
            id.clone(),
            t!("persona.new_persona").to_string(),
        ));
        self.draft.selected = Some(id);
        self.load_form(self.draft.personas.len().checked_sub(1), window, cx);
    }

    /// 请求删除当前人格；实际删除在二次确认后执行。
    pub(super) fn request_delete_persona(&mut self, cx: &mut Context<Self>) {
        if self.is_saving {
            return;
        }
        let Some(persona) = self
            .editing_index
            .and_then(|index| self.draft.personas.get(index))
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
        if self.is_saving {
            return;
        }
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
                self.clear_memory(persona, scope, cx);
            }
            PendingConfirm::DeletePersona { persona } => {
                log::info!("用户已确认删除人格及其记忆");
                self.delete_persona(persona, window, cx);
            }
        }
    }

    fn clear_memory(&mut self, persona: String, scope: MemoryScope, cx: &mut Context<Self>) {
        if matches!(scope, MemoryScope::Context | MemoryScope::All) {
            // 会话文档只有持有会话的视图会写入，清除也必须由它执行才能避免竞争。
            cx.emit(PersonaSettingsEvent::ClearContext(persona.clone()));
        }
        let memory = self.memory.persona(&persona);
        let task = Tokio::spawn(cx, async move { memory.clear(scope).await });
        self.set_status(t!("persona.memory_clearing").to_string(), cx);
        let track = cx.spawn(async move |this, cx| {
            let result = task.await;
            match &result {
                Ok(Ok(())) if matches!(scope, MemoryScope::Medium | MemoryScope::Long) => {
                    log::info!("人格记忆清除完成：scope={}", scope.id());
                }
                Ok(Ok(())) if scope == MemoryScope::Context => {
                    log::info!("人格短期上下文清除请求已提交");
                }
                Ok(Ok(())) => {
                    log::info!("人格中长期记忆已清除，短期上下文清除请求已提交");
                }
                Ok(Err(_)) | Err(_) => {
                    log::error!("人格记忆清除失败：scope={}", scope.id());
                }
            }
            let _ = this.update(cx, |this, cx| {
                let status = match result {
                    Ok(Ok(())) => t!("persona.memory_cleared").to_string(),
                    Ok(Err(error)) => {
                        t!("persona.memory_clear_failed", error = error.to_string()).to_string()
                    }
                    Err(error) => {
                        t!("persona.memory_clear_failed", error = error.to_string()).to_string()
                    }
                };
                this.set_status(status, cx);
                this.refresh_usage(cx);
            });
        });
        self.track_write_task(track);
        cx.notify();
    }

    fn delete_persona(&mut self, persona: String, window: &mut Window, cx: &mut Context<Self>) {
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
        self.draft.personas.remove(index);
        let next_index = index.min(self.draft.personas.len() - 1);
        self.draft.selected = self
            .draft
            .personas
            .get(next_index)
            .map(|persona| persona.id.clone());
        self.load_form(Some(next_index), window, cx);
        // 人格与其记忆必须一起消失；先落盘配置，再删除该人格绑定的全部记忆。
        self.save(cx);
        self.clear_memory(persona, MemoryScope::All, cx);
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

        let limits = context_limit_usage(persona.context);
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

    pub(super) fn reload_usage(&mut self, cx: &mut Context<Self>) {
        self.refresh_usage(cx);
        cx.notify();
    }

    pub(super) fn toggle_advanced(&mut self, cx: &mut Context<Self>) {
        self.advanced_expanded = !self.advanced_expanded;
        cx.notify();
    }

    pub(super) fn toggle_context_messages(&mut self, cx: &mut Context<Self>) {
        self.context_messages_enabled = !self.context_messages_enabled;
        cx.notify();
    }

    pub(super) fn toggle_context_size(&mut self, cx: &mut Context<Self>) {
        self.context_kib_enabled = !self.context_kib_enabled;
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
        let revision = CONFIG.reserve_persona_settings_revision();
        self.set_status(t!("persona.saving").to_string(), cx);
        let background = cx.background_executor().clone();

        let task = cx.spawn(async move |this, cx| {
            let result = background
                .spawn(async move { CONFIG.set_persona_settings_at_revision(normalized, revision) })
                .await;
            let _ = this.update(cx, |this, cx| {
                this.is_saving = false;
                let (status, event) = match result {
                    Ok(Some(_)) => (t!("persona.saved").to_string(), PersonaSettingsEvent::Saved),
                    Ok(None) => (
                        t!("persona.save_replaced").to_string(),
                        PersonaSettingsEvent::SaveFinished,
                    ),
                    Err(error) => (
                        t!("persona.save_failed", error = error.to_string()).to_string(),
                        PersonaSettingsEvent::SaveFinished,
                    ),
                };
                cx.emit(event);
                this.set_status(status, cx);
            });
        });
        self.track_write_task(task);
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

    pub(super) fn is_saving(&self) -> bool {
        self.is_saving
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
                message: t!("persona.confirm_delete_message", persona = name(persona)).to_string(),
                confirm: t!("persona.confirm_delete").to_string(),
            },
        })
    }

    pub(super) fn form(&self) -> PersonaFormInputs<'_> {
        PersonaFormInputs {
            name: &self.name_input,
            system_prompt: &self.system_prompt_input,
            provider: &self.provider_select,
            context_messages: &self.context_messages_input,
            context_kib: &self.context_kib_input,
        }
    }

    pub(super) const fn context_toggles(&self) -> [bool; 2] {
        [self.context_messages_enabled, self.context_kib_enabled]
    }

    /// 返回草稿中的人格 ID 列表，供测试断言增删与选择行为。
    #[cfg(test)]
    pub(in crate::agent) fn persona_ids_for_test(&self) -> Vec<String> {
        self.draft
            .personas
            .iter()
            .map(|persona| persona.id.clone())
            .collect()
    }

    /// 返回当前正在编辑的人格索引。
    #[cfg(test)]
    pub(in crate::agent) fn editing_index_for_test(&self) -> Option<usize> {
        self.editing_index
    }

    /// 返回草稿中当前选中的人格 ID。
    #[cfg(test)]
    pub(in crate::agent) fn selected_persona_for_test(&self) -> Option<&str> {
        self.draft.selected.as_deref()
    }

    /// 返回当前等待二次确认的操作描述，供测试断言危险操作不会立即执行。
    #[cfg(test)]
    pub(in crate::agent) fn pending_confirm_for_test(
        &self,
    ) -> Option<(String, Option<MemoryScope>)> {
        self.pending_confirm.as_ref().map(|pending| match pending {
            PendingConfirm::ClearMemory { persona, scope } => (persona.clone(), Some(*scope)),
            PendingConfirm::DeletePersona { persona } => (persona.clone(), None),
        })
    }

    /// 追加一个新人格条目。
    #[cfg(test)]
    pub(in crate::agent) fn add_persona_for_test(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.add_persona(window, cx);
    }

    /// 请求删除当前人格，但不进行确认。
    #[cfg(test)]
    pub(in crate::agent) fn request_delete_persona_for_test(&mut self, cx: &mut Context<Self>) {
        self.request_delete_persona(cx);
    }

    /// 请求清除指定范围的记忆，但不进行确认。
    #[cfg(test)]
    pub(in crate::agent) fn request_clear_memory_for_test(
        &mut self,
        scope: MemoryScope,
        cx: &mut Context<Self>,
    ) {
        self.request_clear_memory(scope, cx);
    }

    /// 取消等待中的二次确认。
    #[cfg(test)]
    pub(in crate::agent) fn cancel_confirm_for_test(&mut self, cx: &mut Context<Self>) {
        self.cancel_confirm(cx);
    }

    /// 切换到指定索引的人格条目。
    #[cfg(test)]
    pub(in crate::agent) fn select_persona_for_test(
        &mut self,
        index: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.select_persona(index, window, cx);
    }

    /// 返回指定草稿条目当前绑定的供应商 ID。
    #[cfg(test)]
    pub(in crate::agent) fn bound_provider_for_test(&self, index: usize) -> Option<String> {
        self.draft
            .personas
            .get(index)
            .and_then(|persona| persona.model.clone())
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
    pub(super) provider: &'a Entity<SelectState<Vec<SharedString>>>,
    pub(super) context_messages: &'a Entity<InputState>,
    pub(super) context_kib: &'a Entity<InputState>,
}

impl EventEmitter<PersonaSettingsEvent> for PersonaSettingsView {}

/// 把人格的上下文限制翻译为只包含上限的占用结构，供未加载人格的统计回填。
fn context_limit_usage(context: PersonaContextLimits) -> ContextUsage {
    let limits = chat_limits_from_context(context);
    ContextUsage {
        messages: 0,
        max_messages: limits.max_messages,
        bytes: 0,
        max_bytes: limits.max_bytes,
    }
}

pub(super) fn memory_scope_name(scope: MemoryScope) -> String {
    match scope {
        MemoryScope::Context => t!("persona.memory_context").to_string(),
        MemoryScope::Medium => t!("persona.memory_medium").to_string(),
        MemoryScope::Long => t!("persona.memory_long").to_string(),
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
        .step(1.0)
        .min(f64::from(min))
        .max(f64::from(max))
}

fn parse_u32(value: &str) -> Option<u32> {
    let value = value.trim();
    (!value.is_empty()).then(|| value.parse().ok()).flatten()
}

fn next_persona_id(settings: &PersonaSettings) -> String {
    for index in 1_u64.. {
        let id = format!("persona-{index}");
        if !settings.personas.iter().any(|persona| persona.id == id) {
            return id;
        }
    }
    unreachable!("u64 人格 ID 空间不可能被配置上限耗尽")
}

/// 暴露新人格 ID 分配规则，供测试断言不会与既有条目冲突。
#[cfg(test)]
pub(in crate::agent) fn next_persona_id_for_test(settings: &PersonaSettings) -> String {
    next_persona_id(settings)
}

/// 暴露绑定供应商选择项与配置 ID 的双向映射，供测试断言往返一致。
#[cfg(test)]
pub(in crate::agent) fn provider_option_index_for_test(
    providers: &SharedLlmSettings,
    bound: Option<&str>,
) -> usize {
    provider_option_index(providers, bound)
}
