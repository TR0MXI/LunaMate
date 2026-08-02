//! 持久化语音模式与 Agent 工具权限。

use gpui::Context;
use rust_i18n::t;

use crate::config::{CONFIG, ConfigWriteError, VoiceMode};

use super::super::{SettingsEvent, SettingsView, next_save_revision};

impl SettingsView {
    pub(in crate::ui::settings) fn set_allow_agent_screenshot(
        &mut self,
        allowed: bool,
        cx: &mut Context<Self>,
    ) {
        if self.allow_agent_screenshot == allowed && !self.screenshot_permission_retry_required {
            return;
        }
        self.allow_agent_screenshot = allowed;
        self.screenshot_permission_retry_required = false;
        self.screenshot_permission_revision =
            self.screenshot_permission_revision.wrapping_add(1).max(1);
        let ui_revision = self.screenshot_permission_revision;
        let config_revision = CONFIG.reserve_allow_agent_screenshot_revision(allowed);
        let background = cx.background_executor().clone();
        cx.notify();

        let task = cx.spawn(async move |this, cx| {
            let result = background
                .spawn(async move {
                    CONFIG.set_allow_agent_screenshot_at_revision(allowed, config_revision)
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                if this.screenshot_permission_revision != ui_revision {
                    return;
                }
                this.allow_agent_screenshot = CONFIG.allow_agent_screenshot();
                this.screenshot_permission_retry_required =
                    CONFIG.agent_screenshot_permission_retry_required();
                if let Err(error) = result {
                    this.set_status(
                        t!("status.setting_failed", error = error.to_string()).to_string(),
                        cx,
                    );
                } else {
                    cx.notify();
                }
            });
        });
        self.track_write_task(task);
    }

    pub(in crate::ui::settings) fn set_allow_agent_outfit_change(
        &mut self,
        allowed: bool,
        cx: &mut Context<Self>,
    ) {
        if self.allow_agent_outfit_change == allowed {
            return;
        }
        self.allow_agent_outfit_change = allowed;
        let ui_revision =
            next_save_revision(&mut self.preference_save_revisions.allow_agent_outfit_change);
        cx.notify();

        let config_revision = CONFIG.reserve_allow_agent_outfit_change_revision();
        self.persist_setting(
            move || CONFIG.set_allow_agent_outfit_change_at_revision(allowed, config_revision),
            move |this, result, cx| {
                this.finish_allow_agent_outfit_change_write(ui_revision, allowed, result, cx);
            },
            cx,
        );
    }

    fn finish_allow_agent_outfit_change_write(
        &mut self,
        ui_revision: u64,
        requested: bool,
        result: Result<Option<()>, ConfigWriteError>,
        cx: &mut Context<Self>,
    ) {
        match result {
            Ok(Some(())) if CONFIG.allow_agent_outfit_change() == requested => {
                self.applied.allow_agent_outfit_change = requested;
                self.emit_settings_event(SettingsEvent::AgentOutfitToolChanged(requested), cx);
                cx.notify();
            }
            Ok(Some(())) | Ok(None) => {}
            Err(error)
                if self.preference_save_revisions.allow_agent_outfit_change == ui_revision =>
            {
                self.allow_agent_outfit_change = CONFIG.allow_agent_outfit_change();
                self.set_status(
                    t!("status.setting_failed", error = error.to_string()).to_string(),
                    cx,
                );
            }
            Err(_) => {}
        }
    }

    pub(in crate::ui::settings) fn set_voice_mode(
        &mut self,
        mode: VoiceMode,
        cx: &mut Context<Self>,
    ) {
        if self.voice.mode == mode {
            return;
        }
        self.voice.mode = mode;
        self.voice_save_revision = self.voice_save_revision.wrapping_add(1).max(1);
        let ui_revision = self.voice_save_revision;
        let settings = self.voice.clone();
        let config_revision = CONFIG.reserve_voice_settings_revision();
        let background = cx.background_executor().clone();
        cx.notify();
        let task = cx.spawn(async move |this, cx| {
            let result = background
                .spawn(
                    async move { CONFIG.set_voice_settings_at_revision(settings, config_revision) },
                )
                .await;
            let _ = this.update(cx, |this, cx| match result {
                Ok(Some(settings)) => {
                    let current = CONFIG.voice_settings();
                    if current.as_ref() != settings.as_ref() {
                        return;
                    }
                    this.applied.voice = current.as_ref().clone();
                    this.emit_settings_event(
                        SettingsEvent::VoiceChanged(current.as_ref().clone()),
                        cx,
                    );
                    if this.voice_save_revision == ui_revision {
                        this.voice = settings.as_ref().clone();
                        cx.notify();
                    }
                }
                Ok(None) => {}
                Err(error) if this.voice_save_revision == ui_revision => {
                    this.voice = CONFIG.voice_settings().as_ref().clone();
                    this.set_status(
                        t!("status.setting_failed", error = error.to_string()).to_string(),
                        cx,
                    );
                }
                Err(_) => {}
            });
        });
        self.track_write_task(task);
    }
}
