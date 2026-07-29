//! 定义人格配置、删除 tombstone 与上下文限制。
//!
//! 人格是记忆的归属单位：人格 ID 同时作为会话文档键和 `agent_memory.agent_id`，
//! 因此列表必须始终至少保留一条，删除最后一条人格会让已有记忆失去可管理的入口。

use std::{
    collections::HashSet,
    path::{Path, PathBuf},
    sync::Arc,
};

use rust_i18n::t;
use toml_edit::{Array, ArrayOfTables, DocumentMut, Item, Table, Value};

use super::{
    ConfigWriteError, ensure_table_like, llm::MAX_ID_BYTES, llm::MAX_SYSTEM_PROMPT_BYTES,
    remove_key, set_item_value, validate_relative_path,
};

const MAX_NAME_BYTES: usize = 128;

/// 初始默认人格 ID。
pub(crate) const DEFAULT_PERSONA_ID: &str = "default";

/// 上下文条数与 token 上限的可接受区间。下界保证至少能容纳一轮完整对话。
pub(crate) const CONTEXT_MESSAGES_MIN: u32 = 2;
pub(crate) const CONTEXT_MESSAGES_MAX: u32 = 512;
pub(crate) const CONTEXT_TOKENS_MIN: u32 = 256;
pub(crate) const CONTEXT_TOKENS_MAX: u32 = 1_050_624;

/// 未显式设置时实际生效的上下文上限。
pub(crate) const DEFAULT_CONTEXT_MESSAGES: u32 = 64;
pub(crate) const DEFAULT_CONTEXT_TOKENS: u32 = 65_792;

/// 单个人格的短期上下文上限；`None` 表示沿用默认值。
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) struct PersonaContextLimits {
    pub(crate) max_messages: Option<u32>,
    pub(crate) max_tokens: Option<u32>,
}

impl PersonaContextLimits {
    /// 返回实际生效的消息条数上限。
    pub(crate) fn effective_messages(self) -> u32 {
        self.max_messages.unwrap_or(DEFAULT_CONTEXT_MESSAGES)
    }

    /// 返回实际生效的上下文 token 上限。
    pub(crate) fn effective_tokens(self) -> u32 {
        self.max_tokens.unwrap_or(DEFAULT_CONTEXT_TOKENS)
    }

    fn normalize(&mut self) -> Result<(), ConfigWriteError> {
        check_range(
            self.max_messages,
            CONTEXT_MESSAGES_MIN,
            CONTEXT_MESSAGES_MAX,
            t!("persona.context_messages").as_ref(),
        )?;
        check_range(
            self.max_tokens,
            CONTEXT_TOKENS_MIN,
            CONTEXT_TOKENS_MAX,
            t!("persona.context_tokens").as_ref(),
        )
    }
}

fn check_range(
    value: Option<u32>,
    min: u32,
    max: u32,
    field: &str,
) -> Result<(), ConfigWriteError> {
    match value {
        Some(value) if !(min..=max).contains(&value) => Err(invalid(
            t!(
                "llm.error.out_of_range",
                field = field,
                min = min,
                max = max
            )
            .to_string(),
        )),
        _ => Ok(()),
    }
}

/// 一个可切换的人格；模型绑定为空时分别回退到全局默认对话模型与 Live2D 模型。
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct PersonaConfig {
    pub(crate) id: String,
    pub(crate) name: String,
    pub(crate) system_prompt: String,
    /// 用户输入格式化模板；当前只持久化，尚未进入请求构造流程。
    pub(crate) input_prompt: String,
    pub(crate) model: Option<String>,
    /// 相对于 `models/` 根目录的 Live2D 清单路径。
    pub(crate) live2d_model: Option<PathBuf>,
    pub(crate) context: PersonaContextLimits,
}

