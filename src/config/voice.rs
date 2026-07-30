//! 定义语音输入模式；Transcription 模型及其本地推理偏好属于模型配置。

use std::{path::PathBuf, sync::Arc};

use lunamate_agent::config::{LlmSettings, ModelProvider};
use toml_edit::{DocumentMut, Value};

use super::{ConfigWriteError, ensure_table_like, remove_key, set_item_value};

/// 控制麦克风何时采集和提交语音。
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) enum VoiceMode {
    /// 不打开麦克风，也不加载语音模型。
    #[default]
    Off,
    /// 持续使用 VAD，并允许语音输入快捷键接管当前录音。
    Auto,
    /// 仅在用户按住语音输入快捷键时录制。
    PushToTalk,
}

impl VoiceMode {
    /// 返回配置文件使用的稳定标识。
    pub(crate) const fn id(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Auto => "auto",
            Self::PushToTalk => "push-to-talk",
        }
    }

    /// 是否持续采集并使用 VAD 自动划分语音。
    pub(crate) const fn uses_vad(self) -> bool {
        matches!(self, Self::Auto)
    }

    /// 是否允许语音输入快捷键控制录音。
    pub(crate) const fn supports_push_to_talk(self) -> bool {
        matches!(self, Self::Auto | Self::PushToTalk)
    }

    pub(super) fn from_id(id: &str) -> Option<Self> {
        match id {
            "off" => Some(Self::Off),
            "auto" => Some(Self::Auto),
            "push-to-talk" => Some(Self::PushToTalk),
            _ => None,
        }
    }
}

/// 一次性发布的语音录音模式配置。
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct VoiceSettings {
    pub(crate) mode: VoiceMode,
}

pub(crate) type SharedVoiceSettings = Arc<VoiceSettings>;

impl VoiceSettings {
    /// 规范化当前语音模式。
    pub(crate) fn normalized(self) -> Result<Self, ConfigWriteError> {
        Ok(self)
    }

    pub(crate) fn runtime(&self, models: &LlmSettings) -> VoiceRuntimeSettings {
        let selected = models.selected_transcription();
        let (backend, use_gpu, whisper_language) = selected
            .and_then(|model| match model.provider {
                ModelProvider::LocalWhisper => model.local_path.clone().map(|path| {
                    (
                        VoiceTranscriptionBackend::LocalWhisper(path),
                        model.use_gpu,
                        model.whisper_language.clone(),
                    )
                }),
                ModelProvider::Genai(_) | ModelProvider::Doubao => Some((
                    VoiceTranscriptionBackend::Remote(model.id.clone()),
                    false,
                    None,
                )),
            })
            .map_or((None, false, None), |(backend, use_gpu, language)| {
                (Some(backend), use_gpu, language)
            });
        VoiceRuntimeSettings {
            mode: if backend.is_some() {
                self.mode
            } else {
                VoiceMode::Off
            },
            backend,
            use_gpu,
            whisper_language,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum VoiceTranscriptionBackend {
    LocalWhisper(PathBuf),
    Remote(String),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct VoiceRuntimeSettings {
    pub(crate) mode: VoiceMode,
    pub(crate) backend: Option<VoiceTranscriptionBackend>,
    pub(crate) use_gpu: bool,
    pub(crate) whisper_language: Option<String>,
}

pub(crate) type SharedVoiceRuntimeSettings = Arc<VoiceRuntimeSettings>;

pub(super) fn parse_voice_settings(
    document: &DocumentMut,
    warnings: &mut Vec<String>,
) -> VoiceSettings {
    let mut settings = VoiceSettings::default();
    let Some(voice) = document.get("voice") else {
        return settings;
    };

    if let Some(item) = voice.get("mode") {
        match item.as_str().and_then(VoiceMode::from_id) {
            Some(mode) => settings.mode = mode,
            None => warnings.push("voice.mode 无效，已关闭语音输入".to_owned()),
        }
    }
    match settings.normalized() {
        Ok(settings) => settings,
        Err(error) => {
            warnings.push(format!("语音配置无效，已关闭语音输入：{error}"));
            VoiceSettings::default()
        }
    }
}

pub(super) fn write_voice_settings(document: &mut DocumentMut, settings: &VoiceSettings) {
    ensure_table_like(&mut document["voice"]);
    set_item_value(
        &mut document["voice"]["mode"],
        Value::from(settings.mode.id()),
    );
    remove_key(document, "voice", "transcription_model");
    remove_key(document, "voice", "use_gpu");
    remove_key(document, "voice", "vad_model");
}
