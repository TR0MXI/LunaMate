//! 验证标量配置、领域快照与窗口状态持久化。

use std::{
    fs,
    path::{Path, PathBuf},
};

use gpui_component::ThemeMode;

use super::TestDirectory;
use crate::config::*;

#[test]
fn shortcut_settings_publish_and_round_trip() {
    let directory = TestDirectory::new();
    let config = LunaConfig::load_from(directory.config_path());
    let mut settings = ShortcutSettings::default();
    settings.assign(
        ShortcutAction::ToggleSettings,
        Some(KeyboardShortcut::from_id("Control+Shift+KeyS").expect("测试快捷键应当有效")),
    );

    let revision = config.reserve_shortcut_settings_revision();
    let published = config
        .set_shortcut_settings_at_revision(settings.clone(), revision)
        .expect("快捷键配置应当可以持久化")
        .expect("最新快捷键配置应当发布");

    assert_eq!(published.as_ref(), &settings);
    assert_eq!(config.shortcut_settings().as_ref(), &settings);
    assert_eq!(
        LunaConfig::load_from(directory.config_path())
            .shortcut_settings()
            .as_ref(),
        &settings
    );
}

#[test]
fn model_resource_overrides_publish_and_round_trip() {
    let directory = TestDirectory::new();
    let config = LunaConfig::load_from(directory.config_path());
    let key = ModelResourceKey::new(
        PathBuf::from("luna/runtime/luna.model3.json"),
        ModelResourceKind::Motion,
        "external:motions/wave.motion3.json",
    );
    let settings = ModelResourceSettings::default()
        .with_name(key.clone(), Some("挥手"))
        .expect("测试动作名称应当有效");

    let revision = config.reserve_model_resource_settings_revision();
    let published = config
        .set_model_resource_settings_at_revision(settings.clone(), revision)
        .expect("模型资源配置应当可以持久化")
        .expect("最新模型资源配置应当发布");

    assert_eq!(published.as_ref(), &settings);
    assert_eq!(config.model_resource_settings().name(&key), Some("挥手"));
    assert_eq!(
        LunaConfig::load_from(directory.config_path())
            .model_resource_settings()
            .name(&key),
        Some("挥手")
    );
}

#[test]
fn valid_values_load_into_atoms_and_snapshot() {
    let directory = TestDirectory::new();
    directory.write(
        r#"[render]
frame_rate = 60

[model]
selected = "luna/runtime/luna.model3.json"

[debug]
show_fps = true
use_native_tray_menu = true

[tools]
allow_agent_screenshot = true
allow_agent_outfit_change = false

[interaction]
eye_tracking = false

[window]
remember_position = false

[window.desktop_pet]
x = -120.5
y = 48
"#,
    );

    let config = LunaConfig::load_from(directory.config_path());
    assert_eq!(config.frame_rate(), FrameRate::Fps60);
    assert!(!config.remember_window_positions());
    assert!(!config.eye_tracking());
    assert!(config.show_fps());
    assert!(config.use_native_tray_menu());
    assert!(config.allow_agent_screenshot());
    assert!(!config.allow_agent_outfit_change());
    assert_eq!(
        config.selected_model(),
        Some(PathBuf::from("luna/runtime/luna.model3.json"))
    );
    assert_eq!(
        config.window_position(ConfigWindow::DesktopPet),
        WindowPosition::new(-120.5, 48.0)
    );
}

