//! 保存供应商草稿，发布配置写入并维护窗口重建时的任务生命周期。

use std::{sync::Arc, time::Duration};

use gpui::{Context, Task};
#[cfg(test)]
use lunamate_agent::config::LlmSettings;
use lunamate_agent::config::SharedLlmSettings;
use rust_i18n::t;

use crate::config::CONFIG;

use super::ProviderSettingsView;

/// 设置窗口重建时保留的供应商草稿，不向 UI 暴露 Provider 配置类型。
#[derive(Clone)]
pub(in crate::ui) struct ProviderSettingsDraft {
    pub(super) settings: SharedLlmSettings,
}

impl ProviderSettingsDraft {
    /// 从当前已发布配置创建设置窗口草稿。
    pub(in crate::ui) fn current() -> Self {
        Self {
            settings: CONFIG.llm_settings(),
        }
    }

    #[cfg(test)]
    pub(in crate::ui) fn from_settings_for_test(settings: LlmSettings) -> Self {
        Self {
            settings: Arc::new(settings),
        }
    }
}

/// 供应商设置写入完成后通知设置窗口；只有 `Saved` 需要刷新桌宠运行时。
#[derive(Clone, Copy, Debug)]
pub(in crate::ui) enum ProviderSettingsEvent {
    Saved,
    SaveFinished,
}

impl ProviderSettingsView {
    /// 保存窗口草稿并转移尚未结束的写任务，供关闭后重新创建编辑器。
    pub(in crate::ui) fn take_window_state(
        &mut self,
        cx: &mut Context<Self>,
    ) -> (ProviderSettingsDraft, Vec<Task<()>>) {
        self.save(cx);
        (
            ProviderSettingsDraft {
                settings: Arc::new(self.draft.clone()),
            },
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

    pub(super) fn save(&mut self, cx: &mut Context<Self>) {
        if self.loading_form {
            return;
        }
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
            return;
        }
        self.draft = normalized.clone();
        self.submitted_draft = normalized.clone();
        self.save_revision = self.save_revision.wrapping_add(1).max(1);
        let ui_revision = self.save_revision;
        self.config_writes_in_flight = self.config_writes_in_flight.saturating_add(1);
        let config_revision = CONFIG.reserve_llm_settings_revision();
        self.set_status(t!("llm.saving").to_string(), cx);
        let background = cx.background_executor().clone();

        let task = cx.spawn(async move |this, cx| {
            let result = background
                .spawn(async move {
                    CONFIG.set_llm_settings_at_revision(normalized, config_revision, language)
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                let latest = this.save_revision == ui_revision;
                this.config_writes_in_flight = this.config_writes_in_flight.saturating_sub(1);
                match result {
                    Ok(Some(_)) => {
                        cx.emit(ProviderSettingsEvent::Saved);
                        if latest {
                            this.set_status(t!("llm.saved").to_string(), cx);
                        }
                    }
                    Ok(None) => {
                        if latest {
                            this.submitted_draft = CONFIG.llm_settings().as_ref().clone();
                            this.set_status(t!("llm.save_replaced").to_string(), cx);
                        }
                    }
                    Err(error) => {
                        if latest {
                            this.submitted_draft = CONFIG.llm_settings().as_ref().clone();
                            this.set_status(
                                t!("llm.save_failed", error = error.to_string()).to_string(),
                                cx,
                            );
                        }
                    }
                }
                if this.config_writes_in_flight == 0 {
                    cx.emit(ProviderSettingsEvent::SaveFinished);
                }
            });
        });
        // 只保留仍在执行的写任务，避免长期打开设置窗口时无界累积句柄。
        self.write_tasks.retain(|task| !task.is_ready());
        self.write_tasks.push(task);
    }
}
