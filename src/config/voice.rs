//! 定义本地语音输入模式、模型路径和推理设备偏好。

use std::{path::PathBuf, sync::Arc};

use toml_edit::{DocumentMut, Item, Value};

use super::{ConfigWriteError, ensure_table_like, remove_key, set_item_value};

const MAX_MODEL_PATH_BYTES: usize = 4 * 1024;

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

/// 一次性发布的本地语音推理配置。
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct VoiceSettings {
    pub(crate) mode: VoiceMode,
    pub(crate) whisper_model: Option<PathBuf>,
    pub(crate) vad_model: Option<PathBuf>,
    /// 允许 whisper.cpp 使用当前构建包含的 GPU 后端；初始化失败时运行时回退 CPU。
    pub(crate) use_gpu: bool,
}

pub(crate) type SharedVoiceSettings = Arc<VoiceSettings>;

impl VoiceSettings {
    /// 规范化可选路径并拒绝无法安全交给 C API 的值。
    pub(crate) fn normalized(mut self) -> Result<Self, ConfigWriteError> {
        self.whisper_model = normalize_model_path(self.whisper_model, "Whisper 模型")?;
        self.vad_model = normalize_model_path(self.vad_model, "Silero VAD 模型")?;
        Ok(self)
    }
}

fn normalize_model_path(
    path: Option<PathBuf>,
    field: &str,
) -> Result<Option<PathBuf>, ConfigWriteError> {
    let Some(path) = path else {
        return Ok(None);
    };
    let Some(path) = path.to_str() else {
        return Err(ConfigWriteError::InvalidValue(format!(
            "{field}路径必须是有效的 UTF-8"
        )));
    };
    let path = path.trim();
    if path.is_empty() {
        return Ok(None);
    }
    if path.len() > MAX_MODEL_PATH_BYTES {
        return Err(ConfigWriteError::InvalidValue(format!(
            "{field}路径不能超过 {MAX_MODEL_PATH_BYTES} 字节"
        )));
    }
    if path.contains('\0') {
        return Err(ConfigWriteError::InvalidValue(format!(
            "{field}路径不能包含 NUL 字符"
        )));
    }
    Ok(Some(PathBuf::from(path)))
}

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
    if let Some(path) = parse_path(voice.get("whisper_model"), "voice.whisper_model", warnings) {
        settings.whisper_model = Some(path);
    }
    if let Some(path) = parse_path(voice.get("vad_model"), "voice.vad_model", warnings) {
        settings.vad_model = Some(path);
    }
    if let Some(item) = voice.get("use_gpu") {
        match item.as_bool() {
            Some(use_gpu) => settings.use_gpu = use_gpu,
            None => warnings.push("voice.use_gpu 无效，已使用 CPU".to_owned()),
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

fn parse_path(item: Option<&Item>, field: &str, warnings: &mut Vec<String>) -> Option<PathBuf> {
    let item = item?;
    match item.as_str() {
        Some(path) => Some(PathBuf::from(path)),
        None => {
            warnings.push(format!("{field} 必须是字符串，已忽略"));
            None
        }
    }
}

pub(super) fn write_voice_settings(document: &mut DocumentMut, settings: &VoiceSettings) {
    ensure_table_like(&mut document["voice"]);
    set_item_value(
        &mut document["voice"]["mode"],
        Value::from(settings.mode.id()),
    );
    set_item_value(
        &mut document["voice"]["use_gpu"],
        Value::from(settings.use_gpu),
    );
    write_optional_path(document, "whisper_model", settings.whisper_model.as_ref());
    write_optional_path(document, "vad_model", settings.vad_model.as_ref());
}

fn write_optional_path(document: &mut DocumentMut, key: &str, path: Option<&PathBuf>) {
    match path {
        Some(path) => set_item_value(
            &mut document["voice"][key],
            Value::from(path.to_string_lossy().into_owned()),
        ),
        None => remove_key(document, "voice", key),
    }
}
