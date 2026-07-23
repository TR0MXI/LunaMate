//! 负责配置文件定位、TOML 解析、精确修改与原子写回。

use std::{
    env, fs,
    io::{self, Read as _},
    path::{Component, Path, PathBuf},
};

use gpui_component::ThemeMode;
use toml_edit::{DocumentMut, Item, Table, Value};

use crate::persistence::{AtomicReplaceOperation, atomic_replace};

use super::{
    AppLanguage, AppearanceSettings, ConfigWindow, ConfigWriteError, CustomThemeSettings,
    FrameRate, LoadedConfig, ModelWindowSize, ThemePreset, WindowPosition, parse_llm_settings,
};

const LEGACY_CONFIG_PATH: &str = "./config.toml";
const MAX_CONFIG_FILE_BYTES: u64 = 1024 * 1024;

/// 返回默认配置路径，并优先兼容工作目录中的旧配置。
pub(super) fn default_config_path() -> PathBuf {
    let legacy = PathBuf::from(LEGACY_CONFIG_PATH);
    if legacy.is_file() {
        return legacy;
    }

    #[cfg(target_os = "windows")]
    let directory = env::var_os("APPDATA").map(PathBuf::from);
    #[cfg(target_os = "macos")]
    let directory = env::var_os("HOME")
        .map(PathBuf::from)
        .map(|home| home.join("Library").join("Application Support"));
    #[cfg(all(not(target_os = "windows"), not(target_os = "macos")))]
    let directory = env::var_os("XDG_CONFIG_HOME")
        .filter(|directory| !directory.is_empty())
        .map(PathBuf::from)
        .or_else(|| env::var_os("HOME").map(|home| PathBuf::from(home).join(".config")));

    directory
        .map(|directory| directory.join("LunaMate").join("config.toml"))
        .unwrap_or(legacy)
}

/// 读取并解析完整配置，失败时返回默认值与可展示的启动诊断。
pub(super) fn read_config_file(path: &Path) -> (LoadedConfig, Option<String>) {
    match read_config_source(path) {
        Ok(Some(source)) => match source.parse::<DocumentMut>() {
            Ok(document) => parse_document(&document),
            Err(error) => (
                LoadedConfig::default(),
                Some(format!(
                    "配置文件 {} 无法解析，已使用默认值：{}",
                    path.display(),
                    error.message()
                )),
            ),
        },
        Ok(None) => (LoadedConfig::default(), None),
        Err(error) => (
            LoadedConfig::default(),
            Some(format!(
                "配置文件 {} 无法读取，已使用默认值：{error}",
                path.display()
            )),
        ),
    }
}

/// 读取用于精确更新的 TOML；损坏内容会在本次保存时重建。
pub(super) fn document_for_update(path: &Path) -> Result<DocumentMut, ConfigWriteError> {
    match read_config_source(path) {
        Ok(Some(source)) => match source.parse::<DocumentMut>() {
            Ok(document) => Ok(document),
            Err(error) => {
                eprintln!(
                    "配置文件 {} 已损坏，本次保存将重建有效 TOML：{}",
                    path.display(),
                    error.message()
                );
                Ok(DocumentMut::new())
            }
        },
        Ok(None) => Ok(DocumentMut::new()),
        Err(source) => Err(ConfigWriteError::Io {
            operation: "读取配置文件",
            path: path.to_path_buf(),
            source,
        }),
    }
}

fn read_config_source(path: &Path) -> io::Result<Option<String>> {
    let file = match fs::File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
    let metadata = file.metadata()?;
    if !metadata.file_type().is_file() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "配置路径不是普通文件",
        ));
    }
    if metadata.len() > MAX_CONFIG_FILE_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("配置文件超过 {MAX_CONFIG_FILE_BYTES} 字节上限"),
        ));
    }

    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    file.take(MAX_CONFIG_FILE_BYTES + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_CONFIG_FILE_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("配置文件超过 {MAX_CONFIG_FILE_BYTES} 字节上限"),
        ));
    }
    String::from_utf8(bytes)
        .map(Some)
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "配置文件不是有效的 UTF-8"))
}

