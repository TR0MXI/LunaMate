//! 解析并写回 Agent crate 拥有的语言模型配置。

use std::{collections::HashSet, path::PathBuf};

#[cfg(test)]
use lunamate_agent::config::LlmProvider;
use lunamate_agent::config::{
    AppLanguage, LlmAdvancedOptions, LlmModelConfig, LlmSettings, ModelKind, ModelProvider,
    reasoning_budget, reasoning_effort_from_id, reasoning_effort_id,
};
use rust_i18n::t;
use toml_edit::{ArrayOfTables, DocumentMut, Item, Table, Value};

use super::{ConfigWriteError, ensure_table_like, remove_key, set_item_value, table_like_section};

const MAX_MODELS: usize = 64;

#[cfg(test)]
pub(in crate::config) fn normalize_endpoint(
    provider: LlmProvider,
    endpoint: Option<&str>,
    language: AppLanguage,
) -> Result<Option<String>, ConfigWriteError> {
    lunamate_agent::config::normalize_endpoint(provider, endpoint, language).map_err(Into::into)
}

pub(super) fn parse_llm_settings(
    document: &DocumentMut,
    warnings: &mut Vec<String>,
    language: AppLanguage,
) -> LlmSettings {
    let mut settings = LlmSettings::default();
    let Some(llm) = table_like_section(document, "llm", warnings) else {
        return settings;
    };
    if let Some(selected) = llm.get("selected") {
        match selected.as_str() {
            Some(selected) => settings.selected_model = Some(selected.to_owned()),
            None => warnings.push(
                t!(
                    "config.error.expected_string_ignored",
                    locale = language.id(),
                    field = "llm.selected"
                )
                .to_string(),
            ),
        }
    }
    if let Some(selected) = llm.get("selected_transcription") {
        match selected.as_str() {
            Some(selected) => settings.selected_transcription_model = Some(selected.to_owned()),
            None => warnings.push(
                t!(
                    "config.error.expected_string_ignored",
                    locale = language.id(),
                    field = "llm.selected_transcription"
                )
                .to_string(),
            ),
        }
    }
    if let Some(models) = llm.get("models") {
        match models.as_array_of_tables() {
            Some(models) => {
                let mut ids = HashSet::with_capacity(models.len());
                for (index, table) in models.iter().enumerate() {
                    let entry = format!("llm.models[{index}]");
                    let model =
                        match parse_llm_model(table, &entry, warnings, language).and_then(|model| {
                            model.normalized(language).map_err(ConfigWriteError::from)
                        }) {
                            Ok(model) => model,
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
                    if !ids.insert(model.id.clone()) {
                        warnings.push(
                            t!(
                                "config.error.entry_ignored",
                                locale = language.id(),
                                entry = &entry,
                                error = t!(
                                    "llm.error.duplicate_id",
                                    locale = language.id(),
                                    id = &model.id
                                )
                            )
                            .to_string(),
                        );
                        continue;
                    }
                    if settings.models.len() == MAX_MODELS {
                        warnings.push(
                            t!(
                                "llm.error.max_models",
                                locale = language.id(),
                                max = MAX_MODELS
                            )
                            .to_string(),
                        );
                        break;
                    }
                    settings.models.push(model);
                }
            }
            None => warnings.push(
                t!(
                    "config.error.expected_table_array_ignored",
                    locale = language.id(),
                    field = "llm.models"
                )
                .to_string(),
            ),
        }
    }
    settings.selected_model = settings
        .selected_model
        .map(|selected| selected.trim().to_owned())
        .filter(|selected| !selected.is_empty());
    settings.selected_transcription_model = settings
        .selected_transcription_model
        .map(|selected| selected.trim().to_owned())
        .filter(|selected| !selected.is_empty());
    if let Some(selected) = settings.selected_model.as_deref()
        && !settings
            .models
            .iter()
            .any(|model| model.id == selected && model.kind == ModelKind::ChatCompletions)
    {
        warnings.push(
            t!(
                "config.error.entry_ignored",
                locale = language.id(),
                entry = "llm.selected",
                error = t!(
                    "llm.error.missing_selected",
                    locale = language.id(),
                    id = selected
                )
            )
            .to_string(),
        );
        settings.selected_model = None;
    }
    if let Some(selected) = settings.selected_transcription_model.as_deref()
        && !settings
            .models
            .iter()
            .any(|model| model.id == selected && model.kind == ModelKind::Transcription)
    {
        warnings.push(
            t!(
                "config.error.entry_ignored",
                locale = language.id(),
                entry = "llm.selected_transcription",
                error = t!(
                    "llm.error.missing_selected_transcription",
                    locale = language.id(),
                    id = selected
                )
            )
            .to_string(),
        );
        settings.selected_transcription_model = None;
    }
    settings
}

fn parse_llm_model(
    table: &Table,
    entry: &str,
    warnings: &mut Vec<String>,
    language: AppLanguage,
) -> Result<LlmModelConfig, ConfigWriteError> {
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
    let kind_id = required("kind")?;
    let kind = ModelKind::from_id(&kind_id).ok_or_else(|| {
        ConfigWriteError::InvalidValue(
            t!(
                "llm.error.unknown_kind",
                locale = language.id(),
                kind = kind_id
            )
            .to_string(),
        )
    })?;
    let provider_id = required("provider")?;
    let provider = ModelProvider::from_id(&provider_id).ok_or_else(|| {
        ConfigWriteError::InvalidValue(
            t!(
                "llm.error.unknown_provider",
                locale = language.id(),
                provider = &provider_id
            )
            .to_string(),
        )
    })?;
    let use_gpu = match table.get("use_gpu") {
        None => false,
        Some(item) => match item.as_bool() {
            Some(use_gpu) => use_gpu,
            None => {
                warnings.push(
                    t!(
                        "config.error.expected_boolean",
                        locale = language.id(),
                        field = format!("{entry}.use_gpu")
                    )
                    .to_string(),
                );
                false
            }
        },
    };
    Ok(LlmModelConfig {
        id: required("id")?,
        label: required("label")?,
        kind,
        provider,
        model: optional_string(table, "model", entry, warnings, language).unwrap_or_default(),
        endpoint: optional_string(table, "endpoint", entry, warnings, language),
        api_key: optional_string(table, "api_key", entry, warnings, language),
        voice: optional_string(table, "voice", entry, warnings, language),
        voice_type: optional_string(table, "voice_type", entry, warnings, language),
        local_path: optional_string(table, "local_path", entry, warnings, language)
            .map(PathBuf::from),
        use_gpu,
        whisper_language: optional_string(table, "whisper_language", entry, warnings, language),
        advanced: parse_advanced_options(table, entry, warnings, language)?,
    })
}

fn optional_string(
    table: &Table,
    key: &str,
    entry: &str,
    warnings: &mut Vec<String>,
    language: AppLanguage,
) -> Option<String> {
    match table.get(key) {
        None => None,
        Some(item) => match item.as_str() {
            Some(value) => Some(value.to_owned()),
            None => {
                warnings.push(
                    t!(
                        "config.error.expected_string_ignored",
                        locale = language.id(),
                        field = format!("{entry}.{key}")
                    )
                    .to_string(),
                );
                None
            }
        },
    }
}

fn parse_advanced_options(
    table: &Table,
    entry: &str,
    warnings: &mut Vec<String>,
    language: AppLanguage,
) -> Result<LlmAdvancedOptions, ConfigWriteError> {
    let budget = optional_integer(table, "reasoning_budget", entry, warnings, language);
    let reasoning_effort = match table.get("reasoning_effort") {
        None => None,
        Some(item) => match item.as_str() {
            None => {
                warnings.push(
                    t!(
                        "config.error.expected_string_ignored",
                        locale = language.id(),
                        field = format!("{entry}.reasoning_effort")
                    )
                    .to_string(),
                );
                None
            }
            Some(id) => Some(reasoning_effort_from_id(id, budget).ok_or_else(|| {
                ConfigWriteError::InvalidValue(
                    t!(
                        "llm.error.unknown_reasoning_effort",
                        locale = language.id(),
                        effort = id
                    )
                    .to_string(),
                )
            })?),
        },
    };
    Ok(LlmAdvancedOptions {
        context_window_tokens: optional_integer(
            table,
            "context_window_tokens",
            entry,
            warnings,
            language,
        ),
        reasoning_effort,
        max_output_tokens: optional_integer(table, "max_output_tokens", entry, warnings, language),
        temperature: optional_ratio(table, "temperature", entry, warnings, language),
        top_p: optional_ratio(table, "top_p", entry, warnings, language),
    })
}

fn optional_integer(
    table: &Table,
    key: &str,
    entry: &str,
    warnings: &mut Vec<String>,
    language: AppLanguage,
) -> Option<u32> {
    match table
        .get(key)?
        .as_integer()
        .and_then(|value| u32::try_from(value).ok())
    {
        Some(value) => Some(value),
        None => {
            warnings.push(
                t!(
                    "config.error.expected_nonnegative_integer",
                    locale = language.id(),
                    field = format!("{entry}.{key}")
                )
                .to_string(),
            );
            None
        }
    }
}

fn optional_ratio(
    table: &Table,
    key: &str,
    entry: &str,
    warnings: &mut Vec<String>,
    language: AppLanguage,
) -> Option<f64> {
    let item = table.get(key)?;
    match item
        .as_float()
        .or_else(|| item.as_integer().map(|value| value as f64))
    {
        Some(value) => Some(value),
        None => {
            warnings.push(
                t!(
                    "config.error.expected_number",
                    locale = language.id(),
                    field = format!("{entry}.{key}")
                )
                .to_string(),
            );
            None
        }
    }
}

pub(super) fn write_llm_settings(document: &mut DocumentMut, settings: &LlmSettings) {
    ensure_table_like(&mut document["llm"]);
    if let Some(mut key) = document.as_table_mut().key_mut("llm") {
        key.fmt();
    }
    match &settings.selected_model {
        Some(selected) => set_item_value(
            &mut document["llm"]["selected"],
            Value::from(selected.clone()),
        ),
        None => remove_key(document, "llm", "selected"),
    }
    match &settings.selected_transcription_model {
        Some(selected) => set_item_value(
            &mut document["llm"]["selected_transcription"],
            Value::from(selected.clone()),
        ),
        None => remove_key(document, "llm", "selected_transcription"),
    }
    let existing_models = document
        .get("llm")
        .and_then(|llm| llm.get("models"))
        .and_then(Item::as_array_of_tables)
        .map(|models| models.iter().cloned().collect::<Vec<_>>())
        .unwrap_or_default();
    let mut models = ArrayOfTables::new();
    for model in &settings.models {
        let mut table = existing_models
            .iter()
            .find(|table| table.get("id").and_then(Item::as_str) == Some(&model.id))
            .cloned()
            .unwrap_or_else(Table::new);
        set_item_value(&mut table["id"], Value::from(model.id.clone()));
        set_item_value(&mut table["label"], Value::from(model.label.clone()));
        set_item_value(&mut table["kind"], Value::from(model.kind.id()));
        set_item_value(&mut table["provider"], Value::from(model.provider.id()));
        set_item_value(&mut table["model"], Value::from(model.model.clone()));
        write_optional(
            &mut table,
            "endpoint",
            model.endpoint.clone().map(Value::from),
        );
        write_optional(
            &mut table,
            "api_key",
            model.api_key.clone().map(Value::from),
        );
        write_optional(&mut table, "voice", model.voice.clone().map(Value::from));
        write_optional(
            &mut table,
            "voice_type",
            model.voice_type.clone().map(Value::from),
        );
        write_optional(
            &mut table,
            "local_path",
            model
                .local_path
                .as_ref()
                .map(|path| Value::from(path.to_string_lossy().into_owned())),
        );
        write_optional(
            &mut table,
            "use_gpu",
            (model.provider == ModelProvider::LocalWhisper).then_some(Value::from(model.use_gpu)),
        );
        write_optional(
            &mut table,
            "whisper_language",
            model.whisper_language.clone().map(Value::from),
        );
        write_advanced_options(&mut table, &model.advanced);
        models.push(table);
    }
    document["llm"]["models"] = Item::ArrayOfTables(models);
}

fn write_advanced_options(table: &mut Table, advanced: &LlmAdvancedOptions) {
    write_optional(
        table,
        "context_window_tokens",
        advanced
            .context_window_tokens
            .map(|tokens| Value::from(i64::from(tokens))),
    );
    write_optional(
        table,
        "reasoning_effort",
        advanced
            .reasoning_effort
            .as_ref()
            .map(|effort| Value::from(reasoning_effort_id(effort))),
    );
    write_optional(
        table,
        "reasoning_budget",
        advanced
            .reasoning_effort
            .as_ref()
            .and_then(reasoning_budget)
            .map(|tokens| Value::from(i64::from(tokens))),
    );
    write_optional(
        table,
        "max_output_tokens",
        advanced
            .max_output_tokens
            .map(|tokens| Value::from(i64::from(tokens))),
    );
    write_optional(table, "temperature", advanced.temperature.map(Value::from));
    write_optional(table, "top_p", advanced.top_p.map(Value::from));
}

fn write_optional(table: &mut Table, key: &str, value: Option<Value>) {
    match value {
        Some(value) => set_item_value(&mut table[key], value),
        None => {
            table.remove(key);
        }
    }
}
