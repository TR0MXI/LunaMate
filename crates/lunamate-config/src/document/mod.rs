//! 负责配置 TOML 的解析、精确修改与持久化门面。

mod io;

use std::path::{Component, Path, PathBuf};

use gpui_component::ThemeMode;
use rust_i18n::t;
use toml_edit::{DocumentMut, Item, Table, Value};

use super::{
    AppLanguage, AppearanceSettings, ConfigWindow, ConfigWriteError, FrameRate, LoadedConfig,
    LogLevel, LoggingSettings, ModelWindowSize, PersonaSettings, ThemePreset, WindowPosition,
    clear_invalid_model_bindings, parse_llm_settings, parse_model_resource_settings,
    parse_persona_settings, parse_shortcut_settings, parse_voice_settings,
};

pub use io::{
    default_config_path, document_for_update, prepare_config_file, read_config_file,
    replace_config_file, sync_config_file_parent, write_config_file,
};

fn parse_document(document: &DocumentMut) -> (LoadedConfig, Option<String>) {
    let mut loaded = LoadedConfig::default();
    let mut warnings = Vec::new();

    for section in [
        "render",
        "debug",
        "tools",
        "logging",
        "interaction",
        "window",
        "model",
        "appearance",
        "persona",
        "shortcuts",
    ] {
        let _ = table_like_section(document, section, &mut warnings);
    }

    if let Some(item) = nested_item(document, "render", "frame_rate") {
        loaded.frame_rate = match parse_frame_rate(document, item) {
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

    if let Some(item) = nested_item(document, "debug", "use_native_tray_menu") {
        match item.as_bool() {
            Some(enabled) => loaded.use_native_tray_menu = enabled,
            None => {
                warnings.push("debug.use_native_tray_menu 无效，已使用自定义托盘菜单".to_owned())
            }
        }
    }

    if let Some(item) = nested_item(document, "tools", "allow_agent_screenshot") {
        match item.as_bool() {
            Some(allowed) => loaded.allow_agent_screenshot = allowed,
            None => {
                warnings.push("tools.allow_agent_screenshot 无效，已保持 Agent 截屏关闭".to_owned())
            }
        }
    }

    if let Some(item) = nested_item(document, "tools", "allow_agent_outfit_change") {
        match item.as_bool() {
            Some(allowed) => loaded.allow_agent_outfit_change = allowed,
            None => {
                warnings.push("tools.allow_agent_outfit_change 无效，已允许 Agent 换装".to_owned())
            }
        }
    }

    loaded.logging = parse_logging_settings(document, &mut warnings);

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
    loaded.model_resources = parse_model_resource_settings(document, &mut warnings);

    loaded.appearance = parse_appearance(document, &mut warnings);
    let language = loaded.appearance.language;
    loaded.llm = parse_llm_settings(document, &mut warnings, language);
    if document
        .get("persona")
        .is_none_or(|persona| persona.as_table_like().is_some())
    {
        warn_invalid_persona_optional_fields(document, &mut warnings, language);
        loaded.persona = parse_persona_settings(document, &mut warnings, language);
    } else {
        loaded.persona = PersonaSettings::default_for(language);
    }
    clear_invalid_model_bindings(&loaded.llm, &mut loaded.persona, &mut warnings, language);
    loaded.shortcuts = parse_shortcut_settings(document, &mut warnings);
    loaded.voice = parse_voice_settings(document, &mut warnings);
    loaded.window_positions.desktop_pet =
        parse_window_position(document, ConfigWindow::DesktopPet, &mut warnings);
    loaded.window_positions.settings =
        parse_window_position(document, ConfigWindow::Settings, &mut warnings);

    let warning = (!warnings.is_empty()).then(|| {
        warnings.join(&rust_i18n::t!(
            "common.status_separator",
            locale = loaded.appearance.language.id()
        ))
    });
    (loaded, warning)
}

fn parse_frame_rate(document: &DocumentMut, item: &Item) -> Option<FrameRate> {
    match item.as_str() {
        Some(super::UNLIMITED_FRAME_RATE_NAME) => return Some(FrameRate::Unlimited),
        Some(super::FOLLOW_DISPLAY_FRAME_RATE_NAME) => return Some(FrameRate::FollowDisplay),
        Some(super::CUSTOM_FRAME_RATE_NAME) => {
            return nested_item(document, "render", super::CUSTOM_FRAME_RATE_KEY)
                .and_then(Item::as_integer)
                .and_then(|fps| u16::try_from(fps).ok())
                .and_then(|fps| FrameRate::custom(fps).ok());
        }
        Some(_) | None => {}
    }
    match item.as_integer() {
        Some(30) => Some(FrameRate::Fps30),
        Some(60) => Some(FrameRate::Fps60),
        Some(120) => Some(FrameRate::Fps120),
        Some(_) | None => None,
    }
}

fn parse_logging_settings(document: &DocumentMut, warnings: &mut Vec<String>) -> LoggingSettings {
    let mut settings = LoggingSettings::default();

    if let Some(item) = nested_item(document, "logging", "level") {
        match item.as_str().and_then(LogLevel::from_id) {
            Some(level) => settings.level = level,
            None => warnings.push("logging.level 无效，已使用 info".to_owned()),
        }
    }
    if let Some(item) = nested_item(document, "logging", "rotation") {
        match item.as_bool() {
            Some(rotation) => settings.rotation = rotation,
            None => warnings.push("logging.rotation 无效，已启用日志轮转".to_owned()),
        }
    }
    if let Some(item) = nested_item(document, "logging", "compression") {
        match item.as_bool() {
            Some(compression) => settings.compression = compression,
            None => warnings.push("logging.compression 无效，已启用日志压缩".to_owned()),
        }
    }
    if let Some(item) = nested_item(document, "logging", "max_size_mb") {
        let parsed = item
            .as_integer()
            .and_then(|value| u32::try_from(value).ok())
            .map(|max_size_mb| LoggingSettings {
                max_size_mb,
                ..settings
            })
            .and_then(|candidate| candidate.normalized().ok());
        match parsed {
            Some(candidate) => settings = candidate,
            None => warnings.push("logging.max_size_mb 无效，已使用 10 MiB".to_owned()),
        }
    }
    if let Some(item) = nested_item(document, "logging", "keep_files") {
        let parsed = item
            .as_integer()
            .and_then(|value| u32::try_from(value).ok())
            .map(|keep_files| LoggingSettings {
                keep_files,
                ..settings
            })
            .and_then(|candidate| candidate.normalized().ok());
        match parsed {
            Some(candidate) => settings = candidate,
            None => warnings.push("logging.keep_files 无效，已保留 10 项".to_owned()),
        }
    }

    settings
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
            Some(color) => {
                let mut candidate = settings.clone();
                candidate.custom.accent = color.to_owned();
                match candidate.normalized() {
                    Ok(candidate) => settings.custom.accent = candidate.custom.accent,
                    Err(error) => warnings.push(format!(
                        "appearance.custom_accent 无效，已使用默认值：{error}"
                    )),
                }
            }
            None => warnings.push("appearance.custom_accent 必须是字符串".to_owned()),
        }
    }
    if let Some(item) = appearance.get("custom_background") {
        match item.as_str() {
            Some(color) => {
                let mut candidate = settings.clone();
                candidate.custom.background = color.to_owned();
                match candidate.normalized() {
                    Ok(candidate) => settings.custom.background = candidate.custom.background,
                    Err(error) => warnings.push(format!(
                        "appearance.custom_background 无效，已使用默认值：{error}"
                    )),
                }
            }
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

    settings
}

fn warn_invalid_persona_optional_fields(
    document: &DocumentMut,
    warnings: &mut Vec<String>,
    language: AppLanguage,
) {
    let Some(personas) = document
        .get("persona")
        .and_then(|persona| persona.get("list"))
        .and_then(Item::as_array_of_tables)
    else {
        return;
    };
    for (index, persona) in personas.iter().enumerate() {
        for field in [
            "system_prompt",
            "input_prompt",
            "model",
            "tts_model",
            "live2d_model",
        ] {
            if persona
                .get(field)
                .is_some_and(|item| item.as_str().is_none())
            {
                warnings.push(
                    t!(
                        "config.error.expected_string_ignored",
                        locale = language.id(),
                        field = format!("persona.list[{index}].{field}")
                    )
                    .to_string(),
                );
            }
        }
    }
}

pub fn table_like_section<'a>(
    document: &'a DocumentMut,
    section: &str,
    warnings: &mut Vec<String>,
) -> Option<&'a Item> {
    match document.get(section) {
        None => None,
        Some(item) if item.as_table_like().is_some() => Some(item),
        Some(_) => {
            warnings.push(format!("{section} 必须是 TOML 表，已对该配置域使用默认值"));
            None
        }
    }
}

