use gpui_component::{ThemeMode, try_parse_color};

use crate::{
    config::{CustomThemeSettings, ThemePreset},
    ui::theme::theme_config,
};

const ALL_PRESETS: [ThemePreset; 8] = [
    ThemePreset::System,
    ThemePreset::Light,
    ThemePreset::Dark,
    ThemePreset::Graphite,
    ThemePreset::Sakura,
    ThemePreset::Ocean,
    ThemePreset::HighContrast,
    ThemePreset::Custom,
];

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
fn every_preset_fills_all_semantic_color_slots_with_parsable_values() {
    let custom = CustomThemeSettings::default();

    for preset in ALL_PRESETS {
        let config = theme_config(preset, &custom);
        let colors = &config.colors;
        let slots: [(&str, Option<&str>); 24] = [
            ("background", colors.background.as_deref()),
            ("foreground", colors.foreground.as_deref()),
            ("muted_foreground", colors.muted_foreground.as_deref()),
            ("border", colors.border.as_deref()),
            ("input", colors.input.as_deref()),
            ("muted", colors.muted.as_deref()),
            ("primary", colors.primary.as_deref()),
            ("primary_foreground", colors.primary_foreground.as_deref()),
            ("secondary", colors.secondary.as_deref()),
            (
                "secondary_foreground",
                colors.secondary_foreground.as_deref(),
            ),
            ("accent", colors.accent.as_deref()),
            ("accent_foreground", colors.accent_foreground.as_deref()),
            ("sidebar", colors.sidebar.as_deref()),
            ("popover", colors.popover.as_deref()),
            ("danger", colors.danger.as_deref()),
            ("warning", colors.warning.as_deref()),
            ("success", colors.success.as_deref()),
            ("info", colors.info.as_deref()),
            ("button", colors.button.as_deref()),
            ("list_hover", colors.list_hover.as_deref()),
            ("title_bar", colors.title_bar.as_deref()),
            ("status_bar", colors.status_bar.as_deref()),
            ("ring", colors.ring.as_deref()),
            ("selection", colors.selection.as_deref()),
        ];

        for (slot, value) in slots {
            let value = value.unwrap_or_else(|| panic!("{} 主题缺少 {slot} 语义色", preset.id()));
            assert!(
                try_parse_color(value).is_ok(),
                "{} 主题的 {slot} 颜色 {value} 应当可以解析",
                preset.id()
            );
        }
    }
}

#[test]
fn theme_name_and_mode_follow_the_selected_preset() {
    let custom = CustomThemeSettings::default();

    for preset in ALL_PRESETS {
        assert_eq!(theme_config(preset, &custom).name.as_ref(), preset.id());
    }

    for preset in [ThemePreset::System, ThemePreset::Light, ThemePreset::Sakura] {
        assert_eq!(theme_config(preset, &custom).mode, ThemeMode::Light);
    }
    for preset in [
        ThemePreset::Dark,
        ThemePreset::Graphite,
        ThemePreset::Ocean,
        ThemePreset::HighContrast,
    ] {
        assert_eq!(theme_config(preset, &custom).mode, ThemeMode::Dark);
    }
}

#[test]
fn custom_theme_mode_selects_the_readable_static_base() {
    let dark = theme_config(
        ThemePreset::Custom,
        &CustomThemeSettings {
            mode: ThemeMode::Dark,
            ..CustomThemeSettings::default()
        },
    );
    let light = theme_config(
        ThemePreset::Custom,
        &CustomThemeSettings {
            mode: ThemeMode::Light,
            ..CustomThemeSettings::default()
        },
    );

    assert_eq!(dark.mode, ThemeMode::Dark);
    assert_eq!(light.mode, ThemeMode::Light);
    // 静态槽位来自明暗基准方案，因此同一自定义配色在两种模式下前景色不同。
    assert_ne!(dark.colors.foreground, light.colors.foreground);
}

#[test]
fn custom_theme_applies_user_colors_to_primary_background_and_selection() {
    let custom = CustomThemeSettings {
        accent: "#2DD4BF".to_owned(),
        background: "#0F172A".to_owned(),
        mode: ThemeMode::Dark,
    };

    let config = theme_config(ThemePreset::Custom, &custom);

    assert_eq!(config.colors.background.as_deref(), Some("#0F172A"));
    assert_eq!(config.colors.primary.as_deref(), Some("#2DD4BF"));
    assert_eq!(config.colors.ring.as_deref(), Some("#2DD4BF"));
    assert_eq!(config.colors.selection.as_deref(), Some("#2DD4BF"));
    // 强调面为背景与强调色的混色，应当既不等于背景也不等于强调色本身。
    let accent = config
        .colors
        .accent
        .as_deref()
        .expect("自定义强调面应当存在");
    assert_ne!(accent, "#0F172A");
    assert_ne!(accent, "#2DD4BF");
    assert_eq!(config.colors.list_hover.as_deref(), Some(accent));
    assert_eq!(config.colors.list_active.as_deref(), Some(accent));
    assert_eq!(config.colors.sidebar_accent.as_deref(), Some(accent));
}

#[test]
fn unparsable_custom_colors_keep_the_base_scheme_instead_of_panicking() {
    // 已持久化的旧配置可能带有不合法颜色；主题构造必须降级而不是崩溃。
    let config = theme_config(
        ThemePreset::Custom,
        &CustomThemeSettings {
            accent: "not-a-color".to_owned(),
            background: "#0F172A".to_owned(),
            mode: ThemeMode::Dark,
        },
    );

    assert_eq!(config.colors.primary.as_deref(), Some("not-a-color"));
    // 混色分支被跳过，强调面保留暗色基准值。
    assert_eq!(config.colors.accent.as_deref(), Some("#134E4A"));
    assert_eq!(config.colors.sidebar_accent, None);
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
            rust_i18n::t!("debug.use_native_tray_menu", locale = locale),
            "debug.use_native_tray_menu"
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
        assert_ne!(
            rust_i18n::t!("settings.tool_title", locale = locale),
            "settings.tool_title"
        );
        assert_ne!(
            rust_i18n::t!("tools.allow_agent_screenshot", locale = locale),
            "tools.allow_agent_screenshot"
        );
        assert_ne!(
            rust_i18n::t!("tools.allow_agent_outfit_change", locale = locale),
            "tools.allow_agent_outfit_change"
        );
        assert_ne!(
            rust_i18n::t!("tools.outfit_change_notice", locale = locale),
            "tools.outfit_change_notice"
        );
        assert_ne!(
            rust_i18n::t!("voice.model_downloads", locale = locale),
            "voice.model_downloads"
        );
        assert_ne!(
            rust_i18n::t!("settings.shortcut", locale = locale),
            "settings.shortcut"
        );
        assert_ne!(
            rust_i18n::t!("shortcut.toggle_chat_input", locale = locale),
            "shortcut.toggle_chat_input"
        );
        assert_ne!(
            rust_i18n::t!("voice.model_download_notice", locale = locale),
            "voice.model_download_notice"
        );
        assert_ne!(
            rust_i18n::t!("voice.whisper_model_list", locale = locale),
            "voice.whisper_model_list"
        );
        assert_ne!(
            rust_i18n::t!("voice.vad_model_list", locale = locale),
            "voice.vad_model_list"
        );
    }
}