#[test]
fn interaction_and_debug_switches_round_trip() {
    let directory = TestDirectory::new();
    let config = LunaConfig::load_from(directory.config_path());

    let eye_revision = config.reserve_eye_tracking_revision();
    config
        .set_eye_tracking_at_revision(false, eye_revision)
        .expect("眼部跟随开关应当可以持久化")
        .expect("最新眼部跟随请求应当生效");
    let fps_revision = config.reserve_show_fps_revision();
    config
        .set_show_fps_at_revision(true, fps_revision)
        .expect("帧率显示开关应当可以持久化")
        .expect("最新帧率显示请求应当生效");
    let tray_revision = config.reserve_use_native_tray_menu_revision();
    config
        .set_use_native_tray_menu_at_revision(true, tray_revision)
        .expect("原生托盘菜单开关应当可以持久化")
        .expect("最新原生托盘菜单请求应当生效");

    let reloaded = LunaConfig::load_from(directory.config_path());
    assert!(!reloaded.eye_tracking());
    assert!(reloaded.show_fps());
    assert!(reloaded.use_native_tray_menu());
    let saved = fs::read_to_string(directory.config_path()).expect("调试配置应当可以读取");
    assert!(saved.contains("eye_tracking = false"));
    assert!(saved.contains("show_fps = true"));
    assert!(saved.contains("use_native_tray_menu = true"));
}

#[test]
fn logging_settings_load_and_round_trip() {
    let directory = TestDirectory::new();
    directory.write(
        r#"[logging]
level = "debug"
rotation = false
compression = false
max_size_mb = 25
keep_files = 20
"#,
    );
    let config = LunaConfig::load_from(directory.config_path());
    assert_eq!(
        config.logging_settings().as_ref(),
        &LoggingSettings {
            level: LogLevel::Debug,
            rotation: false,
            compression: false,
            max_size_mb: 25,
            keep_files: 20,
        }
    );

    config
        .set_logging_settings(LoggingSettings {
            level: LogLevel::Warn,
            rotation: true,
            compression: true,
            max_size_mb: 10,
            keep_files: 10,
        })
        .expect("日志配置应当可以持久化");

    let reloaded = LunaConfig::load_from(directory.config_path());
    assert_eq!(
        reloaded.logging_settings().as_ref(),
        &LoggingSettings {
            level: LogLevel::Warn,
            rotation: true,
            compression: true,
            max_size_mb: 10,
            keep_files: 10,
        }
    );
    let saved = fs::read_to_string(directory.config_path()).expect("日志配置应当可以读取");
    assert!(saved.contains("level = \"warn\""));
    assert!(saved.contains("rotation = true"));
    assert!(saved.contains("compression = true"));
    assert!(saved.contains("max_size_mb = 10"));
    assert!(saved.contains("keep_files = 10"));
}

#[test]
fn appearance_and_model_window_size_round_trip() {
    let directory = TestDirectory::new();
    directory.write(
        r##"[window]
model_size = "large"

[appearance]
language = "ja"
theme = "custom"
custom_mode = "light"
custom_accent = "#2563eb"
custom_background = "#f8fafc"
"##,
    );
    let config = LunaConfig::load_from(directory.config_path());

    assert_eq!(config.model_window_size(), ModelWindowSize::Large);
    assert_eq!(config.appearance().language, AppLanguage::Japanese);
    assert_eq!(config.appearance().theme, ThemePreset::Custom);
    assert_eq!(config.appearance().custom.mode, ThemeMode::Light);
    assert_eq!(config.appearance().custom.accent, "#2563EB");

    let size_revision = config.reserve_model_window_size_revision();
    config
        .set_model_window_size_at_revision(ModelWindowSize::Compact, size_revision)
        .expect("模型窗口尺寸应当可以持久化")
        .expect("最新模型窗口尺寸请求应当生效");
    let mut appearance = config.appearance().as_ref().clone();
    appearance.language = AppLanguage::TraditionalChinese;
    appearance.theme = ThemePreset::Ocean;
    let appearance_revision = config.reserve_appearance_revision();
    config
        .set_appearance_at_revision(appearance, appearance_revision)
        .expect("外观配置应当可以持久化")
        .expect("最新外观配置请求应当生效");

    let reloaded = LunaConfig::load_from(directory.config_path());
    assert_eq!(reloaded.model_window_size(), ModelWindowSize::Compact);
    assert_eq!(
        reloaded.appearance().language,
        AppLanguage::TraditionalChinese
    );
    assert_eq!(reloaded.appearance().theme, ThemePreset::Ocean);
}

