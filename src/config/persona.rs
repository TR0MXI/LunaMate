//! 解析并写回 Agent crate 拥有的人格配置与删除 tombstone。

use std::{collections::HashSet, path::Path};

use lunamate_agent::config::{
    AppLanguage, DEFAULT_PERSONA_ID, LlmSettings, ModelKind, PersonaConfig, PersonaContextLimits,
    PersonaSettings, normalize_persona_id,
};
use rust_i18n::t;
use toml_edit::{Array, ArrayOfTables, DocumentMut, Item, Table, Value};

use super::{ConfigWriteError, ensure_table_like, remove_key, set_item_value};

pub(super) fn parse_persona_settings(
    document: &DocumentMut,
    warnings: &mut Vec<String>,
    language: AppLanguage,
) -> PersonaSettings {
    let Some(persona) = document.get("persona") else {
        return PersonaSettings::default_for(language);
    };
    let mut settings = PersonaSettings {
        personas: Vec::new(),
        selected: None,
        pending_deletions: parse_pending_deletions(persona, warnings, language),
    };
    if let Some(selected) = persona.get("selected") {
        match selected.as_str() {
            Some(selected) => settings.selected = Some(selected.to_owned()),
            None => warnings.push(
                t!(
                    "config.error.expected_string_ignored",
                    locale = language.id(),
                    field = "persona.selected"
                )
                .to_string(),
            ),
        }
    }
    match persona.get("list") {
        None => {}
        Some(list) => match list.as_array_of_tables() {
            Some(list) => {
                let mut ids = HashSet::with_capacity(list.len());
                for (index, table) in list.iter().enumerate() {
                    let entry = format!("persona.list[{index}]");
                    let config = match parse_persona(table, language).and_then(|config| {
                        config.normalized(language).map_err(ConfigWriteError::from)
                    }) {
                        Ok(config) => config,
                        Err(error) => {
                            warnings.push(
                                t!(
                                    "config.error.entry_ignored",
                                    locale = language.id(),
                                    entry = &entry,
                                    error = error
                                )
                                .to_string(),
                            );
                            continue;
                        }
                    };
                    if !ids.insert(config.id.clone()) {
                        warnings.push(
                            t!(
                                "config.error.entry_ignored",
                                locale = language.id(),
                                entry = &entry,
                                error = t!(
                                    "persona.error.duplicate_id",
                                    locale = language.id(),
                                    id = &config.id
                                )
                            )
                            .to_string(),
                        );
                        continue;
                    }
                    settings.personas.push(config);
                }
            }
            None => warnings.push(
                t!(
                    "config.error.expected_table_array_ignored",
                    locale = language.id(),
                    field = "persona.list"
                )
                .to_string(),
            ),
        },
    }
    if settings.personas.is_empty() {
        warnings.push(t!("persona.error.empty", locale = language.id()).to_string());
        let pending_deletions = settings
            .pending_deletions
            .into_iter()
            .filter(|id| {
                let keep = id != DEFAULT_PERSONA_ID;
                if !keep {
                    warnings.push(
                        t!(
                            "config.error.entry_ignored",
                            locale = language.id(),
                            entry = "persona.pending_deletions",
                            error = t!(
                                "persona.error.pending_deletion_conflict",
                                locale = language.id(),
                                id = DEFAULT_PERSONA_ID
                            )
                        )
                        .to_string(),
                    );
                }
                keep
            })
            .collect();
        return PersonaSettings {
            pending_deletions,
            ..PersonaSettings::default_for(language)
        };
    }
    settings.selected = settings
        .selected
        .map(|selected| selected.trim().to_owned())
        .filter(|selected| !selected.is_empty());
    if let Some(selected) = settings.selected.as_deref()
        && !settings
            .personas
            .iter()
            .any(|persona| persona.id == selected)
    {
        warnings.push(
            t!(
                "config.error.entry_ignored",
                locale = language.id(),
                entry = "persona.selected",
                error = t!(
                    "persona.error.missing_selected",
                    locale = language.id(),
                    id = selected
                )
            )
            .to_string(),
        );
        settings.selected = None;
    }
    let active_ids = settings
        .personas
        .iter()
        .map(|persona| persona.id.as_str())
        .collect::<HashSet<_>>();
    settings.pending_deletions.retain(|id| {
        let keep = !active_ids.contains(id.as_str());
        if !keep {
            warnings.push(
                t!(
                    "config.error.entry_ignored",
                    locale = language.id(),
                    entry = "persona.pending_deletions",
                    error = t!(
                        "persona.error.pending_deletion_conflict",
                        locale = language.id(),
                        id = id
                    )
                )
                .to_string(),
            );
        }
        keep
    });
    settings
}

