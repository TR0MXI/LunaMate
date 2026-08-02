use toml_edit::DocumentMut;

use crate::config::{
    LlmAdvancedOptions, LlmModelConfig, LlmSettings, ModelKind, ModelProvider, VoiceMode,
    VoiceSettings, VoiceTranscriptionBackend, parse_voice_settings, write_voice_settings,
};

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
    let mut document = "[custom]\nkeep = true\n[voice]\nfuture_option = \"keep\"\n"
        .parse::<DocumentMut>()
        .expect("测试配置应当可以解析");
    let settings = VoiceSettings {
        mode: VoiceMode::Auto,
    };

    write_voice_settings(&mut document, &settings);
    let mut warnings = Vec::new();
    let restored = parse_voice_settings(&document, &mut warnings);

    assert!(warnings.is_empty());
    assert_eq!(restored, settings);
    assert_eq!(document["custom"]["keep"].as_bool(), Some(true));
    assert_eq!(document["voice"]["future_option"].as_str(), Some("keep"));
}

#[test]
fn malformed_voice_fields_fail_closed_independently() {
    let document = r#"[voice]
mode = "always"
"#
    .parse::<DocumentMut>()
    .expect("测试配置应当可以解析");
    let mut warnings = Vec::new();

    let settings = parse_voice_settings(&document, &mut warnings);

    assert_eq!(settings.mode, VoiceMode::Off);
    assert_eq!(warnings.len(), 1);
}

#[test]
fn wrong_voice_section_type_warns_and_uses_the_default_domain() {
    let document = "voice = \"auto\"\n"
        .parse::<DocumentMut>()
        .expect("测试配置应当可以解析");
    let mut warnings = Vec::new();

    let settings = parse_voice_settings(&document, &mut warnings);

    assert_eq!(settings, VoiceSettings::default());
    assert_eq!(warnings.len(), 1);
    assert!(warnings[0].contains("voice"));
    assert!(warnings[0].contains("TOML 表"));
}

#[test]
fn selected_local_model_supplies_its_own_gpu_and_language_preferences() {
    let models = LlmSettings {
        models: vec![LlmModelConfig {
            id: "stt-local".to_owned(),
            label: "Local Whisper".to_owned(),
            kind: ModelKind::Transcription,
            provider: ModelProvider::LocalWhisper,
            model: "whisper".to_owned(),
            endpoint: None,
            api_key: None,
            voice: None,
            voice_type: None,
            local_path: Some("/models/ggml-small.bin".into()),
            use_gpu: true,
            whisper_language: Some("zh".to_owned()),
            advanced: LlmAdvancedOptions::default(),
        }],
        selected_model: None,
        selected_transcription_model: Some("stt-local".to_owned()),
    };
    let runtime = VoiceSettings {
        mode: VoiceMode::Auto,
    }
    .runtime(&models);

    assert_eq!(runtime.mode, VoiceMode::Auto);
    assert_eq!(
        runtime.backend,
        Some(VoiceTranscriptionBackend::LocalWhisper(
            "/models/ggml-small.bin".into()
        ))
    );
    assert!(runtime.use_gpu);
    assert_eq!(runtime.whisper_language.as_deref(), Some("zh"));

    let mut automatic = models;
    automatic.models[0].whisper_language = None;
    let runtime = VoiceSettings {
        mode: VoiceMode::Auto,
    }
    .runtime(&automatic);
    assert_eq!(runtime.whisper_language, None);
}

#[test]
fn no_transcription_selection_disables_runtime_voice_input() {
    let settings = VoiceSettings {
        mode: VoiceMode::Auto,
    };

    let runtime = settings.runtime(&LlmSettings::default());

    assert_eq!(runtime.mode, VoiceMode::Off);
    assert!(runtime.backend.is_none());
}