#[test]
fn custom_frame_rates_accept_every_positive_u16_and_preserve_their_mode() {
    assert!(matches!(FrameRate::try_from(30), Ok(FrameRate::Fps30)));
    assert!(matches!(FrameRate::try_from(60), Ok(FrameRate::Fps60)));
    assert!(matches!(FrameRate::try_from(120), Ok(FrameRate::Fps120)));
    assert_eq!(
        FrameRate::try_from(75).expect("正整数应当可以作为自定义帧率"),
        FrameRate::custom(75).expect("测试帧率必须有效")
    );
    assert!(matches!(FrameRate::custom(60), Ok(FrameRate::Custom(_))));
    assert!(FrameRate::custom(u16::MAX).is_ok());
    assert!(FrameRate::custom(0).is_err());
    assert!(FrameRate::try_from(0).is_err());
}

#[test]
fn frame_rate_atomic_encoding_preserves_non_numeric_modes() {
    let custom = FrameRate::custom(60).expect("测试帧率必须有效");
    for frame_rate in [custom, FrameRate::FollowDisplay, FrameRate::Unlimited] {
        assert_eq!(
            FrameRate::from_atomic_value(frame_rate.atomic_value()),
            frame_rate
        );
    }
}

#[test]
fn custom_frame_rate_round_trips_with_an_explicit_mode_marker() {
    let directory = TestDirectory::new();
    directory.write(
        r#"[render]
frame_rate = "custom"
custom_frame_rate = 240
"#,
    );
    let config = LunaConfig::load_from(directory.config_path());
    assert_eq!(
        config.frame_rate(),
        FrameRate::custom(240).expect("测试帧率必须有效")
    );

    config
        .set_frame_rate(FrameRate::custom(360).expect("测试帧率必须有效"))
        .expect("自定义帧率应当可以持久化");

    let reloaded = LunaConfig::load_from(directory.config_path());
    assert!(matches!(reloaded.frame_rate(), FrameRate::Custom(fps) if fps.get() == 360));
    let saved = fs::read_to_string(directory.config_path()).expect("帧率配置应当可以读取");
    assert!(saved.contains("frame_rate = \"custom\""));
    assert!(saved.contains("custom_frame_rate = 360"));

    config
        .set_frame_rate(FrameRate::FollowDisplay)
        .expect("离开自定义档位时应当可以保存新模式");
    let saved = fs::read_to_string(directory.config_path()).expect("帧率配置应当可以读取");
    assert!(saved.contains("frame_rate = \"display\""));
    assert!(!saved.contains("custom_frame_rate"));
}

#[test]
fn follow_display_frame_rate_loads_and_persists_as_named_mode() {
    let directory = TestDirectory::new();
    directory.write(
        r#"[render]
frame_rate = "display"
"#,
    );
    let config = LunaConfig::load_from(directory.config_path());
    assert_eq!(config.frame_rate(), FrameRate::FollowDisplay);

    config
        .set_frame_rate(FrameRate::Fps60)
        .expect("固定帧率应当可以覆盖跟随显示器模式");
    config
        .set_frame_rate(FrameRate::FollowDisplay)
        .expect("跟随显示器模式应当可以持久化");

    let reloaded = LunaConfig::load_from(directory.config_path());
    assert_eq!(reloaded.frame_rate(), FrameRate::FollowDisplay);
    let saved = fs::read_to_string(directory.config_path()).expect("帧率配置应当可以读取");
    assert!(saved.contains("frame_rate = \"display\""));
}

#[test]
fn unlimited_frame_rate_loads_and_persists_as_named_mode() {
    let directory = TestDirectory::new();
    directory.write(
        r#"[render]
frame_rate = "unlimited"
"#,
    );
    let config = LunaConfig::load_from(directory.config_path());

    assert_eq!(config.frame_rate(), FrameRate::Unlimited);
    config
        .set_frame_rate(FrameRate::Fps60)
        .expect("有限帧率应当可以覆盖无限制模式");
    config
        .set_frame_rate(FrameRate::Unlimited)
        .expect("无限制模式应当可以持久化");

    let reloaded = LunaConfig::load_from(directory.config_path());
    assert_eq!(reloaded.frame_rate(), FrameRate::Unlimited);
    let saved = fs::read_to_string(directory.config_path()).expect("帧率配置应当可以读取");
    assert!(saved.contains("frame_rate = \"unlimited\""));
}

