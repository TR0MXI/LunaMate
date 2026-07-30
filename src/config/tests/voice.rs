use std::path::PathBuf;

use toml_edit::DocumentMut;

use crate::config::{VoiceMode, VoiceSettings, parse_voice_settings, write_voice_settings};

#[test]
fn voice_mode_ids_round_trip() {
    for mode in [VoiceMode::Off, VoiceMode::Auto, VoiceMode::PushToTalk] {
        assert_eq!(VoiceMode::from_id(mode.id()), Some(mode));
    }
}

#[test]
fn automatic_mode_combines_vad_and_shortcut_capabilities() {
    assert!(!VoiceMode::Off.uses_vad());
    assert!(!VoiceMode::Off.supports_push_to_talk());
    assert!(VoiceMode::Auto.uses_vad());
    assert!(VoiceMode::Auto.supports_push_to_talk());
    assert!(!VoiceMode::PushToTalk.uses_vad());
    assert!(VoiceMode::PushToTalk.supports_push_to_talk());
}

#[test]
fn voice_settings_round_trip_without_losing_unrelated_keys() {
    let mut document = "[custom]\nkeep = true\n[voice]\nvad_model = \"/models/legacy-vad.bin\"\n"
        .parse::<DocumentMut>()
        .expect("测试配置应当可以解析");
    let settings = VoiceSettings {
        mode: VoiceMode::Auto,
        whisper_model: Some(PathBuf::from("/models/ggml-small.bin")),
        use_gpu: true,
    };

    write_voice_settings(&mut document, &settings);
    let mut warnings = Vec::new();
    let restored = parse_voice_settings(&document, &mut warnings);

    assert!(warnings.is_empty());
    assert_eq!(restored, settings);
    assert_eq!(document["custom"]["keep"].as_bool(), Some(true));
    assert!(document["voice"].get("vad_model").is_none());
}

#[test]
fn malformed_voice_fields_fail_closed_independently() {
    let document = r#"[voice]
mode = "always"
whisper_model = 42
vad_model = "/models/vad.bin"
use_gpu = "yes"
"#
    .parse::<DocumentMut>()
    .expect("测试配置应当可以解析");
    let mut warnings = Vec::new();

    let settings = parse_voice_settings(&document, &mut warnings);

    assert_eq!(settings.mode, VoiceMode::Off);
    assert_eq!(settings.whisper_model, None);
    assert!(!settings.use_gpu);
    assert_eq!(warnings.len(), 3);
}

#[test]
fn model_paths_are_trimmed_and_bounded() {
    let settings = VoiceSettings {
        whisper_model: Some(PathBuf::from("  /models/whisper.bin  ")),
        ..VoiceSettings::default()
    }
    .normalized()
    .expect("普通 UTF-8 模型路径应当有效");

    assert_eq!(
        settings.whisper_model,
        Some(PathBuf::from("/models/whisper.bin"))
    );
    let oversized = VoiceSettings {
        whisper_model: Some(PathBuf::from("a".repeat(4 * 1024 + 1))),
        ..VoiceSettings::default()
    };
    assert!(oversized.normalized().is_err());
}
