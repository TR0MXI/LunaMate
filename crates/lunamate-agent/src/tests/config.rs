//! 验证运行时配置快照只接受并保存规范化后的两个配置域。

use std::sync::Arc;

use crate::config::{
    AgentConfigSnapshot, AppLanguage, LlmAdvancedOptions, LlmModelConfig, LlmProvider, LlmSettings,
    ModelKind, ModelProvider, PersonaConfig, PersonaSettings, WHISPER_LANGUAGE_CODES,
};

#[test]
fn snapshot_constructor_normalizes_both_settings_domains() {
    let settings = Arc::new(LlmSettings {
        models: vec![LlmModelConfig {
            id: " local-model ".to_owned(),
            label: " Local Model ".to_owned(),
            kind: ModelKind::ChatCompletions,
            provider: ModelProvider::Genai(LlmProvider::Ollama),
            model: " qwen3:8b ".to_owned(),
            endpoint: Some(" http://localhost:11434/v1 ".to_owned()),
            api_key: Some(" local-key ".to_owned()),
            voice: None,
            voice_type: None,
            local_path: None,
            use_gpu: false,
            whisper_language: None,
            advanced: LlmAdvancedOptions::default(),
        }],
        selected_model: Some(" local-model ".to_owned()),
        selected_transcription_model: None,
    });
    let mut persona = PersonaConfig::new(" assistant ", " Assistant ");
    persona.model = Some(" local-model ".to_owned());
    let personas = Arc::new(PersonaSettings {
        personas: vec![persona],
        selected: Some(" assistant ".to_owned()),
        pending_deletions: vec![" removed ".to_owned(), "removed".to_owned()],
    });

    let snapshot =
        AgentConfigSnapshot::try_new(41, settings, personas, AppLanguage::TraditionalChinese)
            .expect("可规范化的两个配置域应当被接受");

    assert_eq!(snapshot.generation(), 41);
    assert_eq!(snapshot.language(), AppLanguage::TraditionalChinese);
    let model = &snapshot.settings().models[0];
    assert_eq!(model.id, "local-model");
    assert_eq!(model.label, "Local Model");
    assert_eq!(model.model, "qwen3:8b");
    assert_eq!(
        model.endpoint.as_deref(),
        Some("http://localhost:11434/v1/")
    );
    assert_eq!(model.api_key.as_deref(), Some("local-key"));
    assert_eq!(
        snapshot.settings().selected_model.as_deref(),
        Some("local-model")
    );
    let persona = &snapshot.personas().personas[0];
    assert_eq!(persona.id, "assistant");
    assert_eq!(persona.name, "Assistant");
    assert_eq!(persona.model.as_deref(), Some("local-model"));
    assert_eq!(snapshot.personas().selected.as_deref(), Some("assistant"));
    assert_eq!(snapshot.personas().pending_deletions, ["removed"]);
}

#[test]
fn snapshot_constructor_rejects_invalid_provider_settings() {
    let settings = Arc::new(LlmSettings {
        models: vec![LlmModelConfig {
            id: "invalid/id".to_owned(),
            label: "Invalid".to_owned(),
            kind: ModelKind::ChatCompletions,
            provider: ModelProvider::Genai(LlmProvider::Ollama),
            model: "model".to_owned(),
            endpoint: None,
            api_key: None,
            voice: None,
            voice_type: None,
            local_path: None,
            use_gpu: false,
            whisper_language: None,
            advanced: LlmAdvancedOptions::default(),
        }],
        selected_model: None,
        selected_transcription_model: None,
    });

    assert!(
        AgentConfigSnapshot::try_new(
            1,
            settings,
            Arc::new(PersonaSettings::default()),
            AppLanguage::English,
        )
        .is_err()
    );
}

#[test]
fn snapshot_constructor_rejects_invalid_persona_settings() {
    let personas = Arc::new(PersonaSettings {
        personas: Vec::new(),
        selected: None,
        pending_deletions: Vec::new(),
    });

    assert!(
        AgentConfigSnapshot::try_new(
            1,
            Arc::new(LlmSettings::default()),
            personas,
            AppLanguage::English,
        )
        .is_err()
    );
}

