use gpui_component::ThemeMode;

use crate::config::{AppLanguage, AppearanceSettings, CustomThemeSettings, ThemePreset};

#[test]
fn custom_colors_are_normalized_before_persistence() {
    let settings = AppearanceSettings {
        custom: CustomThemeSettings {
            accent: " 2563eb ".to_owned(),
            background: "#f8fafc".to_owned(),
            mode: ThemeMode::Light,
        },
        ..AppearanceSettings::default()
    }
    .normalized()
    .expect("有效十六进制颜色应当可以规范化");

    assert_eq!(settings.custom.accent, "#2563EB");
    assert_eq!(settings.custom.background, "#F8FAFC");
}

#[test]
fn eight_digit_colors_keep_their_alpha_channel() {
    let settings = AppearanceSettings {
        custom: CustomThemeSettings {
            accent: "#2dd4bfcc".to_owned(),
            background: "0f172a80".to_owned(),
            mode: ThemeMode::Dark,
        },
        ..AppearanceSettings::default()
    }
    .normalized()
    .expect("含 Alpha 的十六进制颜色应当可以规范化");

    assert_eq!(settings.custom.accent, "#2DD4BFCC");
    assert_eq!(settings.custom.background, "#0F172A80");
}

#[test]
fn malformed_colors_are_rejected_with_the_offending_field_label() {
    for accent in ["", "#12345", "#1234567", "#gg2233", "rgb(1,2,3)"] {
        let error = AppearanceSettings {
            custom: CustomThemeSettings {
                accent: accent.to_owned(),
                ..CustomThemeSettings::default()
            },
            ..AppearanceSettings::default()
        }
        .normalized()
        .expect_err("非法强调色应当被拒绝");
        assert!(!error.is_empty(), "颜色 {accent:?} 应当有可展示的错误说明");
    }

    assert!(
        AppearanceSettings {
            custom: CustomThemeSettings {
                background: "#xyzxyz".to_owned(),
                ..CustomThemeSettings::default()
            },
            ..AppearanceSettings::default()
        }
        .normalized()
        .is_err()
    );
}

#[test]
fn default_appearance_is_already_normalized() {
    let default = AppearanceSettings::default();

    assert_eq!(default.language, AppLanguage::SimplifiedChinese);
    assert_eq!(default.theme, ThemePreset::System);
    assert_eq!(
        default.clone().normalized().expect("默认外观必须有效"),
        default
    );
}

#[test]
fn language_identifiers_round_trip_and_accept_regional_aliases() {
    for language in [
        AppLanguage::SimplifiedChinese,
        AppLanguage::TraditionalChinese,
        AppLanguage::English,
        AppLanguage::Japanese,
    ] {
        assert_eq!(AppLanguage::from_id(language.id()), Some(language));
    }

    assert_eq!(
        AppLanguage::from_id("zh"),
        Some(AppLanguage::SimplifiedChinese)
    );
    assert_eq!(
        AppLanguage::from_id("zh-HK"),
        Some(AppLanguage::TraditionalChinese)
    );
    for alias in ["en-US", "en-GB"] {
        assert_eq!(AppLanguage::from_id(alias), Some(AppLanguage::English));
    }
    assert_eq!(AppLanguage::from_id("ja-JP"), Some(AppLanguage::Japanese));
    assert_eq!(AppLanguage::from_id("ko"), None);
}

#[test]
fn theme_identifiers_round_trip_and_reject_unknown_presets() {
    for theme in [
        ThemePreset::System,
        ThemePreset::Light,
        ThemePreset::Dark,
        ThemePreset::Graphite,
        ThemePreset::Sakura,
        ThemePreset::Ocean,
        ThemePreset::HighContrast,
        ThemePreset::Custom,
    ] {
        assert_eq!(ThemePreset::from_id(theme.id()), Some(theme));
    }

    assert_eq!(ThemePreset::from_id("solarized"), None);
    assert_eq!(ThemePreset::from_id("High-Contrast"), None);
}