/// 清除解析后无法解析为对应模型能力的人格绑定，避免把宽松读取结果交给严格快照。
pub(super) fn clear_invalid_model_bindings(
    llm: &LlmSettings,
    settings: &mut PersonaSettings,
    warnings: &mut Vec<String>,
    language: AppLanguage,
) {
    for persona in &mut settings.personas {
        let chat_binding_valid = persona
            .model
            .as_deref()
            .map(|id| {
                llm.model(id)
                    .is_some_and(|model| model.kind == ModelKind::ChatCompletions)
            })
            .unwrap_or(true);
        if !chat_binding_valid {
            warnings.push(
                t!(
                    "persona.error.model_binding_cleared",
                    locale = language.id(),
                    persona = &persona.id,
                    field = "persona.model"
                )
                .to_string(),
            );
            persona.model = None;
        }

        let tts_binding_valid = persona
            .tts_model
            .as_deref()
            .map(|id| {
                llm.model(id)
                    .is_some_and(|model| model.kind == ModelKind::SpeechSynthesis)
            })
            .unwrap_or(true);
        if !tts_binding_valid {
            warnings.push(
                t!(
                    "persona.error.model_binding_cleared",
                    locale = language.id(),
                    persona = &persona.id,
                    field = "persona.tts_model"
                )
                .to_string(),
            );
            persona.tts_model = None;
        }
    }
}

fn parse_pending_deletions(
    persona: &Item,
    warnings: &mut Vec<String>,
    language: AppLanguage,
) -> Vec<String> {
    let Some(pending) = persona.get("pending_deletions") else {
        return Vec::new();
    };
    let Some(pending) = pending.as_array() else {
        warnings.push(
            t!(
                "config.error.expected_array_ignored",
                locale = language.id(),
                field = "persona.pending_deletions"
            )
            .to_string(),
        );
        return Vec::new();
    };
    let mut result = Vec::with_capacity(pending.len());
    let mut seen = HashSet::with_capacity(pending.len());
    for (index, value) in pending.iter().enumerate() {
        let entry = format!("persona.pending_deletions[{index}]");
        let Some(id) = value.as_str() else {
            warnings.push(
                t!(
                    "config.error.expected_string_ignored",
                    locale = language.id(),
                    field = &entry
                )
                .to_string(),
            );
            continue;
        };
        match normalize_persona_id(id, language) {
            Ok(id) if seen.insert(id.clone()) => result.push(id),
            Ok(_) => {}
            Err(error) => warnings.push(
                t!(
                    "config.error.entry_ignored",
                    locale = language.id(),
                    entry = &entry,
                    error = error
                )
                .to_string(),
            ),
        }
    }
    result
}

