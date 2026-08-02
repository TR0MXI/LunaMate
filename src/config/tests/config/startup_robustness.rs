//! 验证配置启动加载、局部回退与损坏内容修复。

use std::{fs, process::Command};

use gpui_component::ThemeMode;
use toml_edit::{DocumentMut, Item};

use super::TestDirectory;
use crate::config::{document::nested_item, *};

const NO_CONFIG_DIRECTORY_CHILD: &str = "LUNAMATE_TEST_NO_CONFIG_DIRECTORY_CHILD";

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
fn missing_platform_config_directory_does_not_read_working_directory() {
    if std::env::var_os(NO_CONFIG_DIRECTORY_CHILD).is_some() {
        let config = LunaConfig::load();
        assert_eq!(config.frame_rate(), FrameRate::Fps30);
        assert!(
            config
                .startup_warning()
                .is_some_and(|warning| warning.contains("不可持久化"))
        );
        let error = config
            .set_frame_rate(FrameRate::Fps60)
            .expect_err("缺少可信配置目录时保存必须失败");
        assert!(matches!(error, ConfigWriteError::PersistenceUnavailable));
        let working_config = fs::read_to_string("config.toml").expect("工作目录配置应保持可读");
        assert!(working_config.contains("frame_rate = 120"));
        fs::write("child-ran", b"ok").expect("子进程应当写入完成标记");
        return;
    }

    let directory = TestDirectory::new();
    directory.write("[render]\nframe_rate = 120\n");
    let output = Command::new(std::env::current_exe().expect("测试二进制路径应当可用"))
        .args([
            "--exact",
            "config::tests::config::startup_robustness::missing_platform_config_directory_does_not_read_working_directory",
            "--nocapture",
        ])
        .current_dir(&directory.0)
        .env(NO_CONFIG_DIRECTORY_CHILD, "1")
        .env_remove("APPDATA")
        .env_remove("HOME")
        .env_remove("XDG_CONFIG_HOME")
        .output()
        .expect("隔离环境中的配置测试应当可以启动");
    assert!(
        output.status.success(),
        "子进程失败：stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(directory.0.join("child-ran").is_file());
    assert_eq!(
        fs::read_to_string(directory.config_path()).expect("工作目录配置应保持可读"),
        "[render]\nframe_rate = 120\n"
    );
}

#[test]
fn oversized_persona_directory_is_rejected_before_creating_an_unreadable_config() {
    let directory = TestDirectory::new();
    let config = LunaConfig::load_from(directory.config_path());
    let prompt = "x".repeat(64 * 1024);
    let personas = (0..9)
        .map(|index| {
            let mut persona = PersonaConfig::new(format!("p-{index}"), format!("人格 {index}"));
            persona.system_prompt = prompt.clone();
            persona.input_prompt = prompt.clone();
            persona
        })
        .collect();
    let settings = PersonaSettings {
        personas,
        selected: Some("p-0".to_owned()),
        pending_deletions: Vec::new(),
    };

    let revision = config.reserve_persona_settings_revision();
    let error = config
        .set_persona_settings_at_revision(settings, revision, AppLanguage::SimplifiedChinese)
        .expect_err("超过完整配置上限的人格目录必须拒绝写入");

    assert!(matches!(error, ConfigWriteError::InvalidValue(_)));
    assert!(!directory.config_path().exists());
    assert_eq!(
        config
            .persona_settings()
            .active()
            .map(|persona| persona.id.as_str()),
        Some(DEFAULT_PERSONA_ID)
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
fn startup_clears_dangling_and_wrong_capability_persona_bindings() {
    let secret = "startup-secret-must-not-appear";
    for (language, expected_chat_warning, expected_tts_warning) in [
        (
            AppLanguage::SimplifiedChinese,
            "人格 dangling 的 persona.model 绑定不存在或能力类型错误，已清除",
            "人格 dangling 的 persona.tts_model 绑定不存在或能力类型错误，已清除",
        ),
        (
            AppLanguage::TraditionalChinese,
            "人格 dangling 的 persona.model 綁定不存在或能力類型錯誤，已清除",
            "人格 dangling 的 persona.tts_model 綁定不存在或能力類型錯誤，已清除",
        ),
        (
            AppLanguage::English,
            "The persona.model binding for persona dangling is missing or has the wrong capability, so it was cleared",
            "The persona.tts_model binding for persona dangling is missing or has the wrong capability, so it was cleared",
        ),
        (
            AppLanguage::Japanese,
            "ペルソナ dangling の persona.model バインドは、参照先が存在しないか機能種別が一致しないため解除しました",
            "ペルソナ dangling の persona.tts_model バインドは、参照先が存在しないか機能種別が一致しないため解除しました",
        ),
    ] {
        let directory = TestDirectory::new();
        directory.write(&format!(
            r#"[appearance]
language = "{}"

[llm]
selected = "valid-chat-model-id"

[[llm.models]]
id = "valid-chat-model-id"
label = "Chat"
kind = "chat-completions"
provider = "ollama"
model = "provider-chat-model-id"

[[llm.models]]
id = "valid-tts-model-id"
label = "Voice"
kind = "speech-synthesis"
provider = "openai"
model = "provider-tts-model-id"
api_key = "{secret}"
voice = "alloy"

[persona]
selected = "dangling"

[[persona.list]]
id = "dangling"
name = "悬空绑定"
model = "missing-chat-model-id"
tts_model = "missing-tts-model-id"

[[persona.list]]
id = "wrong-kind"
name = "能力错误"
model = "valid-tts-model-id"
tts_model = "valid-chat-model-id"
"#,
            language.id()
        ));

        let config = LunaConfig::load_from(directory.config_path());

        for persona in &config.persona_settings().personas {
            assert_eq!(persona.model, None);
            assert_eq!(persona.tts_model, None);
        }
        let warning = config
            .startup_warning()
            .expect("无效人格模型绑定必须产生启动诊断");
        assert!(
            warning.contains(expected_chat_warning),
            "语言：{language:?}"
        );
        assert!(warning.contains(expected_tts_warning), "语言：{language:?}");
        assert_eq!(warning.matches("persona.model").count(), 2);
        assert_eq!(warning.matches("persona.tts_model").count(), 2);
        for sensitive in [
            secret,
            "missing-chat-model-id",
            "missing-tts-model-id",
            "valid-chat-model-id",
            "valid-tts-model-id",
            "provider-chat-model-id",
            "provider-tts-model-id",
        ] {
            assert!(
                !warning.contains(sensitive),
                "启动诊断不得包含模型 ID 或 API key：{sensitive}"
            );
        }
        let snapshot = config.agent_config_snapshot();
        assert_eq!(snapshot.settings().models.len(), 2);
        assert!(
            snapshot
                .personas()
                .personas
                .iter()
                .all(|persona| persona.model.is_none() && persona.tts_model.is_none())
        );
    }
}

#[test]
fn final_agent_validation_aggregates_a_warning_and_falls_back_as_one_domain() {
    let secret = "binding-secret-must-not-appear";
    let mut loaded = LoadedConfig::default();
    loaded
        .persona
        .personas
        .first_mut()
        .expect("默认人格必须存在")
        .model = Some(secret.to_owned());
    let mut warning = Some("既有启动诊断".to_owned());

    let snapshot = finalize_loaded_agent_config(&mut loaded, &mut warning);

    assert_eq!(loaded.llm, LlmSettings::default());
    assert_eq!(
        loaded.persona,
        PersonaSettings::default_for(AppLanguage::SimplifiedChinese)
    );
    assert_eq!(snapshot.settings().as_ref(), &loaded.llm);
    assert_eq!(snapshot.personas().as_ref(), &loaded.persona);
    let warning = warning.expect("终检失败必须聚合启动诊断");
    assert!(warning.contains("既有启动诊断"));
    assert!(warning.contains("整体回退默认值"));
    assert!(!warning.contains(secret));
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
fn wrong_known_section_types_warn_and_default_each_domain() {
    let directory = TestDirectory::new();
    directory.write(
        r#"render = 60
tools = true
logging = "debug"
interaction = false
window = []
model = "luna.model3.json"
appearance = 1
llm = false
persona = []
shortcuts = "Control+KeyS"
voice = "auto"
debug = { show_fps = true }
"#,
    );

    let config = LunaConfig::load_from(directory.config_path());

    assert_eq!(config.frame_rate(), FrameRate::Fps30);
    assert!(config.allow_agent_outfit_change());
    assert_eq!(
        config.logging_settings().as_ref(),
        &LoggingSettings::default()
    );
    assert!(config.eye_tracking());
    assert!(config.remember_window_positions());
    assert_eq!(config.selected_model(), None);
    assert_eq!(config.appearance().as_ref(), &AppearanceSettings::default());
    assert_eq!(config.llm_settings().as_ref(), &LlmSettings::default());
    assert_eq!(config.voice_settings().as_ref(), &VoiceSettings::default());
    assert_eq!(config.shortcut_settings().configured_count(), 0);
    assert!(
        config.show_fps(),
        "合法内联 table-like section 必须继续读取"
    );
    let warning = config
        .startup_warning()
        .expect("错误 section 类型必须产生启动诊断");
    for section in [
        "render",
        "tools",
        "logging",
        "interaction",
        "window",
        "model",
        "appearance",
        "llm",
        "persona",
        "shortcuts",
        "voice",
    ] {
        assert!(
            warning.contains(&format!("{section} 必须是 TOML 表")),
            "缺少 {section} section 诊断：{warning}"
        );
    }
}

#[test]
fn wrong_optional_persona_field_types_fall_back_independently() {
    let directory = TestDirectory::new();
    directory.write(
        r#"[persona]
selected = "moon"

[[persona.list]]
id = "moon"
name = "保留的人格"
system_prompt = 42
model = false
tts_model = []
"#,
    );

    let config = LunaConfig::load_from(directory.config_path());
    let personas = config.persona_settings();
    let persona = personas.active().expect("其余有效人格字段必须保留");

    assert_eq!(persona.name, "保留的人格");
    assert!(persona.system_prompt.is_empty());
    assert_eq!(persona.model, None);
    assert_eq!(persona.tts_model, None);
    let warning = config
        .startup_warning()
        .expect("人格可选字段错误类型必须产生诊断");
    for field in ["system_prompt", "model", "tts_model"] {
        assert!(
            warning.contains(&format!("persona.list[0].{field}")),
            "缺少 {field} 字段诊断：{warning}"
        );
    }
}

#[test]
fn one_invalid_custom_color_keeps_valid_appearance_siblings() {
    let directory = TestDirectory::new();
    directory.write(
        r##"[appearance]
language = "ja"
theme = "custom"
custom_mode = "light"
custom_accent = "not-a-color"
custom_background = "#abcdef"
"##,
    );

    let config = LunaConfig::load_from(directory.config_path());
    let appearance = config.appearance();

    assert_eq!(appearance.language, AppLanguage::Japanese);
    assert_eq!(appearance.theme, ThemePreset::Custom);
    assert_eq!(appearance.custom.mode, ThemeMode::Light);
    assert_eq!(appearance.custom.accent, "#2DD4BF");
    assert_eq!(appearance.custom.background, "#ABCDEF");
    let warning = config.startup_warning().expect("损坏颜色必须产生启动诊断");
    assert!(warning.contains("appearance.custom_accent"));
    assert!(!warning.contains("appearance.custom_background 无效"));
}

#[test]
fn agent_config_parsing_uses_the_stored_appearance_language() {
    let directory = TestDirectory::new();
    directory.write(
        r#"[appearance]
language = "ja"

[[llm.models]]
id = "bad/id"
label = "Broken"
kind = "chat-completions"
provider = "ollama"
model = "model"
"#,
    );

    let config = LunaConfig::load_from(directory.config_path());

    assert_eq!(
        config
            .persona_settings()
            .active()
            .map(|persona| persona.name.as_str()),
        Some("既定のペルソナ")
    );
    assert!(config.startup_warning().is_some_and(|warning| {
        warning.contains("モデル IDには ASCII の英字、数字、-、_ のみ使用できます")
    }));
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
