//! 构造 portal 绑定请求，并协调合成器实际授权的动作子集。

use std::collections::HashSet;

use ashpd::desktop::global_shortcuts::{NewShortcut, Shortcut};
use rust_i18n::t;

use crate::config::{ShortcutAction, ShortcutSettings};

use super::{ShortcutRuntimeBinding, trigger::portal_trigger};

pub(super) struct RequestedShortcuts {
    pub(super) descriptors: Vec<NewShortcut>,
    pub(super) actions: HashSet<ShortcutAction>,
}

pub(in crate::shortcut) struct ActiveBindings {
    bound: HashSet<ShortcutAction>,
    pressed: HashSet<ShortcutAction>,
}

impl ActiveBindings {
    pub(in crate::shortcut) fn new(bound: HashSet<ShortcutAction>) -> Self {
        Self {
            bound,
            pressed: HashSet::new(),
        }
    }

    pub(super) fn bound(&self) -> &HashSet<ShortcutAction> {
        &self.bound
    }

    pub(in crate::shortcut) fn press(&mut self, action: ShortcutAction) -> bool {
        self.bound.contains(&action) && self.pressed.insert(action)
    }

    pub(in crate::shortcut) fn release(&mut self, action: ShortcutAction) -> bool {
        self.pressed.remove(&action)
    }

    pub(in crate::shortcut) fn replace_bound(
        &mut self,
        next: HashSet<ShortcutAction>,
    ) -> Vec<ShortcutAction> {
        let released = self
            .pressed
            .iter()
            .copied()
            .filter(|action| !next.contains(action))
            .collect::<Vec<_>>();
        self.pressed.retain(|action| next.contains(action));
        self.bound = next;
        released
    }

    pub(in crate::shortcut) fn take_pressed(&mut self) -> Vec<ShortcutAction> {
        self.pressed.drain().collect()
    }
}

pub(super) fn requested_shortcuts(
    settings: &ShortcutSettings,
) -> Result<RequestedShortcuts, String> {
    let mut descriptors = Vec::with_capacity(settings.configured_count());
    let mut actions = HashSet::with_capacity(settings.configured_count());
    for action in ShortcutAction::ALL {
        let Some(shortcut) = settings.shortcut(action) else {
            continue;
        };
        let trigger = portal_trigger(shortcut)?;
        descriptors.push(
            NewShortcut::new(action.id(), action_description(action))
                .preferred_trigger(Some(trigger.as_str())),
        );
        actions.insert(action);
    }
    Ok(RequestedShortcuts {
        descriptors,
        actions,
    })
}

fn action_description(action: ShortcutAction) -> String {
    match action {
        ShortcutAction::VoiceInput => t!("shortcut.voice_input").to_string(),
        ShortcutAction::ToggleDesktopPet => t!("shortcut.toggle_desktop_pet").to_string(),
        ShortcutAction::ToggleSettings => t!("shortcut.toggle_settings").to_string(),
        ShortcutAction::ToggleChatInput => t!("shortcut.toggle_chat_input").to_string(),
    }
}

pub(super) fn bound_actions(shortcuts: &[Shortcut]) -> HashSet<ShortcutAction> {
    shortcuts
        .iter()
        .filter_map(|shortcut| ShortcutAction::from_id(shortcut.id()))
        .collect()
}

pub(super) fn runtime_bindings(shortcuts: &[Shortcut]) -> Vec<ShortcutRuntimeBinding> {
    shortcuts
        .iter()
        .filter_map(|shortcut| {
            let action = ShortcutAction::from_id(shortcut.id())?;
            Some(ShortcutRuntimeBinding::new(
                action,
                shortcut.trigger_description().to_owned(),
            ))
        })
        .collect()
}

pub(super) fn missing_binding_errors(
    requested: &HashSet<ShortcutAction>,
    bound: &HashSet<ShortcutAction>,
) -> Vec<String> {
    requested
        .difference(bound)
        .map(|action| format!("{} 未获 Wayland 合成器授权", action.id()))
        .collect()
}