impl PersonaConfig {
    /// 使用默认上下文限制创建一个未绑定供应商的人格。
    pub(crate) fn new(id: impl Into<String>, name: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            name: name.into(),
            system_prompt: String::new(),
            input_prompt: String::new(),
            model: None,
            live2d_model: None,
            context: PersonaContextLimits::default(),
        }
    }

    fn normalize(&mut self) -> Result<(), ConfigWriteError> {
        self.id = normalized_id(&self.id)?;
        self.name = normalized_required(&self.name, t!("persona.name").as_ref(), MAX_NAME_BYTES)?;
        if self.system_prompt.len() > MAX_SYSTEM_PROMPT_BYTES {
            return Err(invalid(
                t!(
                    "llm.error.system_prompt_too_long",
                    max = MAX_SYSTEM_PROMPT_BYTES
                )
                .to_string(),
            ));
        }
        if self.input_prompt.len() > MAX_SYSTEM_PROMPT_BYTES {
            return Err(invalid(
                t!(
                    "llm.error.too_long",
                    field = t!("persona.input_prompt"),
                    max = MAX_SYSTEM_PROMPT_BYTES
                )
                .to_string(),
            ));
        }
        self.model = self
            .model
            .as_deref()
            .map(str::trim)
            .filter(|model| !model.is_empty())
            .map(str::to_owned);
        if let Some(model) = &self.model
            && model.len() > MAX_ID_BYTES
        {
            return Err(invalid(
                t!(
                    "llm.error.too_long",
                    field = t!("persona.provider"),
                    max = MAX_ID_BYTES
                )
                .to_string(),
            ));
        }
        self.live2d_model = self
            .live2d_model
            .as_deref()
            .map(validate_relative_path)
            .transpose()?;
        self.context.normalize()
    }
}

/// 一次性发布的人格目录与当前人格。
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct PersonaSettings {
    pub(crate) personas: Vec<PersonaConfig>,
    pub(crate) selected: Option<String>,
    /// 已从人格列表移除、但数据库记忆尚未确认清理完成的 ID。
    pub(crate) pending_deletions: Vec<String>,
}

impl Default for PersonaSettings {
    fn default() -> Self {
        Self {
            personas: vec![PersonaConfig::new(
                DEFAULT_PERSONA_ID,
                t!("persona.default_name").to_string(),
            )],
            selected: Some(DEFAULT_PERSONA_ID.to_owned()),
            pending_deletions: Vec::new(),
        }
    }
}

impl PersonaSettings {
    /// 返回当前人格；选择缺失时回退到第一条，因此列表非空即总能返回。
    pub(crate) fn active(&self) -> Option<&PersonaConfig> {
        self.selected
            .as_deref()
            .and_then(|selected| self.personas.iter().find(|persona| persona.id == selected))
            .or_else(|| self.personas.first())
    }

    /// 规范化并校验准备发布的完整人格配置。
    pub(crate) fn normalized(mut self) -> Result<Self, ConfigWriteError> {
        if self.personas.is_empty() {
            return Err(invalid(t!("persona.error.empty").to_string()));
        }
        let mut ids = HashSet::with_capacity(self.personas.len());
        for persona in &mut self.personas {
            persona.normalize()?;
            if !ids.insert(persona.id.clone()) {
                return Err(invalid(
                    t!("persona.error.duplicate_id", id = &persona.id).to_string(),
                ));
            }
        }

        self.selected = self
            .selected
            .as_deref()
            .map(str::trim)
            .filter(|selected| !selected.is_empty())
            .map(str::to_owned);
        if let Some(selected) = &self.selected
            && !ids.contains(selected)
        {
            return Err(invalid(
                t!("persona.error.missing_selected", id = selected).to_string(),
            ));
        }

        let mut pending = HashSet::with_capacity(self.pending_deletions.len());
        for id in std::mem::take(&mut self.pending_deletions) {
            let id = normalized_id(&id)?;
            if ids.contains(&id) {
                return Err(invalid(format!("待清理人格 ID 与现有人格冲突：{id}")));
            }
            if pending.insert(id.clone()) {
                self.pending_deletions.push(id);
            }
        }
        Ok(self)
    }
}

/// 为跨线程任务共享当前人格配置提供清晰的所有权类型。
pub(crate) type SharedPersonaSettings = Arc<PersonaSettings>;

fn normalized_id(id: &str) -> Result<String, ConfigWriteError> {
    let id = normalized_required(id, t!("persona.id").as_ref(), MAX_ID_BYTES)?;
    // ID 会直接进入数据库文档键与 agent_memory.agent_id，必须限制为安全字符集。
    if !id
        .bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
    {
        return Err(invalid(t!("llm.error.id_characters").to_string()));
    }
    Ok(id)
}

fn normalized_required(
    value: &str,
    field: &str,
    max_bytes: usize,
) -> Result<String, ConfigWriteError> {
    let value = value.trim();
    if value.is_empty() {
        return Err(invalid(t!("llm.error.required", field = field).to_string()));
    }
    if value.len() > max_bytes {
        return Err(invalid(
            t!("llm.error.too_long", field = field, max = max_bytes).to_string(),
        ));
    }
    Ok(value.to_owned())
}

fn invalid(message: impl Into<String>) -> ConfigWriteError {
    ConfigWriteError::InvalidValue(message.into())
}