/// 原子写回一份完整 TOML 文档，并把共享 helper 错误转换为配置错误。
pub(super) fn write_config_file(
    path: &Path,
    document: &DocumentMut,
    nonce: u64,
) -> Result<(), ConfigWriteError> {
    atomic_replace(path, document.to_string().as_bytes(), nonce).map_err(|error| {
        let (operation, path, source) = error.into_parts();
        let operation = match operation {
            AtomicReplaceOperation::CreateTemporary => "创建配置临时文件",
            #[cfg(unix)]
            AtomicReplaceOperation::SetPermissions => "设置配置临时文件权限",
            AtomicReplaceOperation::WriteTemporary => "写入配置临时文件",
            AtomicReplaceOperation::SyncTemporary => "同步配置临时文件",
            AtomicReplaceOperation::Replace => "提交配置文件",
            #[cfg(unix)]
            AtomicReplaceOperation::SyncParent => "同步配置目录",
        };
        ConfigWriteError::Io {
            operation,
            path,
            source,
        }
    })
}

fn parse_document(document: &DocumentMut) -> (LoadedConfig, Option<String>) {
    let mut loaded = LoadedConfig::default();
    let mut warnings = Vec::new();

    if let Some(item) = nested_item(document, "render", "frame_rate") {
        loaded.frame_rate = match parse_frame_rate(item) {
            Some(frame_rate) => frame_rate,
            None => {
                warnings.push(format!(
                    "render.frame_rate 无效，已使用默认值 {}",
                    FrameRate::default().display_name()
                ));
                FrameRate::default()
            }
        };
    }

    if let Some(item) = nested_item(document, "debug", "show_fps") {
        match item.as_bool() {
            Some(show) => loaded.show_fps = show,
            None => warnings.push("debug.show_fps 无效，已使用默认值".to_owned()),
        }
    }

    if let Some(item) = nested_item(document, "interaction", "eye_tracking") {
        match item.as_bool() {
            Some(enabled) => loaded.eye_tracking = enabled,
            None => warnings.push("interaction.eye_tracking 无效，已使用默认值".to_owned()),
        }
    }

    if let Some(item) = nested_item(document, "window", "remember_position") {
        match item.as_bool() {
            Some(remember) => loaded.remember_window_positions = remember,
            None => warnings.push("window.remember_position 无效，已使用默认值".to_owned()),
        }
    }

    if let Some(item) = nested_item(document, "window", "model_size") {
        loaded.model_window_size = match item.as_str().and_then(ModelWindowSize::from_id) {
            Some(size) => size,
            None => {
                warnings.push("window.model_size 无效，已使用自动尺寸".to_owned());
                ModelWindowSize::default()
            }
        };
    }

    if let Some(item) = nested_item(document, "model", "selected") {
        loaded.snapshot.selected_model =
            match item.as_str().map(Path::new).map(validate_relative_path) {
                Some(Ok(path)) => Some(path),
                Some(Err(error)) => {
                    warnings.push(error.to_string());
                    None
                }
                None => {
                    warnings.push("model.selected 必须是 UTF-8 相对路径，已忽略".to_owned());
                    None
                }
            };
    }

    loaded.appearance = parse_appearance(document, &mut warnings);
    loaded.llm = parse_llm_settings(document, &mut warnings);
    loaded.window_positions.desktop_pet =
        parse_window_position(document, ConfigWindow::DesktopPet, &mut warnings);
    loaded.window_positions.settings =
        parse_window_position(document, ConfigWindow::Settings, &mut warnings);

    let warning = (!warnings.is_empty()).then(|| warnings.join("；"));
    (loaded, warning)
}

fn parse_frame_rate(item: &Item) -> Option<FrameRate> {
    if item.as_str() == Some(super::UNLIMITED_FRAME_RATE_NAME) {
        return Some(FrameRate::Unlimited);
    }
    item.as_integer()
        .and_then(|fps| u16::try_from(fps).ok())
        .and_then(|fps| FrameRate::try_from(fps).ok())
}

fn parse_appearance(document: &DocumentMut, warnings: &mut Vec<String>) -> AppearanceSettings {
    let mut settings = AppearanceSettings::default();
    let Some(appearance) = document.get("appearance") else {
        return settings;
    };

    if let Some(item) = appearance.get("language") {
        match item.as_str().and_then(AppLanguage::from_id) {
            Some(language) => settings.language = language,
            None => warnings.push("appearance.language 无效，已使用简体中文".to_owned()),
        }
    }
    if let Some(item) = appearance.get("theme") {
        match item.as_str().and_then(ThemePreset::from_id) {
            Some(theme) => settings.theme = theme,
            None => warnings.push("appearance.theme 无效，已改为跟随系统".to_owned()),
        }
    }
    if let Some(item) = appearance.get("custom_accent") {
        match item.as_str() {
            Some(color) => settings.custom.accent = color.to_owned(),
            None => warnings.push("appearance.custom_accent 必须是字符串".to_owned()),
        }
    }
    if let Some(item) = appearance.get("custom_background") {
        match item.as_str() {
            Some(color) => settings.custom.background = color.to_owned(),
            None => warnings.push("appearance.custom_background 必须是字符串".to_owned()),
        }
    }
    if let Some(item) = appearance.get("custom_mode") {
        match item.as_str() {
            Some("light") => settings.custom.mode = ThemeMode::Light,
            Some("dark") => settings.custom.mode = ThemeMode::Dark,
            _ => warnings.push("appearance.custom_mode 必须是 light 或 dark".to_owned()),
        }
    }

    match settings.clone().normalized() {
        Ok(settings) => settings,
        Err(error) => {
            warnings.push(format!("自定义主题颜色无效，已使用默认颜色：{error}"));
            AppearanceSettings {
                custom: CustomThemeSettings::default(),
                ..settings
            }
        }
    }
}