#[test]
fn snapshot_validation_uses_its_explicit_language() {
    let cases = [
        (
            AppLanguage::SimplifiedChinese,
            "模型 ID 只能包含 ASCII 字母、数字、- 和 _",
        ),
        (
            AppLanguage::TraditionalChinese,
            "模型 ID 只能包含 ASCII 字母、數字、- 和 _",
        ),
        (
            AppLanguage::English,
            "Model ID may contain only ASCII letters, digits, - and _",
        ),
        (
            AppLanguage::Japanese,
            "モデル IDには ASCII の英字、数字、-、_ のみ使用できます",
        ),
    ];

    for (language, expected) in cases {
        let settings = LlmSettings {
            models: vec![LlmModelConfig {
                id: "invalid/id".to_owned(),
                label: "Invalid".to_owned(),
                kind: ModelKind::ChatCompletions,
                provider: ModelProvider::Genai(LlmProvider::Ollama),
                model: "model".to_owned(),
                endpoint: None,
                api_key: None,
                voice: None,
                voice_type: None,
                local_path: None,
                use_gpu: false,
                whisper_language: None,
                advanced: LlmAdvancedOptions::default(),
            }],
            selected_model: None,
            selected_transcription_model: None,
        };
        let error = AgentConfigSnapshot::try_new(
            1,
            Arc::new(settings),
            Arc::new(PersonaSettings::default_for(language)),
            language,
        )
        .err()
        .expect("非法模型 ID 必须被拒绝");

        assert_eq!(error.to_string(), expected);
    }
}

#[test]
fn default_persona_name_uses_the_requested_language() {
    let cases = [
        (AppLanguage::SimplifiedChinese, "默认人格"),
        (AppLanguage::TraditionalChinese, "預設人格"),
        (AppLanguage::English, "Default persona"),
        (AppLanguage::Japanese, "既定のペルソナ"),
    ];

    for (language, expected) in cases {
        assert_eq!(
            PersonaSettings::default_for(language)
                .active()
                .map(|persona| persona.name.as_str()),
            Some(expected)
        );
    }
}

#[test]
fn model_capabilities_reject_cross_kind_defaults_and_normalize_local_whisper() {
    let local = LlmModelConfig {
        id: "local-stt".to_owned(),
        label: "Local Whisper".to_owned(),
        kind: ModelKind::Transcription,
        provider: ModelProvider::LocalWhisper,
        model: String::new(),
        endpoint: Some("https://should-be-cleared.example".to_owned()),
        api_key: Some("should-be-cleared".to_owned()),
        voice: Some("should-be-cleared".to_owned()),
        voice_type: Some("should-also-be-cleared".to_owned()),
        local_path: Some(" /models/ggml-small.bin ".into()),
        use_gpu: true,
        whisper_language: Some(" zh ".to_owned()),
        advanced: LlmAdvancedOptions {
            max_output_tokens: Some(20),
            ..LlmAdvancedOptions::default()
        },
    };
    let settings = LlmSettings {
        models: vec![local.clone()],
        selected_model: Some("local-stt".to_owned()),
        selected_transcription_model: Some("local-stt".to_owned()),
    };

    assert!(settings.clone().normalized(AppLanguage::English).is_err());
    let normalized = LlmSettings {
        selected_model: None,
        ..settings
    }
    .normalized(AppLanguage::English)
    .expect("本地 Whisper 应作为 Transcription 模型接受");
    let model = &normalized.models[0];
    assert_eq!(model.model, "whisper");
    assert_eq!(
        model.local_path.as_deref(),
        Some(std::path::Path::new("/models/ggml-small.bin"))
    );
    assert!(model.endpoint.is_none());
    assert!(model.api_key.is_none());
    assert!(model.use_gpu);
    assert_eq!(model.whisper_language.as_deref(), Some("zh"));
    assert_eq!(
        normalized.selected_transcription_model.as_deref(),
        Some("local-stt")
    );
    assert_eq!(model.advanced, LlmAdvancedOptions::default());

    for language in WHISPER_LANGUAGE_CODES {
        assert!(
            LlmModelConfig {
                whisper_language: Some(language.to_owned()),
                ..local.clone()
            }
            .normalized(AppLanguage::English)
            .is_ok(),
            "Whisper 语言代码 {language} 应当受支持"
        );
    }
    assert!(
        LlmModelConfig {
            whisper_language: Some("unsupported".to_owned()),
            ..local
        }
        .normalized(AppLanguage::English)
        .is_err()
    );
}