#[test]
fn precise_edit_preserves_comments_and_unrelated_keys() {
    let directory = TestDirectory::new();
    directory.write(
        r#"# 用户注释
[render]
frame_rate = 30 # 保留行尾注释
quality = "custom"

[provider]
name = "local"
"#,
    );
    let config = LunaConfig::load_from(directory.config_path());

    config
        .set_frame_rate(FrameRate::Fps60)
        .expect("有效配置应当可以修改");
    let saved = fs::read_to_string(directory.config_path()).expect("修改后的配置应当可以读取");

    assert!(saved.contains("# 用户注释"));
    assert!(saved.contains("# 保留行尾注释"));
    assert!(saved.contains("frame_rate = 60"));
    assert!(saved.contains("quality = \"custom\""));
    assert!(saved.contains("name = \"local\""));
}

#[test]
fn window_positions_are_cached_then_persisted_together() {
    let directory = TestDirectory::new();
    let config = LunaConfig::load_from(directory.config_path());
    config.cache_window_position(
        ConfigWindow::DesktopPet,
        WindowPosition::new(10.0, 20.0).expect("测试坐标必须有效"),
    );
    config.cache_window_position(
        ConfigWindow::Settings,
        WindowPosition::new(30.0, 40.0).expect("测试坐标必须有效"),
    );
    config
        .persist_window_positions()
        .expect("窗口位置应当可以集中保存");

    let reloaded = LunaConfig::load_from(directory.config_path());
    assert_eq!(
        reloaded.window_position(ConfigWindow::DesktopPet),
        WindowPosition::new(10.0, 20.0)
    );
    assert_eq!(
        reloaded.window_position(ConfigWindow::Settings),
        WindowPosition::new(30.0, 40.0)
    );
}

#[test]
fn reset_window_positions_clears_memory_and_persisted_tables() {
    let directory = TestDirectory::new();
    let config = LunaConfig::load_from(directory.config_path());
    config.cache_window_position(
        ConfigWindow::DesktopPet,
        WindowPosition::new(10.0, 20.0).expect("测试坐标必须有效"),
    );
    config.cache_window_position(
        ConfigWindow::Settings,
        WindowPosition::new(30.0, 40.0).expect("测试坐标必须有效"),
    );
    config
        .persist_window_positions()
        .expect("测试位置应当可以写入");

    config
        .reset_window_positions()
        .expect("窗口位置应当可以重置");

    assert_eq!(config.window_position(ConfigWindow::DesktopPet), None);
    assert_eq!(config.window_position(ConfigWindow::Settings), None);
    let reloaded = LunaConfig::load_from(directory.config_path());
    assert_eq!(reloaded.window_position(ConfigWindow::DesktopPet), None);
    assert_eq!(reloaded.window_position(ConfigWindow::Settings), None);
}

#[test]
fn independent_config_fields_do_not_cancel_each_other() {
    let directory = TestDirectory::new();
    let config = LunaConfig::load_from(directory.config_path());
    let model_revision = config.reserve_model_revision();
    let frame_revision = config.reserve_frame_rate_revision();

    assert_eq!(
        config
            .set_selected_model_at_revision(
                Some(Path::new("luna/luna.model3.json")),
                model_revision,
            )
            .expect("模型选择应当可以保存"),
        Some(())
    );
    assert_eq!(
        config
            .set_frame_rate_at_revision(FrameRate::Fps60, frame_revision)
            .expect("帧率应当可以保存"),
        Some(())
    );

    let reloaded = LunaConfig::load_from(directory.config_path());
    assert_eq!(reloaded.frame_rate(), FrameRate::Fps60);
    assert_eq!(
        reloaded.selected_model(),
        Some(PathBuf::from("luna/luna.model3.json"))
    );
}
