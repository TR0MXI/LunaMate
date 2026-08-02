//! 持久化日志设置，并在写盘成功后即时应用过滤等级。

use std::time::Duration;

use gpui::{Context, Entity};
use gpui_component::input::InputState;
use rust_i18n::t;

use crate::{
    config::{CONFIG, LoggingSettings},
    logging::ApplyLoggingSettingsOutcome,
};

use super::super::{SettingsView, next_save_revision};

const LOGGING_SAVE_DELAY: Duration = Duration::from_millis(250);

impl SettingsView {
    pub(in crate::ui::settings) fn set_logging_settings(
        &mut self,
        settings: LoggingSettings,
        cx: &mut Context<Self>,
    ) {
        if self.logging == settings {
            return;
        }
        self.logging = settings;
        let ui_revision = next_save_revision(&mut self.preference_save_revisions.logging);
        cx.notify();

        let config_revision = CONFIG.reserve_logging_settings_revision();
        let background = cx.background_executor().clone();
        let task = cx.spawn(async move |this, cx| {
            let result = background
                .spawn(async move {
                    let persisted = CONFIG
                        .set_logging_settings_at_revision(settings, config_revision)
                        .map_err(|error| ("persist", error.to_string()))?;
                    if persisted.is_some() {
                        let outcome = crate::logging::apply_current_settings()
                            .map_err(|error| ("apply_runtime", error))?;
                        return Ok(Some(outcome));
                    }
                    Ok::<Option<ApplyLoggingSettingsOutcome>, (&'static str, String)>(None)
                })
                .await;
            if let Err(("apply_runtime", _)) = &result {
                log::error!("event=logging_runtime_apply_failed phase=apply_runtime");
            }
            let _ = this.update(cx, |this, cx| {
                let current = *CONFIG.logging_settings();
                this.finish_logging_write(ui_revision, settings, current, result, cx);
            });
        });
        self.track_write_task(task);
    }

    pub(in crate::ui::settings) fn finish_logging_write(
        &mut self,
        ui_revision: u64,
        requested: LoggingSettings,
        current: LoggingSettings,
        result: Result<Option<ApplyLoggingSettingsOutcome>, (&'static str, String)>,
        cx: &mut Context<Self>,
    ) {
        let request_was_published = current == requested
            && (matches!(&result, Ok(Some(_))) || matches!(&result, Err(("apply_runtime", _))));
        let previous_persisted = self.persisted_logging;
        if request_was_published {
            self.persisted_logging = requested;
        }
        if self.preference_save_revisions.logging != ui_revision {
            return;
        }

        match result {
            Ok(Some(_)) if request_was_published => {
                self.logging = requested;
                if crate::logging::file_policy_differs(previous_persisted, requested) {
                    self.set_status(
                        t!("status.logging_file_policy_saved_restart").to_string(),
                        cx,
                    );
                } else {
                    cx.notify();
                }
            }
            Ok(Some(_)) | Ok(None) => {
                if self.logging == requested {
                    self.logging = current;
                }
                cx.notify();
            }
            Err(("persist", error)) => {
                self.logging = current;
                self.set_status(
                    t!("status.logging_save_failed", error = error).to_string(),
                    cx,
                );
            }
            Err(("apply_runtime", error)) if request_was_published => {
                self.logging = requested;
                if crate::logging::file_policy_differs(previous_persisted, requested) {
                    self.set_status(
                        t!("status.logging_file_policy_saved_restart").to_string(),
                        cx,
                    );
                } else {
                    self.set_status(
                        t!("status.logging_level_apply_failed", error = error).to_string(),
                        cx,
                    );
                }
            }
            Err((_, error)) => {
                self.logging = current;
                self.set_status(
                    t!("status.logging_level_apply_failed", error = error).to_string(),
                    cx,
                );
            }
        }
    }

    /// 输入过程中延迟提交，避免每次按键都触发一次完整配置写盘。
    pub(in crate::ui::settings) fn schedule_logging_save(
        &mut self,
        input: &Entity<InputState>,
        apply: fn(&mut Self, &Entity<InputState>, &mut Context<Self>),
        cx: &mut Context<Self>,
    ) {
        self.logging_input_revision = self.logging_input_revision.wrapping_add(1);
        let revision = self.logging_input_revision;
        let input = input.clone();
        let background = cx.background_executor().clone();
        self.logging_save_task = Some(cx.spawn(async move |this, cx| {
            background.timer(LOGGING_SAVE_DELAY).await;
            let _ = this.update(cx, |this, cx| {
                if this.logging_input_revision == revision {
                    apply(this, &input, cx);
                }
            });
        }));
    }

    pub(in crate::ui::settings) fn commit_logging_input(
        &mut self,
        input: &Entity<InputState>,
        apply: fn(&mut Self, &Entity<InputState>, &mut Context<Self>),
        cx: &mut Context<Self>,
    ) {
        self.logging_input_revision = self.logging_input_revision.wrapping_add(1);
        self.logging_save_task = None;
        apply(self, input, cx);
    }

    /// 在设置窗口关闭或应用退出前提交尚未到期的日志输入。
    pub(in crate::ui::settings) fn flush_logging_inputs(&mut self, cx: &mut Context<Self>) {
        self.logging_input_revision = self.logging_input_revision.wrapping_add(1);
        self.logging_save_task = None;
        if let Some(input) = self.log_max_size_input.clone() {
            self.set_log_max_size_from_input(&input, cx);
        }
        if let Some(input) = self.log_keep_files_input.clone() {
            self.set_log_keep_files_from_input(&input, cx);
        }
    }

    pub(in crate::ui::settings) fn set_log_max_size_from_input(
        &mut self,
        input: &Entity<InputState>,
        cx: &mut Context<Self>,
    ) {
        let Ok(max_size_mb) = input.read(cx).value().parse::<u32>() else {
            return;
        };
        let settings = LoggingSettings {
            max_size_mb,
            ..self.logging
        };
        if settings.normalized().is_ok() {
            self.set_logging_settings(settings, cx);
        }
    }

    pub(in crate::ui::settings) fn set_log_keep_files_from_input(
        &mut self,
        input: &Entity<InputState>,
        cx: &mut Context<Self>,
    ) {
        let Ok(keep_files) = input.read(cx).value().parse::<u32>() else {
            return;
        };
        let settings = LoggingSettings {
            keep_files,
            ..self.logging
        };
        if settings.normalized().is_ok() {
            self.set_logging_settings(settings, cx);
        }
    }
}