#[test]
fn persona_tts_binding_must_reference_a_speech_synthesis_model() {
    let chat = LlmModelConfig {
        id: "chat".to_owned(),
        label: "Chat".to_owned(),
        kind: ModelKind::ChatCompletions,
        provider: ModelProvider::Genai(LlmProvider::OpenAI),
        model: "gpt-5-mini".to_owned(),
        endpoint: None,
        api_key: None,
        voice: None,
        voice_type: None,
        local_path: None,
        use_gpu: false,
        whisper_language: None,
        advanced: LlmAdvancedOptions::default(),
    };
    let settings = Arc::new(LlmSettings {
        models: vec![chat],
        selected_model: Some("chat".to_owned()),
        selected_transcription_model: None,
    });
    let mut persona = PersonaConfig::new("default", "Default");
    persona.tts_model = Some("chat".to_owned());

    assert!(
        AgentConfigSnapshot::try_new(
            1,
            settings,
            Arc::new(PersonaSettings {
                personas: vec![persona],
                selected: Some("default".to_owned()),
                pending_deletions: Vec::new(),
            }),
            AppLanguage::English,
        )
        .is_err()
    );
}

#[test]
fn doubao_speech_models_accept_only_websocket_endpoints() {
    let model = LlmModelConfig {
        id: "doubao-stt".to_owned(),
        label: "Doubao STT".to_owned(),
        kind: ModelKind::Transcription,
        provider: ModelProvider::Doubao,
        model: "volc.bigasr.sauc.duration".to_owned(),
        endpoint: Some(" wss://example.com/api/v3/sauc/bigmodel ".to_owned()),
        api_key: Some("api-key".to_owned()),
        voice: None,
        voice_type: None,
        local_path: None,
        use_gpu: false,
        whisper_language: None,
        advanced: LlmAdvancedOptions::default(),
    };

    let normalized = model
        .clone()
        .normalized(AppLanguage::English)
        .expect("豆包语音模型应接受 WSS endpoint");
    assert_eq!(
        normalized.endpoint.as_deref(),
        Some("wss://example.com/api/v3/sauc/bigmodel")
    );
    assert!(
        LlmModelConfig {
            endpoint: Some("https://example.com/api/v3/sauc/bigmodel".to_owned()),
            ..model
        }
        .normalized(AppLanguage::English)
        .is_err()
    );

    let tts = LlmModelConfig {
        id: "doubao-tts".to_owned(),
        label: "Doubao TTS".to_owned(),
        kind: ModelKind::SpeechSynthesis,
        provider: ModelProvider::Doubao,
        model: "seed-tts-2.0".to_owned(),
        endpoint: None,
        api_key: Some("api-key".to_owned()),
        voice: Some("legacy-voice-value".to_owned()),
        voice_type: None,
        local_path: None,
        use_gpu: false,
        whisper_language: None,
        advanced: LlmAdvancedOptions::default(),
    };
    assert!(tts.clone().normalized(AppLanguage::English).is_err());
    let normalized = LlmModelConfig {
        voice: None,
        voice_type: Some("zh_female_vv_uranus_bigtts".to_owned()),
        ..tts
    }
    .normalized(AppLanguage::English)
    .expect("豆包 TTS 应只接受当前 voice_type 字段");
    assert_eq!(normalized.voice, None);
    assert_eq!(
        normalized.voice_type.as_deref(),
        Some("zh_female_vv_uranus_bigtts")
    );
}

