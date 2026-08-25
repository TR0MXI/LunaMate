use toml_edit::DocumentMut;

use crate::config::{
    KeyboardShortcut, ShortcutAction, ShortcutSettings, parse_shortcut_settings,
    write_shortcut_settings,
};

#[test]
fn shortcut_ids_are_normalized_and_escape_is_reserved() {
    let shortcut = KeyboardShortcut::from_id("ctrl+shift+k").expect("普通组合键应当可以解析");

    assert_eq!(shortcut.id(), "shift+control+KeyK");
    assert!(KeyboardShortcut::from_id("Escape").is_err());
    assert!(KeyboardShortcut::from_id("Shift+Control").is_err());
    assert!(KeyboardShortcut::from_id("NumLock").is_err());
    assert!(KeyboardShortcut::from_id("NumpadEnter").is_err());
    assert!(KeyboardShortcut::from_id("NumpadEqual").is_err());
}

#[test]
fn assigning_a_duplicate_moves_it_to_the_new_action() {
    let shortcut = KeyboardShortcut::from_id("F8").expect("功能键应当可以解析");
    let mut settings = ShortcutSettings::default();
    assert_eq!(
        settings.assign(ShortcutAction::VoiceInput, Some(shortcut)),
        None
    );

    assert_eq!(
        settings.assign(ShortcutAction::ToggleSettings, Some(shortcut)),
        Some(ShortcutAction::VoiceInput)
    );
    assert_eq!(settings.shortcut(ShortcutAction::VoiceInput), None);
    assert_eq!(
        settings.shortcut(ShortcutAction::ToggleSettings),
        Some(shortcut)
    );
}

#[test]
fn shortcut_settings_round_trip_without_losing_unrelated_keys() {
    let mut document = "[custom]\nkeep = true\n"
        .parse::<DocumentMut>()
        .expect("测试配置应当可以解析");
    let mut settings = ShortcutSettings::default();
    settings.assign(
        ShortcutAction::VoiceInput,
        Some(KeyboardShortcut::from_id("Alt+Space").expect("测试快捷键应当有效")),
    );
    settings.assign(
        ShortcutAction::ToggleChatInput,
        Some(KeyboardShortcut::from_id("F9").expect("测试快捷键应当有效")),
    );

    write_shortcut_settings(&mut document, &settings);
    let mut warnings = Vec::new();
    let restored = parse_shortcut_settings(&document, &mut warnings);

    assert!(warnings.is_empty());
    assert_eq!(restored, settings);
    assert_eq!(document["custom"]["keep"].as_bool(), Some(true));
    assert!(document["shortcuts"].get("toggle_settings").is_none());
}

#[test]
fn malformed_and_duplicate_shortcuts_are_ignored_independently() {
    let document = r#"[shortcuts]
voice_input = "Ctrl+KeyV"
toggle_desktop_pet = "Ctrl+KeyV"
toggle_settings = 42
toggle_chat_input = "Escape"
"#
    .parse::<DocumentMut>()
    .expect("测试配置应当可以解析");
    let mut warnings = Vec::new();

    let settings = parse_shortcut_settings(&document, &mut warnings);

    assert!(settings.shortcut(ShortcutAction::VoiceInput).is_some());
    assert_eq!(settings.shortcut(ShortcutAction::ToggleDesktopPet), None);
    assert_eq!(settings.shortcut(ShortcutAction::ToggleSettings), None);
    assert_eq!(settings.shortcut(ShortcutAction::ToggleChatInput), None);
    assert_eq!(warnings.len(), 3);
}