pub(super) fn parse_persona_settings(
    document: &DocumentMut,
    warnings: &mut Vec<String>,
) -> PersonaSettings {
    let Some(persona) = document.get("persona") else {
        return PersonaSettings::default();
    };

    let mut settings = PersonaSettings {
        personas: Vec::new(),
        selected: None,
        pending_deletions: parse_pending_deletions(persona, warnings),
    };
    if let Some(selected) = persona.get("selected") {
        match selected.as_str() {
            Some(selected) => settings.selected = Some(selected.to_owned()),
            None => warnings.push("persona.selected 必须是字符串，已忽略".to_owned()),
        }
    }
    match persona.get("list") {
        None => {}
        Some(list) => match list.as_array_of_tables() {
            Some(list) => {
                let mut ids = HashSet::with_capacity(list.len());
                for (index, table) in list.iter().enumerate() {
                    // 逐条跳过无效人格，避免一处手写错误丢弃其余人格与其绑定的记忆入口。
                    let mut config = match parse_persona(table) {
                        Ok(config) => config,
                        Err(error) => {
                            warnings.push(format!("persona.list[{index}] 已忽略：{error}"));
                            continue;
                        }
                    };
                    if let Err(error) = config.normalize() {
                        warnings.push(format!("persona.list[{index}] 已忽略：{error}"));
                        continue;
                    }
                    if !ids.insert(config.id.clone()) {
                        warnings.push(format!(
                            "persona.list[{index}] 已忽略：{}",
                            t!("persona.error.duplicate_id", id = &config.id)
                        ));
                        continue;
                    }
                    settings.personas.push(config);
                }
            }
            None => warnings.push("persona.list 必须是 TOML 表数组，已忽略".to_owned()),
        },
    }

    if settings.personas.is_empty() {
        warnings.push(t!("persona.error.empty").to_string());
        let pending_deletions = settings
            .pending_deletions
            .into_iter()
            .filter(|id| {
                let keep = id != DEFAULT_PERSONA_ID;
                if !keep {
                    warnings.push("persona.pending_deletions 与默认人格冲突，已忽略".to_owned());
                }
                keep
            })
            .collect();
        return PersonaSettings {
            pending_deletions,
            ..PersonaSettings::default()
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
        warnings.push(format!(
            "persona.selected 指向不存在的人格 {selected}，已忽略"
        ));
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
            warnings.push(format!(
                "persona.pending_deletions 与现有人格 {id} 冲突，已忽略"
            ));
        }
        keep
    });
    settings
}

fn parse_pending_deletions(persona: &Item, warnings: &mut Vec<String>) -> Vec<String> {
    let Some(pending) = persona.get("pending_deletions") else {
        return Vec::new();
    };
    let Some(pending) = pending.as_array() else {
        warnings.push("persona.pending_deletions 必须是数组，已忽略".to_owned());
        return Vec::new();
    };
    let mut result = Vec::with_capacity(pending.len());
    let mut seen = HashSet::with_capacity(pending.len());
    for (index, value) in pending.iter().enumerate() {
        let Some(id) = value.as_str() else {
            warnings.push(format!(
                "persona.pending_deletions[{index}] 必须是字符串，已忽略"
            ));
            continue;
        };
        match normalized_id(id) {
            Ok(id) if seen.insert(id.clone()) => result.push(id),
            Ok(_) => {}
            Err(error) => warnings.push(format!(
                "persona.pending_deletions[{index}] 已忽略：{error}"
            )),
        }
    }
    result
}

fn parse_persona(table: &Table) -> Result<PersonaConfig, ConfigWriteError> {
    let required = |key: &str| {
        table
            .get(key)
            .and_then(Item::as_str)
            .map(str::to_owned)
            .ok_or_else(|| ConfigWriteError::InvalidValue(format!("{key} 必须是字符串")))
    };
    let integer = |key: &'static str| -> Result<Option<u32>, ConfigWriteError> {
        match table.get(key) {
            None => Ok(None),
            Some(item) => item
                .as_integer()
                .and_then(|value| u32::try_from(value).ok())
                .map(Some)
                .ok_or_else(|| ConfigWriteError::InvalidValue(format!("{key} 必须是非负整数"))),
        }
    };

    let max_messages = integer("max_context_messages")?;
    let max_tokens = integer("max_context_tokens")?;
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
        live2d_model: table
            .get("live2d_model")
            .and_then(Item::as_str)
            .map(Path::new)
            .map(Path::to_path_buf),
        context: PersonaContextLimits {
            max_messages,
            max_tokens,
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