#[test]
fn network_models_accept_remote_plaintext_endpoints() {
    let cases = [
        (
            ModelKind::ChatCompletions,
            ModelProvider::Genai(LlmProvider::Ollama),
            "http://api.example.com/v1",
        ),
        (
            ModelKind::Transcription,
            ModelProvider::Genai(LlmProvider::OpenAI),
            "http://api.example.com/v1",
        ),
        (
            ModelKind::SpeechSynthesis,
            ModelProvider::Genai(LlmProvider::OpenAI),
            "http://api.example.com/v1",
        ),
        (
            ModelKind::Transcription,
            ModelProvider::Doubao,
            "ws://api.example.com/speech",
        ),
        (
            ModelKind::SpeechSynthesis,
            ModelProvider::Doubao,
            "ws://api.example.com/speech",
        ),
        (
            ModelKind::ChatCompletions,
            ModelProvider::Genai(LlmProvider::Ollama),
            "http://localhost.evil/v1",
        ),
        (
            ModelKind::Transcription,
            ModelProvider::Doubao,
            "ws://localhost.evil/speech",
        ),
    ];

    for (kind, provider, endpoint) in cases {
        network_model(kind, provider, endpoint)
            .normalized(AppLanguage::English)
            .expect("远端 HTTP/WS endpoint 应被接受");
    }
}

#[test]
fn network_models_accept_plaintext_loopback_endpoints() {
    let http_endpoints = [
        "http://localhost:11434/v1",
        "http://127.0.0.1:11434/v1",
        "http://[::1]:11434/v1",
    ];
    let http_models = [
        (
            ModelKind::ChatCompletions,
            ModelProvider::Genai(LlmProvider::Ollama),
        ),
        (
            ModelKind::Transcription,
            ModelProvider::Genai(LlmProvider::OpenAI),
        ),
        (
            ModelKind::SpeechSynthesis,
            ModelProvider::Genai(LlmProvider::OpenAI),
        ),
    ];
    for (kind, provider) in http_models {
        for endpoint in http_endpoints {
            network_model(kind, provider, endpoint)
                .normalized(AppLanguage::English)
                .expect("Chat、STT 与 TTS 应接受回环 HTTP endpoint");
        }
    }

    let websocket_endpoints = [
        "ws://localhost:8080/speech",
        "ws://127.0.0.1:8080/speech",
        "ws://[::1]:8080/speech",
    ];
    for kind in [ModelKind::Transcription, ModelKind::SpeechSynthesis] {
        for endpoint in websocket_endpoints {
            network_model(kind, ModelProvider::Doubao, endpoint)
                .normalized(AppLanguage::English)
                .expect("豆包 STT 与 TTS 应接受回环 WS endpoint");
        }
    }
}

#[test]
fn network_models_accept_remote_tls_endpoints() {
    let cases = [
        (
            ModelKind::ChatCompletions,
            ModelProvider::Genai(LlmProvider::Ollama),
            "https://api.example.com/v1",
        ),
        (
            ModelKind::Transcription,
            ModelProvider::Genai(LlmProvider::OpenAI),
            "https://api.example.com/v1",
        ),
        (
            ModelKind::SpeechSynthesis,
            ModelProvider::Genai(LlmProvider::OpenAI),
            "https://api.example.com/v1",
        ),
        (
            ModelKind::Transcription,
            ModelProvider::Doubao,
            "wss://api.example.com/speech",
        ),
        (
            ModelKind::SpeechSynthesis,
            ModelProvider::Doubao,
            "wss://api.example.com/speech",
        ),
    ];

    for (kind, provider, endpoint) in cases {
        network_model(kind, provider, endpoint)
            .normalized(AppLanguage::English)
            .expect("远端 HTTPS/WSS endpoint 应被接受");
    }
}

fn network_model(kind: ModelKind, provider: ModelProvider, endpoint: &str) -> LlmModelConfig {
    LlmModelConfig {
        id: "network-model".to_owned(),
        label: "Network model".to_owned(),
        kind,
        provider,
        model: "model".to_owned(),
        endpoint: Some(endpoint.to_owned()),
        api_key: Some("api-key".to_owned()),
        voice: (kind == ModelKind::SpeechSynthesis
            && provider == ModelProvider::Genai(LlmProvider::OpenAI))
        .then(|| "alloy".to_owned()),
        voice_type: (kind == ModelKind::SpeechSynthesis && provider == ModelProvider::Doubao)
            .then(|| "zh_female_vv_uranus_bigtts".to_owned()),
        local_path: None,
        use_gpu: false,
        whisper_language: None,
        advanced: LlmAdvancedOptions::default(),
    }
}
