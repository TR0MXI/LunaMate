//! 管理全局快捷键录入、持久化以及运行时注册反馈。

use gpui::{Context, KeyDownEvent, KeybindingKeystroke, Window};
use rust_i18n::t;

use crate::{
    config::{CONFIG, KeyboardShortcut, ShortcutAction},
    shortcut::{ShortcutRuntimeBinding, shortcut_from_keybinding},
};

use super::{SettingsEvent, SettingsView};

impl SettingsView {
    pub(super) fn begin_shortcut_recording(
        &mut self,
        action: ShortcutAction,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let starting = self.shortcut_recording.is_none();
        self.shortcut_recording = Some(action);
        if let Some(focus) = &self.shortcut_focus {
            focus.focus(window, cx);
        }
        if starting {
            self.emit_settings_event(SettingsEvent::ShortcutRecordingChanged(true), cx);
        }
        cx.notify();
    }

    pub(super) fn stop_shortcut_recording(&mut self, cx: &mut Context<Self>) {
        if self.shortcut_recording.take().is_some() {
            self.emit_settings_event(SettingsEvent::ShortcutRecordingChanged(false), cx);
            cx.notify();
        }
    }

    pub(super) fn handle_shortcut_key_down(
        &mut self,
        event: &KeyDownEvent,
        cx: &mut Context<Self>,
    ) {
        let Some(action) = self.shortcut_recording else {
            return;
        };
        if event.is_held {
            return;
        }
        if event.keystroke.key.eq_ignore_ascii_case("escape") {
            self.commit_shortcut(action, None, cx);
            return;
        }
        let keystroke = KeybindingKeystroke::new_with_mapper(
            event.keystroke.clone(),
            false,
            cx.keyboard_mapper().as_ref(),
        );
        match shortcut_from_keybinding(&keystroke) {
            Ok(Some(shortcut)) => self.commit_shortcut(action, Some(shortcut), cx),
            Ok(None) => {}
            Err(error) => self.set_status(error, cx),
        }
    }

    fn commit_shortcut(
        &mut self,
        action: ShortcutAction,
        shortcut: Option<KeyboardShortcut>,
        cx: &mut Context<Self>,
    ) {
        self.stop_shortcut_recording(cx);
        let mut settings = self.shortcuts.clone();
        settings.assign(action, shortcut);
        if settings == self.shortcuts {
            return;
        }
        self.shortcuts = settings.clone();
        self.shortcut_save_revision = self.shortcut_save_revision.wrapping_add(1).max(1);
        let ui_revision = self.shortcut_save_revision;
        let config_revision = CONFIG.reserve_shortcut_settings_revision();
        let background = cx.background_executor().clone();
        let task = cx.spawn(async move |this, cx| {
            let result = background
                .spawn(async move {
                    CONFIG.set_shortcut_settings_at_revision(settings, config_revision)
                })
                .await;
            let _ = this.update(cx, |this, cx| match result {
                Ok(Some(settings)) => {
                    let current = CONFIG.shortcut_settings();
                    if current.as_ref() != settings.as_ref() {
                        return;
                    }
                    this.applied.shortcuts = current.as_ref().clone();
                    this.emit_settings_event(
                        SettingsEvent::ShortcutsChanged(current.as_ref().clone()),
                        cx,
                    );
                    if this.shortcut_save_revision == ui_revision {
                        this.shortcuts = settings.as_ref().clone();
                        this.set_status(t!("shortcut.saved").to_string(), cx);
                    }
                }
                Ok(None) => {}
                Err(error) if this.shortcut_save_revision == ui_revision => {
                    this.shortcuts = CONFIG.shortcut_settings().as_ref().clone();
                    this.set_status(
                        t!("shortcut.save_failed", error = error.to_string()).to_string(),
                        cx,
                    );
                }
                Err(_) => {}
            });
        });
        self.track_write_task(task);
        cx.notify();
    }

    /// 把系统注册失败反馈到仍可复用的设置实体。
    pub(crate) fn report_shortcut_runtime_errors(
        &mut self,
        errors: Vec<String>,
        cx: &mut Context<Self>,
    ) {
        self.shortcut_runtime_errors = errors;
        let message = (!self.shortcut_runtime_errors.is_empty()).then(|| {
            t!(
                "shortcut.registration_failed",
                error = self
                    .shortcut_runtime_errors
                    .join(t!("common.status_separator").as_ref())
            )
            .to_string()
        });
        if let Some(message) = message {
            self.set_status(message, cx);
        } else {
            cx.notify();
        }
    }

    /// 显示 Wayland 合成器实际确认的触发方式，而不是 preferred trigger。
    pub(crate) fn report_shortcut_runtime_bindings(
        &mut self,
        bindings: Vec<ShortcutRuntimeBinding>,
        cx: &mut Context<Self>,
    ) {
        self.shortcut_runtime_bindings.clear();
        self.shortcut_runtime_bindings.extend(
            bindings
                .into_iter()
                .filter(|binding| !binding.trigger_description().is_empty())
                .map(|binding| (binding.action(), binding.trigger_description().to_owned())),
        );
        cx.notify();
    }
}
