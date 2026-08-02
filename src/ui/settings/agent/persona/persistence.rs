//! 保存人格草稿，发布配置写入，并协调删除 tombstone 的清理生命周期。

use std::{collections::HashSet, sync::Arc, time::Duration};

use gpui::{Context, Task};
#[cfg(test)]
use lunamate_agent::config::PersonaSettings;
use lunamate_agent::config::SharedPersonaSettings;
use rust_i18n::t;

use crate::config::CONFIG;

use super::{PersonaPage, PersonaSettingsDraftWrite, PersonaSettingsView, memory::MemoryScope};

/// 会话持有者完成单条上下文修改后返回设置页的结果通道。
pub(crate) type ContextMutationCompletion = async_channel::Sender<Result<(), String>>;

/// 设置窗口重建时保留的人格草稿，不向 UI 暴露配置类型。
#[derive(Clone)]
pub(in crate::ui) struct PersonaSettingsDraft {
    pub(super) settings: SharedPersonaSettings,
    pub(super) reserved_cleanup: HashSet<String>,
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

impl PersonaSettingsView {
    /// 保存窗口草稿并转移尚未结束的写任务，供关闭后重新创建编辑器。
    pub(in crate::ui) fn take_window_state(
        &mut self,
        cx: &mut Context<Self>,
    ) -> (PersonaSettingsDraft, Vec<Task<()>>, bool) {
        self.stop_context_auto_refresh();
        self.cancel_context_selection();
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

    pub(super) fn set_status(&mut self, message: impl Into<String>, cx: &mut Context<Self>) {
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

    pub(super) fn emit_save_finished_if_idle(&self, cx: &mut Context<Self>) {
        if self.config_writes_in_flight == 0 && self.persona_cleanup_in_flight.is_empty() {
            cx.emit(PersonaSettingsEvent::SaveFinished);
        }
    }

    pub(super) fn track_write_task(&mut self, task: Task<()>) {
        // 只保留仍在执行的任务，避免长期打开设置窗口时无界累积句柄。
        self.write_tasks.retain(|task| !task.is_ready());
        self.write_tasks.push(task);
    }
}