pub(super) fn nested_item<'a>(
    document: &'a DocumentMut,
    table: &str,
    key: &str,
) -> Option<&'a Item> {
    document.get(table)?.get(key)
}

fn parse_window_position(
    document: &DocumentMut,
    window: ConfigWindow,
    warnings: &mut Vec<String>,
) -> Option<WindowPosition> {
    let window_table = document.get("window")?.get(window.table_name())?;
    let x = item_number(window_table.get("x")?);
    let y = item_number(window_table.get("y")?);
    match (x, y) {
        (Some(x), Some(y)) => WindowPosition::new(x as f32, y as f32).or_else(|| {
            warnings.push(format!(
                "window.{} 包含非有限坐标，已忽略",
                window.table_name()
            ));
            None
        }),
        _ => {
            warnings.push(format!("window.{} 坐标不完整，已忽略", window.table_name()));
            None
        }
    }
}

fn item_number(item: &Item) -> Option<f64> {
    item.as_float()
        .or_else(|| item.as_integer().map(|value| value as f64))
        .filter(|value| value.is_finite())
}

pub(super) fn validate_relative_path(path: &Path) -> Result<PathBuf, ConfigWriteError> {
    if path.as_os_str().is_empty()
        || path.to_str().is_none()
        || path
            .components()
            .any(|component| !matches!(component, Component::Normal(_)))
    {
        return Err(ConfigWriteError::InvalidValue(format!(
            "模型配置路径必须是安全的 UTF-8 相对路径：{}",
            path.display()
        )));
    }
    Ok(path.to_path_buf())
}

pub(super) fn write_window_position(
    document: &mut DocumentMut,
    window: ConfigWindow,
    position: Option<WindowPosition>,
) {
    let table_name = window.table_name();
    match position {
        Some(position) => {
            ensure_table_like(&mut document["window"]);
            ensure_table_like(&mut document["window"][table_name]);
            set_item_value(
                &mut document["window"][table_name]["x"],
                Value::from(f64::from(position.x)),
            );
            set_item_value(
                &mut document["window"][table_name]["y"],
                Value::from(f64::from(position.y)),
            );
        }
        None => remove_key(document, "window", table_name),
    }
}

pub(super) fn write_appearance(document: &mut DocumentMut, settings: &AppearanceSettings) {
    ensure_table_like(&mut document["appearance"]);
    set_item_value(
        &mut document["appearance"]["language"],
        Value::from(settings.language.id()),
    );
    set_item_value(
        &mut document["appearance"]["theme"],
        Value::from(settings.theme.id()),
    );
    set_item_value(
        &mut document["appearance"]["custom_accent"],
        Value::from(settings.custom.accent.clone()),
    );
    set_item_value(
        &mut document["appearance"]["custom_background"],
        Value::from(settings.custom.background.clone()),
    );
    set_item_value(
        &mut document["appearance"]["custom_mode"],
        Value::from(settings.custom.mode.name()),
    );
}

pub(super) fn ensure_table_like(item: &mut Item) {
    if item.is_table() {
        return;
    }
    if let Some(inline_table) = item.as_inline_table() {
        let mut table = Table::new();
        for (key, value) in inline_table.iter() {
            let mut value = value.clone();
            value.decor_mut().clear();
            table[key] = Item::Value(value);
        }
        *item = Item::Table(table);
    } else {
        *item = Item::Table(Table::new());
    }
}

pub(super) fn set_item_value(item: &mut Item, mut next: Value) {
    if let Some(current) = item.as_value() {
        *next.decor_mut() = current.decor().clone();
    }
    *item = Item::Value(next);
}

pub(super) fn remove_key(document: &mut DocumentMut, table: &str, key: &str) {
    let Some(table) = document.get_mut(table).and_then(Item::as_table_like_mut) else {
        return;
    };
    table.remove(key);
}
