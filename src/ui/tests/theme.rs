use crate::{
    config::{CustomThemeSettings, ThemePreset},
    ui::theme::theme_config,
};

#[test]
fn custom_theme_chooses_readable_primary_foreground() {
    let light_accent = CustomThemeSettings {
        accent: "#FACC15".to_owned(),
        ..CustomThemeSettings::default()
    };
    let dark_accent = CustomThemeSettings {
        accent: "#0F172A".to_owned(),
        ..CustomThemeSettings::default()
    };

    let light = theme_config(ThemePreset::Custom, &light_accent);
    let dark = theme_config(ThemePreset::Custom, &dark_accent);
    assert_eq!(light.colors.primary_foreground.as_deref(), Some("#111827"));
    assert_ne!(light.colors.accent.as_deref(), Some("#FACC15"));
    assert_eq!(dark.colors.primary_foreground.as_deref(), Some("#F8FAFC"));
}

#[test]
fn core_settings_text_exists_in_every_supported_language() {
    assert_eq!(
        rust_i18n::t!("settings.system_title", locale = "zh-CN"),
        "系统设置"
    );
    assert_eq!(
        rust_i18n::t!("settings.system_title", locale = "zh-TW"),
        "系統設定"
    );
    assert_eq!(
        rust_i18n::t!("settings.system_title", locale = "en"),
        "System Settings"
    );
    assert_eq!(
        rust_i18n::t!("settings.system_title", locale = "ja"),
        "システム設定"
    );

    for locale in ["zh-CN", "zh-TW", "en", "ja"] {
        assert_ne!(
            rust_i18n::t!("settings.debug_title", locale = locale),
            "settings.debug_title"
        );
        assert_ne!(
            rust_i18n::t!("system.eye_tracking", locale = locale),
            "system.eye_tracking"
        );
        assert_ne!(
            rust_i18n::t!("system.follow_display", locale = locale),
            "system.follow_display"
        );
        assert_ne!(
            rust_i18n::t!("system.custom_frame_rate", locale = locale),
            "system.custom_frame_rate"
        );
        assert_ne!(
            rust_i18n::t!("llm.add_model", locale = locale),
            "llm.add_model"
        );
    }
}