pub fn nested_item<'a>(document: &'a DocumentMut, table: &str, key: &str) -> Option<&'a Item> {
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

pub fn validate_relative_path(path: &Path) -> Result<PathBuf, ConfigWriteError> {
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

pub fn write_window_position(
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

pub fn write_appearance(document: &mut DocumentMut, settings: &AppearanceSettings) {
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

pub fn write_logging_settings(document: &mut DocumentMut, settings: &LoggingSettings) {
    ensure_table_like(&mut document["logging"]);
    set_item_value(
        &mut document["logging"]["level"],
        Value::from(settings.level.id()),
    );
    set_item_value(
        &mut document["logging"]["rotation"],
        Value::from(settings.rotation),
    );
    set_item_value(
        &mut document["logging"]["compression"],
        Value::from(settings.compression),
    );
    set_item_value(
        &mut document["logging"]["max_size_mb"],
        Value::from(i64::from(settings.max_size_mb)),
    );
    set_item_value(
        &mut document["logging"]["keep_files"],
        Value::from(i64::from(settings.keep_files)),
    );
}

pub fn ensure_table_like(item: &mut Item) {
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

pub fn set_item_value(item: &mut Item, mut next: Value) {
    if let Some(current) = item.as_value() {
        *next.decor_mut() = current.decor().clone();
    }
    *item = Item::Value(next);
}

pub fn remove_key(document: &mut DocumentMut, table: &str, key: &str) {
    let Some(table) = document.get_mut(table).and_then(Item::as_table_like_mut) else {
        return;
    };
    table.remove(key);
}