fn parse_persona(table: &Table, language: AppLanguage) -> Result<PersonaConfig, ConfigWriteError> {
    let required = |key: &str| {
        table
            .get(key)
            .and_then(Item::as_str)
            .map(str::to_owned)
            .ok_or_else(|| {
                ConfigWriteError::InvalidValue(
                    t!(
                        "config.error.expected_string",
                        locale = language.id(),
                        field = key
                    )
                    .to_string(),
                )
            })
    };
    let integer = |key: &'static str| -> Result<Option<u32>, ConfigWriteError> {
        match table.get(key) {
            None => Ok(None),
            Some(item) => item
                .as_integer()
                .and_then(|value| u32::try_from(value).ok())
                .map(Some)
                .ok_or_else(|| {
                    ConfigWriteError::InvalidValue(
                        t!(
                            "config.error.expected_nonnegative_integer",
                            locale = language.id(),
                            field = key
                        )
                        .to_string(),
                    )
                }),
        }
    };
    Ok(PersonaConfig {
        id: required("id")?,
        name: required("name")?,
        system_prompt: table
            .get("system_prompt")
            .and_then(Item::as_str)
            .unwrap_or_default()
            .to_owned(),
        input_prompt: table
            .get("input_prompt")
            .and_then(Item::as_str)
            .unwrap_or_default()
            .to_owned(),
        model: table.get("model").and_then(Item::as_str).map(str::to_owned),
        tts_model: table
            .get("tts_model")
            .and_then(Item::as_str)
            .map(str::to_owned),
        live2d_model: table
            .get("live2d_model")
            .and_then(Item::as_str)
            .map(Path::new)
            .map(Path::to_path_buf),
        context: PersonaContextLimits {
            max_messages: integer("max_context_messages")?,
            max_tokens: integer("max_context_tokens")?,
        },
    })
}

pub(super) fn write_persona_settings(document: &mut DocumentMut, settings: &PersonaSettings) {
    ensure_table_like(&mut document["persona"]);
    if let Some(mut key) = document.as_table_mut().key_mut("persona") {
        key.fmt();
    }
    match &settings.selected {
        Some(selected) => set_item_value(
            &mut document["persona"]["selected"],
            Value::from(selected.clone()),
        ),
        None => remove_key(document, "persona", "selected"),
    }
    if settings.pending_deletions.is_empty() {
        remove_key(document, "persona", "pending_deletions");
    } else {
        let mut pending = Array::new();
        for id in &settings.pending_deletions {
            pending.push(id.as_str());
        }
        set_item_value(
            &mut document["persona"]["pending_deletions"],
            Value::Array(pending),
        );
    }
    let existing = document
        .get("persona")
        .and_then(|persona| persona.get("list"))
        .and_then(Item::as_array_of_tables)
        .map(|list| list.iter().cloned().collect::<Vec<_>>())
        .unwrap_or_default();
    let mut list = ArrayOfTables::new();
    for persona in &settings.personas {
        let mut table = existing
            .iter()
            .find(|table| table.get("id").and_then(Item::as_str) == Some(&persona.id))
            .cloned()
            .unwrap_or_else(Table::new);
        set_item_value(&mut table["id"], Value::from(persona.id.clone()));
        set_item_value(&mut table["name"], Value::from(persona.name.clone()));
        set_item_value(
            &mut table["system_prompt"],
            Value::from(persona.system_prompt.clone()),
        );
        write_optional(
            &mut table,
            "input_prompt",
            (!persona.input_prompt.is_empty()).then(|| Value::from(persona.input_prompt.clone())),
        );
        write_optional(&mut table, "model", persona.model.clone().map(Value::from));
        write_optional(
            &mut table,
            "tts_model",
            persona.tts_model.clone().map(Value::from),
        );
        write_optional(
            &mut table,
            "live2d_model",
            persona
                .live2d_model
                .as_ref()
                .map(|path| Value::from(path.to_string_lossy().into_owned())),
        );
        write_optional(
            &mut table,
            "max_context_messages",
            persona
                .context
                .max_messages
                .map(|value| Value::from(i64::from(value))),
        );
        write_optional(
            &mut table,
            "max_context_tokens",
            persona
                .context
                .max_tokens
                .map(|value| Value::from(i64::from(value))),
        );
        list.push(table);
    }
    document["persona"]["list"] = Item::ArrayOfTables(list);
}

fn write_optional(table: &mut Table, key: &str, value: Option<Value>) {
    match value {
        Some(value) => set_item_value(&mut table[key], value),
        None => {
            table.remove(key);
        }
    }
}
