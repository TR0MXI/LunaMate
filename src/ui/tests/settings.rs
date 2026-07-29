use gpui::{Keystroke, Modifiers};

use crate::{
    config::{FrameRate, KeyboardShortcut},
    shortcut::{shortcut_from_keystroke, shortcut_keycaps},
    ui::settings::{custom_frame_rate_seed, parse_custom_frame_rate},
};

#[test]
fn custom_frame_rate_input_accepts_only_positive_u16_digits() {
    assert!(matches!(
        parse_custom_frame_rate("60"),
        Some(FrameRate::Custom(fps)) if fps.get() == 60
    ));
    assert!(matches!(
        parse_custom_frame_rate("65535"),
        Some(FrameRate::Custom(fps)) if fps.get() == u16::MAX
    ));
    for invalid in ["", "0", "-1", "+60", "60.0", "65536", "６０"] {
        assert_eq!(parse_custom_frame_rate(invalid), None);
    }
}

#[test]
fn custom_frame_rate_seed_uses_fixed_rate_or_sixty() {
    assert_eq!(custom_frame_rate_seed(FrameRate::Fps30), 30);
    assert_eq!(custom_frame_rate_seed(FrameRate::Fps120), 120);
    assert_eq!(custom_frame_rate_seed(FrameRate::FollowDisplay), 60);
    assert_eq!(custom_frame_rate_seed(FrameRate::Unlimited), 60);
    assert_eq!(
        custom_frame_rate_seed(FrameRate::custom(75).expect("测试帧率必须有效")),
        75
    );
}

#[test]
fn shortcut_recording_accepts_single_keys_and_modifier_combinations() {
    let single = shortcut_from_keystroke(&Keystroke {
        key: "f8".to_owned(),
        ..Keystroke::default()
    })
    .expect("单键应当可以录入")
    .expect("功能键不是单独的修饰键");
    assert_eq!(
        single,
        KeyboardShortcut::from_id("F8").expect("F8 应当有效")
    );

    let combination = shortcut_from_keystroke(&Keystroke {
        modifiers: Modifiers {
            control: true,
            shift: true,
            ..Modifiers::default()
        },
        key: "k".to_owned(),
        ..Keystroke::default()
    })
    .expect("组合键应当可以录入")
    .expect("K 不是单独的修饰键");
    assert_eq!(shortcut_keycaps(combination), ["Ctrl", "Shift", "K"]);
}

#[test]
fn shortcut_recording_waits_for_a_main_key_and_reserves_escape() {
    let modifier = shortcut_from_keystroke(&Keystroke {
        modifiers: Modifiers {
            control: true,
            ..Modifiers::default()
        },
        key: "control".to_owned(),
        ..Keystroke::default()
    })
    .expect("单独修饰键不应报错");
    assert_eq!(modifier, None);

    assert!(
        shortcut_from_keystroke(&Keystroke {
            key: "escape".to_owned(),
            ..Keystroke::default()
        })
        .is_err()
    );
}

#[test]
fn shortcut_recording_accepts_numeric_and_shifted_symbol_keys() {
    let digit = shortcut_from_keystroke(&Keystroke {
        key: "1".to_owned(),
        ..Keystroke::default()
    })
    .expect("数字键应当可以录入")
    .expect("数字键不是修饰键");
    let shifted = shortcut_from_keystroke(&Keystroke {
        modifiers: Modifiers {
            control: true,
            ..Modifiers::default()
        },
        key: "@".to_owned(),
        ..Keystroke::default()
    })
    .expect("移位符号应当可以录入")
    .expect("移位符号不是修饰键");

    assert_eq!(shortcut_keycaps(digit), ["1"]);
    assert_eq!(shortcut_keycaps(shifted), ["Ctrl", "Shift", "2"]);
}

#[test]
fn shortcut_recording_keeps_every_supported_modifier() {
    let shortcut = shortcut_from_keystroke(&Keystroke {
        modifiers: Modifiers {
            control: true,
            alt: true,
            shift: true,
            platform: true,
            ..Modifiers::default()
        },
        key: "k".to_owned(),
        ..Keystroke::default()
    })
    .expect("多修饰键组合应当可以录入")
    .expect("K 不是修饰键");

    let alt = if cfg!(target_os = "macos") {
        "Option"
    } else {
        "Alt"
    };
    let platform = if cfg!(target_os = "macos") {
        "Cmd"
    } else if cfg!(target_os = "windows") {
        "Win"
    } else {
        "Super"
    };
    assert_eq!(
        shortcut_keycaps(shortcut),
        ["Ctrl", alt, "Shift", platform, "K"]
    );
}
