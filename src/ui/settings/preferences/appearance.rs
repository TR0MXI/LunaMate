//! 持久化主题与语言，并在写盘成功后发布到运行时。

use gpui::{Context, Window};
use rust_i18n::t;

use crate::{
    config::{AppLanguage, AppearanceSettings, CONFIG, ConfigWriteError, ThemePreset},
    ui::{apply, apply_language},
};

use super::super::{SettingsEvent, SettingsView, next_save_revision};

impl SettingsView {
    fn capture_custom_theme(&mut self, cx: &mut Context<Self>) {
        if let Some(input) = &self.custom_accent_input {
            self.appearance.custom.accent = input.read(cx).value().to_string();
        }
        if let Some(input) = &self.custom_background_input {
            self.appearance.custom.background = input.read(cx).value().to_string();
        }
    }

    pub(in crate::ui::settings) fn set_appearance(
        &mut self,
        appearance: AppearanceSettings,
        show_feedback: bool,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let appearance = match appearance.normalized() {
            Ok(appearance) => appearance,
            Err(error) => {
                self.set_status(error, cx);
                return;
            }
        };
        self.appearance = appearance.clone();
        let ui_revision = next_save_revision(&mut self.preference_save_revisions.appearance);
        cx.notify();
        let config_revision = CONFIG.reserve_appearance_revision();
        let background = cx.background_executor().clone();
        let requested = appearance.clone();
        let task = cx.spawn(async move |this, cx| {
            let result = background
                .spawn(
                    async move { CONFIG.set_appearance_at_revision(appearance, config_revision) },
                )
                .await;
            let _ = this.update(cx, |this, cx| {
                this.finish_appearance_write(ui_revision, requested, show_feedback, result, cx);
            });
        });
        self.track_write_task(task);
    }

    pub(in crate::ui::settings) fn finish_appearance_write(
        &mut self,
        ui_revision: u64,
        requested: AppearanceSettings,
        show_feedback: bool,
        result: Result<Option<std::sync::Arc<AppearanceSettings>>, ConfigWriteError>,
        cx: &mut Context<Self>,
    ) {
        match result {
            Ok(Some(published)) => {
                let current = CONFIG.appearance();
                if current.as_ref() != published.as_ref() {
                    return;
                }
                let published = published.as_ref().clone();
                self.applied.appearance = published.clone();
                if self.preference_save_revisions.appearance == ui_revision
                    && self.appearance == requested
                {
                    self.appearance = published.clone();
                }
                apply_language(published.language);
                apply(&published, None, cx);
                self.emit_settings_event(SettingsEvent::AppearanceChanged(published), cx);
                if self.preference_save_revisions.appearance == ui_revision && show_feedback {
                    self.set_status(t!("status.appearance_saved").to_string(), cx);
                } else {
                    cx.notify();
                }
            }
            Ok(None) => {}
            Err(error) if self.preference_save_revisions.appearance == ui_revision => {
                self.appearance = CONFIG.appearance().as_ref().clone();
                self.set_status(
                    t!("status.appearance_failed", error = error.to_string()).to_string(),
                    cx,
                );
            }
            Err(_) => {}
        }
    }

    pub(in crate::ui::settings) fn set_theme(
        &mut self,
        theme: ThemePreset,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if theme == ThemePreset::Custom {
            self.capture_custom_theme(cx);
        }
        let mut appearance = self.appearance.clone();
        appearance.theme = theme;
        self.set_appearance(appearance, false, window, cx);
    }

    pub(in crate::ui::settings) fn apply_custom_theme(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.capture_custom_theme(cx);
        let mut appearance = self.appearance.clone();
        appearance.theme = ThemePreset::Custom;
        self.set_appearance(appearance, true, window, cx);
    }

    pub(in crate::ui::settings) fn set_language(
        &mut self,
        language: AppLanguage,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let mut appearance = self.appearance.clone();
        appearance.language = language;
        self.set_appearance(appearance, false, window, cx);
    }
}
