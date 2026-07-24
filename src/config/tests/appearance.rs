use gpui_component::ThemeMode;

use crate::config::{AppearanceSettings, CustomThemeSettings};

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
