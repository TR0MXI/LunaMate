use std::collections::HashSet;

use crate::{
    config::{KeyboardShortcut, ShortcutAction},
    platform::APPLICATION_ID,
    shortcut::wayland::{ActiveBindings, portal_trigger},
};

#[test]
fn portal_trigger_keeps_four_modifiers_and_main_key() {
    let shortcut =
        KeyboardShortcut::from_id("Ctrl+Alt+Shift+Super+KeyK").expect("测试组合键应当有效");

    assert_eq!(
        portal_trigger(shortcut).expect("组合键应当可转换为 XDG trigger"),
        "CTRL+ALT+SHIFT+LOGO+k"
    );
}

#[test]
fn portal_trigger_uses_xkb_names_for_special_keys() {
    let cases = [
        ("Ctrl+Enter", "CTRL+Return"),
        ("Alt+PageDown", "ALT+Page_Down"),
        ("Shift+NumpadAdd", "SHIFT+KP_Add"),
        ("Ctrl+Quote", "CTRL+apostrophe"),
        ("F24", "F24"),
    ];

    for (source, expected) in cases {
        let shortcut = KeyboardShortcut::from_id(source).expect("测试快捷键应当有效");
        assert_eq!(
            portal_trigger(shortcut).expect("快捷键应当可转换为 XDG trigger"),
            expected
        );
    }
}

#[test]
fn active_bindings_suppress_duplicate_presses_and_releases() {
    let mut bindings = ActiveBindings::new(HashSet::from([ShortcutAction::VoiceInput]));

    assert!(bindings.press(ShortcutAction::VoiceInput));
    assert!(!bindings.press(ShortcutAction::VoiceInput));
    assert!(bindings.release(ShortcutAction::VoiceInput));
    assert!(!bindings.release(ShortcutAction::VoiceInput));
    assert!(!bindings.press(ShortcutAction::ToggleSettings));
}

#[test]
fn removing_a_bound_action_releases_a_held_shortcut() {
    let mut bindings = ActiveBindings::new(HashSet::from([
        ShortcutAction::VoiceInput,
        ShortcutAction::ToggleSettings,
    ]));
    assert!(bindings.press(ShortcutAction::VoiceInput));

    let released = bindings.replace_bound(HashSet::from([ShortcutAction::ToggleSettings]));

    assert_eq!(released, [ShortcutAction::VoiceInput]);
    assert!(bindings.take_pressed().is_empty());
}

#[test]
fn desktop_entry_matches_the_portal_application_id() {
    let desktop_entry = include_str!("../../../assets/linux/io.github.tr0mxi.lunamate.desktop");

    assert_eq!(APPLICATION_ID, "io.github.tr0mxi.lunamate");
    assert!(desktop_entry.contains("StartupWMClass=io.github.tr0mxi.lunamate"));
}
