//! 同步全局快捷键配置，并把运行时事件路由到桌宠交互。

use gpui::{Context, Window};

use super::DesktopPetView;
use crate::shortcut::ShortcutEvent;

impl DesktopPetView {
    pub(super) fn apply_shortcut_settings(
        &mut self,
        settings: &crate::config::ShortcutSettings,
        cx: &mut Context<Self>,
    ) {
        self.release_voice_shortcut();
        let (errors, asynchronous) = if let Some(manager) = &mut self.shortcut_manager {
            let errors = manager.configure(settings.clone());
            (errors, manager.reports_status_asynchronously())
        } else {
            (self.shortcut_runtime_errors.clone(), false)
        };
        if !asynchronous || !errors.is_empty() {
            self.report_shortcut_runtime_errors(errors, cx);
        }
    }

    pub(super) fn set_shortcut_recording(&mut self, recording: bool, cx: &mut Context<Self>) {
        if recording {
            self.release_voice_shortcut();
        }
        if let Some(manager) = &mut self.shortcut_manager {
            let errors = manager.set_suspended(recording);
            if !manager.reports_status_asynchronously() && (!errors.is_empty() || !recording) {
                self.report_shortcut_runtime_errors(errors, cx);
            }
        } else {
            self.report_shortcut_runtime_errors(self.shortcut_runtime_errors.clone(), cx);
        }
    }

    fn report_shortcut_runtime_errors(&mut self, errors: Vec<String>, cx: &mut Context<Self>) {
        if !errors.is_empty() {
            log::warn!("event=shortcut_runtime_errors count={}", errors.len());
        }
        self.shortcut_runtime_errors = errors.clone();
        self.config.update(cx, |config, cx| {
            config.report_shortcut_runtime_errors(errors, cx);
        });
    }

    pub(super) fn handle_shortcut_event(
        &mut self,
        event: ShortcutEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let runtime_bindings = self
            .shortcut_manager
            .as_ref()
            .and_then(|manager| manager.runtime_bindings(&event))
            .map(|bindings| bindings.to_vec());
        if let Some(bindings) = runtime_bindings {
            self.config.update(cx, |config, cx| {
                config.report_shortcut_runtime_bindings(bindings, cx);
            });
            return true;
        }
        let runtime_errors = self
            .shortcut_manager
            .as_ref()
            .and_then(|manager| manager.runtime_errors(&event))
            .map(|errors| errors.to_vec());
        if let Some(errors) = runtime_errors {
            self.report_shortcut_runtime_errors(errors, cx);
            return true;
        }
        let Some(event) = self
            .shortcut_manager
            .as_ref()
            .and_then(|manager| manager.resolve(&event))
        else {
            return true;
        };
        let action = event.action();
        let should_activate_main_window = event.is_pressed()
            && match action {
                crate::config::ShortcutAction::ToggleDesktopPet => !self.desktop_pet_visible,
                crate::config::ShortcutAction::ToggleChatInput => {
                    !self.desktop_pet_visible || !self.chat_input_open
                }
                crate::config::ShortcutAction::VoiceInput
                | crate::config::ShortcutAction::ToggleSettings => false,
            };
        if should_activate_main_window
            && let Some(token) = event.activation_token()
            && let Some(manager) = &self.shortcut_manager
            && manager.activate_wayland(token.to_owned()).is_err()
        {
            log::warn!("event=shortcut_window_activation_failed platform=wayland");
        }
        if action == crate::config::ShortcutAction::VoiceInput {
            self.set_voice_shortcut_pressed(event.is_pressed(), cx);
            return true;
        }
        if !event.is_pressed() {
            return true;
        }
        match action {
            crate::config::ShortcutAction::VoiceInput => {}
            crate::config::ShortcutAction::ToggleDesktopPet => {
                if self.toggle_desktop_pet_visibility(window, cx).is_err() {
                    log::warn!("event=desktop_pet_visibility_change_failed source=shortcut");
                }
            }
            crate::config::ShortcutAction::ToggleSettings => self.toggle_config_window(cx),
            crate::config::ShortcutAction::ToggleChatInput => {
                self.toggle_chat_input_from_shortcut(window, cx);
            }
        }
        true
    }
}
