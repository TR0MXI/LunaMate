//! 验证配置加载、精确修改、revision 与持久化一致性。

use std::{
    fs,
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

use gpui_component::ThemeMode;
use toml_edit::{DocumentMut, Item};

use crate::config::{document::nested_item, *};

struct TestDirectory(PathBuf);

impl TestDirectory {
    fn new() -> Self {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("系统时间必须晚于 Unix 纪元")
            .as_nanos();
        let path =
            std::env::temp_dir().join(format!("lunamate-config-{}-{nonce}", std::process::id()));
        fs::create_dir_all(&path).expect("测试配置目录应当可以创建");
        Self(path)
    }

    fn config_path(&self) -> PathBuf {
        self.0.join("config.toml")
    }

    fn write(&self, contents: &str) {
        fs::write(self.config_path(), contents).expect("测试配置应当可以写入");
    }
}

impl Drop for TestDirectory {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[test]
fn missing_config_uses_complete_defaults() {
    let directory = TestDirectory::new();
    let config = LunaConfig::load_from(directory.config_path());

    assert_eq!(config.frame_rate(), FrameRate::Fps30);
    assert_eq!(config.model_window_size(), ModelWindowSize::Auto);
    assert!(config.remember_window_positions());
    assert!(config.eye_tracking());
    assert!(!config.show_fps());
    assert!(!config.use_native_tray_menu());
    assert!(!config.allow_agent_screenshot());
    assert!(config.allow_agent_outfit_change());
    assert_eq!(
        config.logging_settings().as_ref(),
        &LoggingSettings::default()
    );
    assert_eq!(config.appearance().as_ref(), &AppearanceSettings::default());
    assert_eq!(config.selected_model(), None);
    assert_eq!(config.shortcut_settings().configured_count(), 0);
    assert!(config.startup_warning().is_none());
}

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
fn malformed_config_warning_does_not_expose_api_key() {
    let directory = TestDirectory::new();
    let secret = "local-secret-must-not-appear";
    directory.write(&format!("[llm]\napi_key = \"{secret}\"\nmodels = [\n"));

    let config = LunaConfig::load_from(directory.config_path());
    let warning = config.startup_warning().expect("损坏配置应当产生启动诊断");

    assert!(!warning.contains(secret));
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
fn invalid_native_tray_menu_switch_uses_custom_default() {
    let directory = TestDirectory::new();
    directory.write("[debug]\nuse_native_tray_menu = \"yes\"\n");

    let config = LunaConfig::load_from(directory.config_path());

    assert!(!config.use_native_tray_menu());
    assert!(config.startup_warning().is_some());
}

#[test]
fn agent_screenshot_permission_is_explicit_and_round_trips() {
    let directory = TestDirectory::new();
    let config = LunaConfig::load_from(directory.config_path());
    assert!(!config.allow_agent_screenshot());

    let enable_revision = config.reserve_allow_agent_screenshot_revision(true);
    assert!(
        !config.allow_agent_screenshot(),
        "授权写入完成前不得开放工具"
    );
    assert_eq!(
        config
            .set_allow_agent_screenshot_at_revision(true, enable_revision)
            .expect("Agent 截屏授权应当可以开启"),
        Some(())
    );
    assert!(config.allow_agent_screenshot());
    assert!(LunaConfig::load_from(directory.config_path()).allow_agent_screenshot());

    let disable_revision = config.reserve_allow_agent_screenshot_revision(false);
    assert!(
        !config.allow_agent_screenshot(),
        "关闭请求一经提交就必须立即撤销运行时授权"
    );
    assert_eq!(
        config
            .set_allow_agent_screenshot_at_revision(false, disable_revision)
            .expect("Agent 截屏授权应当可以关闭"),
        Some(())
    );
    assert!(!config.allow_agent_screenshot());
    let saved = fs::read_to_string(directory.config_path()).expect("工具配置应当可以读取");
    assert!(saved.contains("allow_agent_screenshot = false"));
}

#[test]
fn agent_outfit_tool_switch_defaults_to_enabled_and_round_trips() {
    let directory = TestDirectory::new();
    let config = LunaConfig::load_from(directory.config_path());
    assert!(config.allow_agent_outfit_change());

    let revision = config.reserve_allow_agent_outfit_change_revision();
    assert_eq!(
        config
            .set_allow_agent_outfit_change_at_revision(false, revision)
            .expect("Agent 换装工具应当可以关闭"),
        Some(())
    );
    assert!(!config.allow_agent_outfit_change());
    assert!(!LunaConfig::load_from(directory.config_path()).allow_agent_outfit_change());
    let saved = fs::read_to_string(directory.config_path()).expect("工具配置应当可以读取");
    assert!(saved.contains("allow_agent_outfit_change = false"));
}

#[test]
fn agent_screenshot_permission_revision_notifies_subscribers() {
    let directory = TestDirectory::new();
    let config = LunaConfig::load_from(directory.config_path());
    let mut revisions = config.subscribe_agent_screenshot_permission_revision();
    assert_eq!(*revisions.borrow_and_update(), 0);

    let revision = config.reserve_allow_agent_screenshot_revision(true);

    assert!(
        revisions
            .has_changed()
            .expect("本地授权 revision channel 应当保持开放")
    );
    assert_eq!(*revisions.borrow_and_update(), revision);
}

#[test]
fn invalid_tool_switches_use_their_defaults() {
    let directory = TestDirectory::new();
    directory.write(
        r#"[tools]
allow_agent_screenshot = "yes"
allow_agent_outfit_change = "yes"

[debug]
show_fps = true
"#,
    );

    let config = LunaConfig::load_from(directory.config_path());
    assert!(!config.allow_agent_screenshot());
    assert!(config.allow_agent_outfit_change());
    assert!(config.show_fps());
    assert!(config.startup_warning().is_some());
}

#[test]
fn stale_screenshot_enable_cannot_override_newer_disable() {
    let directory = TestDirectory::new();
    let config = LunaConfig::load_from(directory.config_path());
    let stale_enable = config.reserve_allow_agent_screenshot_revision(true);
    let current_disable = config.reserve_allow_agent_screenshot_revision(false);

    assert_eq!(
        config
            .set_allow_agent_screenshot_at_revision(false, current_disable)
            .expect("最新关闭请求应当可以保存"),
        Some(())
    );
    assert_eq!(
        config
            .set_allow_agent_screenshot_at_revision(true, stale_enable)
            .expect("迟到开启请求应当被无害丢弃"),
        None
    );
    assert!(!config.allow_agent_screenshot());
    assert!(!LunaConfig::load_from(directory.config_path()).allow_agent_screenshot());
}

#[test]
fn failed_screenshot_disable_stays_closed_when_config_path_becomes_unreadable() {
    let directory = TestDirectory::new();
    directory.write("[tools]\nallow_agent_screenshot = true\n");
    let config_path = directory.config_path();
    let config = LunaConfig::load_from(config_path.clone());
    assert!(config.allow_agent_screenshot());
    fs::remove_file(&config_path).expect("测试配置文件应当可以移除");
    fs::create_dir(&config_path).expect("冲突目标目录应当可以创建");

    let revision = config.reserve_allow_agent_screenshot_revision(false);
    assert!(!config.allow_agent_screenshot());
    let result = config.set_allow_agent_screenshot_at_revision(false, revision);

    assert!(matches!(result, Err(ConfigWriteError::Io { .. })));
    assert!(
        !config.allow_agent_screenshot(),
        "配置路径已不可读时必须保持截屏权限关闭"
    );
    assert!(!config.requested_allow_agent_screenshot());
    assert!(config.agent_screenshot_permission_retry_required());

    fs::remove_dir(&config_path).expect("冲突目标目录应当可以移除");
    let retry_revision = config.reserve_allow_agent_screenshot_revision(false);
    assert_eq!(
        config
            .set_allow_agent_screenshot_at_revision(false, retry_revision)
            .expect("关闭状态应当可以安全重试"),
        Some(())
    );
    assert!(!config.agent_screenshot_permission_retry_required());
    assert!(!LunaConfig::load_from(config_path).allow_agent_screenshot());
}

#[cfg(target_os = "linux")]
#[test]
fn failed_screenshot_disable_does_not_reopen_permission_from_readable_old_file() {
    use std::os::fd::AsRawFd as _;

    let directory = TestDirectory::new();
    directory.write("[tools]\nallow_agent_screenshot = true\n");
    let readable_file = fs::File::open(directory.config_path()).expect("测试配置文件应当保持可读");
    let read_only_path = PathBuf::from(format!("/proc/self/fd/{}", readable_file.as_raw_fd()));
    let config = LunaConfig::load_from(read_only_path);
    assert!(config.allow_agent_screenshot());

    let revision = config.reserve_allow_agent_screenshot_revision(false);
    let result = config.set_allow_agent_screenshot_at_revision(false, revision);

    assert!(matches!(result, Err(ConfigWriteError::Io { .. })));
    assert!(!config.allow_agent_screenshot());
    assert!(!config.requested_allow_agent_screenshot());
    assert!(config.agent_screenshot_permission_retry_required());
    assert!(
        LunaConfig::load_from(directory.config_path()).allow_agent_screenshot(),
        "回归前置条件要求旧磁盘授权仍然可读"
    );
}

#[test]
fn failed_screenshot_enable_rolls_back_without_requesting_disable_retry() {
    let directory = TestDirectory::new();
    let config_path = directory.config_path();
    let config = LunaConfig::load_from(config_path.clone());
    fs::create_dir(&config_path).expect("冲突目标目录应当可以创建");

    let revision = config.reserve_allow_agent_screenshot_revision(true);
    let result = config.set_allow_agent_screenshot_at_revision(true, revision);

    assert!(matches!(result, Err(ConfigWriteError::Io { .. })));
    assert!(!config.allow_agent_screenshot());
    assert!(!config.requested_allow_agent_screenshot());
    assert!(!config.agent_screenshot_permission_retry_required());
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
fn invalid_logging_fields_fall_back_independently() {
    let directory = TestDirectory::new();
    directory.write(
        r#"[logging]
level = "verbose"
rotation = "yes"
compression = 1
max_size_mb = 0
keep_files = 101
"#,
    );

    let config = LunaConfig::load_from(directory.config_path());
    assert_eq!(
        config.logging_settings().as_ref(),
        &LoggingSettings::default()
    );
    assert!(config.startup_warning().is_some());
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
fn invalid_custom_frame_rate_payloads_fall_back_to_default() {
    for source in [
        "[render]\nframe_rate = \"custom\"\n",
        "[render]\nframe_rate = \"custom\"\ncustom_frame_rate = 0\n",
        "[render]\nframe_rate = \"custom\"\ncustom_frame_rate = 65536\n",
        "[render]\nframe_rate = \"custom\"\ncustom_frame_rate = \"60\"\n",
    ] {
        let directory = TestDirectory::new();
        directory.write(source);
        let config = LunaConfig::load_from(directory.config_path());

        assert_eq!(config.frame_rate(), FrameRate::Fps30);
        assert!(config.startup_warning().is_some());
    }
}

#[test]
fn legacy_integer_custom_frame_rate_loads_as_custom_mode() {
    let directory = TestDirectory::new();
    directory.write(
        r#"[render]
frame_rate = 75
"#,
    );

    assert_eq!(
        LunaConfig::load_from(directory.config_path()).frame_rate(),
        FrameRate::custom(75).expect("测试帧率必须有效")
    );
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
fn invalid_fields_fall_back_without_rejecting_valid_fields() {
    let directory = TestDirectory::new();
    directory.write(
        r#"[render]
frame_rate = 0

[window]
remember_position = false

[model]
selected = "../outside.model3.json"
"#,
    );

    let config = LunaConfig::load_from(directory.config_path());
    assert_eq!(config.frame_rate(), FrameRate::Fps30);
    assert!(!config.remember_window_positions());
    assert_eq!(config.selected_model(), None);
    assert!(config.startup_warning().is_some());
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
fn atomic_replacement_leaves_a_recoverable_complete_document() {
    let directory = TestDirectory::new();
    directory.write(
        r#"[render]
frame_rate = 30

[custom]
value = "preserved"
"#,
    );
    let config = LunaConfig::load_from(directory.config_path());

    config
        .set_frame_rate(FrameRate::Fps120)
        .expect("配置文件应当可以原子替换");

    let saved = fs::read_to_string(directory.config_path()).expect("替换后的配置应当可以读取");
    saved
        .parse::<DocumentMut>()
        .expect("替换后的配置必须是完整 TOML");
    assert!(saved.contains("value = \"preserved\""));
    assert_eq!(
        LunaConfig::load_from(directory.config_path()).frame_rate(),
        FrameRate::Fps120
    );
    let temporary_files = fs::read_dir(&directory.0)
        .expect("测试目录应当可以读取")
        .filter_map(Result::ok)
        .filter(|entry| {
            entry
                .file_name()
                .to_string_lossy()
                .starts_with(".config.toml.tmp-")
        })
        .count();
    assert_eq!(temporary_files, 0);
}

#[test]
fn failed_write_does_not_publish_runtime_value() {
    let directory = TestDirectory::new();
    let config_path = directory.config_path();
    let config = LunaConfig::load_from(config_path.clone());
    fs::create_dir(&config_path).expect("冲突目标目录应当可以创建");

    let revision = config.reserve_frame_rate_revision();
    let result = config.set_frame_rate_at_revision(FrameRate::Fps120, revision);

    assert!(matches!(result, Err(ConfigWriteError::Io { .. })));
    assert_eq!(config.frame_rate(), FrameRate::Fps30);
    assert!(config_path.is_dir());
}

#[test]
fn malformed_file_is_rebuilt_on_first_ui_update() {
    let directory = TestDirectory::new();
    directory.write("[render\nframe_rate = 30");
    let config = LunaConfig::load_from(directory.config_path());
    assert!(config.startup_warning().is_some());

    config
        .set_remember_window_positions(false)
        .expect("损坏配置应当在修改时重建");
    let saved = fs::read_to_string(directory.config_path()).expect("重建后的配置应当可以读取");
    let parsed = saved
        .parse::<DocumentMut>()
        .expect("重建后的内容必须是有效 TOML");
    assert_eq!(
        nested_item(&parsed, "window", "remember_position").and_then(Item::as_bool),
        Some(false)
    );
}

#[test]
fn wrong_section_type_is_repaired_without_touching_other_sections() {
    let directory = TestDirectory::new();
    directory.write(
        r#"render = "invalid"

[provider]
name = "local"
"#,
    );
    let config = LunaConfig::load_from(directory.config_path());

    config
        .set_frame_rate(FrameRate::Fps120)
        .expect("错误类型的配置节应当可以修复");
    let saved = fs::read_to_string(directory.config_path()).expect("修复后的配置应当可以读取");

    assert!(saved.contains("frame_rate = 120"));
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
fn llm_models_round_trip_with_direct_api_key_and_advanced_options() {
    let directory = TestDirectory::new();
    directory.write(
        r#"# 保留配置注释
[custom]
enabled = true

[llm]
selected = "local"
system_prompt = """你是 LunaMate。
回答保持简洁。"""

[[llm.models]]
id = "local"
label = "本地 Qwen"
provider = "ollama"
model = "qwen3:8b"
endpoint = "http://localhost:11434/"

[[llm.models]]
id = "cloud"
label = "云端模型"
provider = "openai"
model = "gpt-5-mini"
api_key = "test-token+/="
future_option = "keep"
"#,
    );
    let config = LunaConfig::load_from(directory.config_path());
    let loaded = config.llm_settings();
    assert_eq!(loaded.models.len(), 2);
    assert_eq!(
        loaded.selected().map(|model| model.id.as_str()),
        Some("local")
    );
    // 旧版全局提示词在没有 `[persona]` 时迁移为唯一的默认人格。
    let persona = config.persona_settings();
    assert_eq!(persona.personas.len(), 1);
    assert!(
        persona
            .active()
            .expect("默认人格必须存在")
            .system_prompt
            .contains("回答保持简洁")
    );

    let mut edited = loaded.as_ref().clone();
    edited.selected_model = Some("cloud".to_owned());
    if let Some(model) = edited.models.first_mut() {
        model.advanced = LlmAdvancedOptions {
            reasoning_effort: Some(ReasoningEffort::Budget(2_048)),
            max_output_tokens: Some(512),
            temperature: Some(0.5),
            top_p: None,
        };
    }
    let revision = config.reserve_llm_settings_revision();
    config
        .set_llm_settings_at_revision(edited, revision)
        .expect("有效语言模型配置应当可以保存")
        .expect("最新语言模型配置不应被丢弃");

    let saved = fs::read_to_string(directory.config_path()).expect("保存配置应当可以读取");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;

        let mode = fs::metadata(directory.config_path())
            .expect("保存后的配置文件应当存在")
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o600);
    }
    assert!(saved.contains("# 保留配置注释"));
    assert!(saved.contains("enabled = true"));
    assert!(saved.contains("api_key = \"test-token+/=\""));
    assert!(!saved.contains("api_key_env"));
    assert!(saved.contains("future_option = \"keep\""));
    assert!(saved.contains("reasoning_effort = \"budget\""));
    assert!(saved.contains("reasoning_budget = 2048"));
    let reloaded = LunaConfig::load_from(directory.config_path()).llm_settings();
    assert_eq!(
        reloaded.selected().map(|model| model.id.as_str()),
        Some("cloud")
    );
    assert_eq!(
        reloaded.model("local").map(|model| model.advanced),
        Some(LlmAdvancedOptions {
            reasoning_effort: Some(ReasoningEffort::Budget(2_048)),
            max_output_tokens: Some(512),
            temperature: Some(0.5),
            top_p: None,
        })
    );
    assert_eq!(
        reloaded
            .selected()
            .and_then(|model| model.api_key.as_deref()),
        Some("test-token+/=")
    );
}

#[test]
fn inline_llm_table_becomes_a_table_before_models_are_added() {
    let directory = TestDirectory::new();
    directory.write("llm = { system_prompt = \"你好\" }\n");
    let config = LunaConfig::load_from(directory.config_path());
    let settings = LlmSettings {
        models: vec![LlmModelConfig {
            id: "local".to_owned(),
            label: "本地模型".to_owned(),
            provider: LlmProvider::Ollama,
            model: "qwen3:8b".to_owned(),
            endpoint: Some("http://localhost:11434".to_owned()),
            api_key: None,
            advanced: LlmAdvancedOptions::default(),
        }],
        selected_model: Some("local".to_owned()),
    };
    let revision = config.reserve_llm_settings_revision();
    config
        .set_llm_settings_at_revision(settings, revision)
        .expect("内联表配置应当可以保存")
        .expect("最新配置不应被丢弃");

    let saved = fs::read_to_string(directory.config_path()).expect("保存配置应当可以读取");
    assert!(saved.contains("[llm]"), "保存内容：{saved}");
    assert!(saved.contains("[[llm.models]]"));
    let reloaded = LunaConfig::load_from(directory.config_path()).llm_settings();
    assert_eq!(reloaded.models.len(), 1);
    assert_eq!(
        reloaded.selected().map(|model| model.id.as_str()),
        Some("local")
    );
}

#[test]
fn stale_llm_write_cannot_replace_newer_selection() {
    let directory = TestDirectory::new();
    let config = LunaConfig::load_from(directory.config_path());
    let local = LlmSettings {
        models: vec![LlmModelConfig {
            id: "local".to_owned(),
            label: "本地模型".to_owned(),
            provider: LlmProvider::Ollama,
            model: "qwen3:8b".to_owned(),
            endpoint: Some("http://localhost:11434".to_owned()),
            api_key: None,
            advanced: LlmAdvancedOptions::default(),
        }],
        selected_model: Some("local".to_owned()),
    };
    let cloud = LlmSettings {
        models: vec![LlmModelConfig {
            id: "cloud".to_owned(),
            label: "云端模型".to_owned(),
            provider: LlmProvider::OpenAi,
            model: "gpt-5-mini".to_owned(),
            endpoint: None,
            api_key: Some("test-token".to_owned()),
            advanced: LlmAdvancedOptions::default(),
        }],
        selected_model: Some("cloud".to_owned()),
    };
    let old_revision = config.reserve_llm_settings_revision();
    let new_revision = config.reserve_llm_settings_revision();

    assert!(
        config
            .set_llm_settings_at_revision(cloud, new_revision)
            .expect("新配置应当可以保存")
            .is_some()
    );
    assert!(
        config
            .set_llm_settings_at_revision(local, old_revision)
            .expect("旧配置应当被无害丢弃")
            .is_none()
    );
    assert_eq!(
        config
            .llm_settings()
            .selected()
            .map(|model| model.id.as_str()),
        Some("cloud")
    );
}

#[test]
fn stale_frame_rate_write_cannot_replace_newer_value() {
    let directory = TestDirectory::new();
    let config = LunaConfig::load_from(directory.config_path());
    let old_revision = config.reserve_frame_rate_revision();
    let new_revision = config.reserve_frame_rate_revision();

    assert_eq!(
        config
            .set_frame_rate_at_revision(FrameRate::Fps120, new_revision)
            .expect("新帧率应当可以保存"),
        Some(())
    );
    assert_eq!(
        config
            .set_frame_rate_at_revision(FrameRate::Fps30, old_revision)
            .expect("旧帧率应当被无害丢弃"),
        None
    );
    assert_eq!(config.frame_rate(), FrameRate::Fps120);
    assert_eq!(
        LunaConfig::load_from(directory.config_path()).frame_rate(),
        FrameRate::Fps120
    );
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
