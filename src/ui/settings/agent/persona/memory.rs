//! 处理记忆统计、危险操作确认和人格删除后的完整记忆清理。

use gpui::{Context, Window};
use gpui_tokio::Tokio;
use lunamate_agent::memory::PersistentMemoryScope;
use rust_i18n::t;

use super::{PersonaPage, PersonaSettingsEvent, PersonaSettingsView, context::context_limit_usage};

/// 等待二次确认的危险操作。
#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) enum PendingConfirm {
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

/// 二次确认框需要展示的文案。
pub(super) struct ConfirmPrompt {
    pub(super) title: String,
    pub(super) message: String,
    pub(super) confirm: String,
}

impl PersonaSettingsView {
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
                log::info!("event=memory_clear_confirmed scope={}", scope.id());
                self.clear_memory(persona, scope, false, cx);
            }
            PendingConfirm::DeletePersona { persona } => {
                log::info!("event=persona_delete_confirmed memory=true");
                self.delete_persona(persona, window, cx);
            }
            PendingConfirm::DeleteContextMessages {
                persona,
                message_ids,
            } => {
                log::info!(
                    "event=context_message_delete_confirmed count={}",
                    message_ids.len()
                );
                self.delete_context_messages(persona, message_ids, window, cx);
            }
        }
    }

    pub(super) fn clear_memory(
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
                log::info!("event=memory_clear_completed scope={}", scope.id());
            } else {
                log::error!("event=memory_clear_failed scope={}", scope.id());
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
                    this.cancel_context_selection();
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

    pub(super) fn refresh_usage(&mut self, cx: &mut Context<Self>) {
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
}

fn memory_scope_name(scope: MemoryScope) -> String {
    match scope {
        MemoryScope::Context => t!("persona.memory_context").to_string(),
        MemoryScope::All => t!("persona.memory_all").to_string(),
    }
}
