//! 定义模型动作、表情与服装的稀疏显示名和分类覆盖。

use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
};

use toml_edit::{ArrayOfTables, DocumentMut, Item, Table, Value};

use super::{
    ConfigWriteError, ensure_table_like, remove_key, set_item_value, validate_relative_path,
};

const MAX_MODEL_RESOURCE_OVERRIDES: usize = 1_024;
const MAX_MODEL_RESOURCE_ID_BYTES: usize = 512;
pub const MAX_MODEL_RESOURCE_NAME_BYTES: usize = 256;
const MAX_MODEL_RESOURCE_SETTINGS_BYTES: usize = 768 * 1_024;

/// 可在 LunaMate 内覆盖显示名的模型资源类型。
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum ModelResourceKind {
    Variant,
    Motion,
    Expression,
}

impl ModelResourceKind {
    const fn id(self) -> &'static str {
        match self {
            Self::Variant => "variant",
            Self::Motion => "motion",
            Self::Expression => "expression",
        }
    }

    fn from_id(id: &str) -> Option<Self> {
        match id {
            "variant" => Some(Self::Variant),
            "motion" => Some(Self::Motion),
            "expression" => Some(Self::Expression),
            _ => None,
        }
    }
}

/// 根目录外部表达式在设置界面中的用途。
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum ModelExpressionCategory {
    #[default]
    Expression,
    Outfit,
}

/// 一个不依赖显示名的模型资源配置键。
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct ModelResourceKey {
    manifest: PathBuf,
    kind: ModelResourceKind,
    resource_id: String,
}

impl ModelResourceKey {
    pub fn new(
        manifest: impl Into<PathBuf>,
        kind: ModelResourceKind,
        resource_id: impl Into<String>,
    ) -> Self {
        Self {
            manifest: manifest.into(),
            kind,
            resource_id: resource_id.into(),
        }
    }

    fn normalized(mut self) -> Result<Self, ConfigWriteError> {
        self.manifest = validate_relative_path(&self.manifest)?;
        if self.resource_id.is_empty()
            || self.resource_id.len() > MAX_MODEL_RESOURCE_ID_BYTES
            || self.resource_id.chars().any(char::is_control)
        {
            return Err(invalid(format!(
                "模型资源 ID 必须为不含控制字符的非空文本，且不超过 {MAX_MODEL_RESOURCE_ID_BYTES} 字节"
            )));
        }
        Ok(self)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct ModelResourceOverride {
    name: Option<String>,
    expression_category: ModelExpressionCategory,
}

impl Default for ModelResourceOverride {
    fn default() -> Self {
        Self {
            name: None,
            expression_category: ModelExpressionCategory::Expression,
        }
    }
}

/// 一次性发布的全部模型资源名称与分类覆盖。
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ModelResourceSettings {
    entries: BTreeMap<ModelResourceKey, ModelResourceOverride>,
}

pub type SharedModelResourceSettings = std::sync::Arc<ModelResourceSettings>;

impl ModelResourceSettings {
    /// 返回资源的自定义显示名；未重命名时由调用方使用模型原始名称。
    pub fn name<'a>(&'a self, key: &ModelResourceKey) -> Option<&'a str> {
        self.entries
            .get(key)
            .and_then(|entry| entry.name.as_deref())
    }

    /// 返回根目录外部表达式的分类；专属目录或清单表情应由调用方忽略此覆盖。
    pub fn expression_category(&self, key: &ModelResourceKey) -> ModelExpressionCategory {
        self.entries
            .get(key)
            .map(|entry| entry.expression_category)
            .unwrap_or_default()
    }

    /// 返回带有指定显示名覆盖的新快照；`None` 会恢复模型原始名称。
    pub fn with_name(
        &self,
        key: ModelResourceKey,
        name: Option<&str>,
    ) -> Result<Self, ConfigWriteError> {
        let key = key.normalized()?;
        let name = name.map(normalized_name).transpose()?;
        let mut next = self.clone();
        next.entries.entry(key.clone()).or_default().name = name;
        next.remove_empty_entry(&key);
        next.ensure_limit()?;
        Ok(next)
    }

    /// 返回带有指定表达式用途的新快照。
    pub fn with_expression_category(
        &self,
        key: ModelResourceKey,
        category: ModelExpressionCategory,
    ) -> Result<Self, ConfigWriteError> {
        let key = key.normalized()?;
        if key.kind != ModelResourceKind::Expression {
            return Err(invalid("只有表达式资源可以切换到服装区域"));
        }
        let mut next = self.clone();
        next.entries
            .entry(key.clone())
            .or_default()
            .expression_category = category;
        next.remove_empty_entry(&key);
        next.ensure_limit()?;
        Ok(next)
    }

    fn remove_empty_entry(&mut self, key: &ModelResourceKey) {
        let should_remove = self.entries.get(key).is_some_and(|entry| {
            entry.name.is_none() && entry.expression_category == ModelExpressionCategory::Expression
        });
        if should_remove {
            self.entries.remove(key);
        }
    }

    fn ensure_limit(&self) -> Result<(), ConfigWriteError> {
        if self.entries.len() > MAX_MODEL_RESOURCE_OVERRIDES {
            return Err(invalid(format!(
                "模型资源覆盖最多允许 {MAX_MODEL_RESOURCE_OVERRIDES} 项"
            )));
        }
        if self.estimated_document_bytes() > MAX_MODEL_RESOURCE_SETTINGS_BYTES {
            return Err(invalid(format!(
                "模型资源覆盖预计写入大小不能超过 {MAX_MODEL_RESOURCE_SETTINGS_BYTES} 字节"
            )));
        }
        Ok(())
    }

    fn estimated_document_bytes(&self) -> usize {
        self.entries.iter().fold(0_usize, |total, (key, entry)| {
            total
                .saturating_add(key.manifest.to_string_lossy().len())
                .saturating_add(key.resource_id.len())
                .saturating_add(entry.name.as_deref().map_or(0, str::len))
                .saturating_add(128)
        })
    }

    #[cfg(test)]
    pub fn entry_count_for_test(&self) -> usize {
        self.entries.len()
    }
}

fn normalized_name(name: &str) -> Result<String, ConfigWriteError> {
    let name = name.trim();
    if name.is_empty() {
        return Err(invalid("模型资源显示名不能为空"));
    }
    if name.len() > MAX_MODEL_RESOURCE_NAME_BYTES {
        return Err(invalid(format!(
            "模型资源显示名不能超过 {MAX_MODEL_RESOURCE_NAME_BYTES} 字节"
        )));
    }
    if name.chars().any(char::is_control) {
        return Err(invalid("模型资源显示名不能包含控制字符"));
    }
    Ok(name.to_owned())
}

fn invalid(message: impl Into<String>) -> ConfigWriteError {
    ConfigWriteError::InvalidValue(message.into())
}

pub fn parse_model_resource_settings(
    document: &DocumentMut,
    warnings: &mut Vec<String>,
) -> ModelResourceSettings {
    let Some(resources) = document
        .get("model")
        .and_then(|model| model.get("resources"))
    else {
        return ModelResourceSettings::default();
    };
    let Some(resources) = resources.as_array_of_tables() else {
        warnings.push("model.resources 必须是 TOML 表数组，已忽略".to_owned());
        return ModelResourceSettings::default();
    };

    let mut settings = ModelResourceSettings::default();
    for (index, table) in resources.iter().enumerate() {
        let (key, entry) = match parse_resource_override(table) {
            Ok(parsed) => parsed,
            Err(error) => {
                warnings.push(format!("model.resources[{index}] 已忽略：{error}"));
                continue;
            }
        };
        if settings.entries.len() == MAX_MODEL_RESOURCE_OVERRIDES
            && !settings.entries.contains_key(&key)
        {
            warnings.push(format!(
                "model.resources 超过 {MAX_MODEL_RESOURCE_OVERRIDES} 项，其余配置已忽略"
            ));
            break;
        }
        let previous = settings.entries.insert(key.clone(), entry);
        let duplicate = previous.is_some();
        if settings.estimated_document_bytes() > MAX_MODEL_RESOURCE_SETTINGS_BYTES {
            match previous {
                Some(previous) => {
                    settings.entries.insert(key, previous);
                }
                None => {
                    settings.entries.remove(&key);
                }
            }
            warnings.push(format!(
                "model.resources 预计写入大小超过 {MAX_MODEL_RESOURCE_SETTINGS_BYTES} 字节，其余配置已忽略"
            ));
            break;
        }
        if duplicate {
            warnings.push(format!("model.resources[{index}] 覆盖了此前的重复资源配置"));
        }
    }
    settings
}

fn parse_resource_override(
    table: &Table,
) -> Result<(ModelResourceKey, ModelResourceOverride), ConfigWriteError> {
    let required = |key: &str| {
        table
            .get(key)
            .and_then(Item::as_str)
            .ok_or_else(|| invalid(format!("{key} 必须是字符串")))
    };
    let kind = ModelResourceKind::from_id(required("kind")?)
        .ok_or_else(|| invalid("kind 必须是 variant、motion 或 expression"))?;
    let key = ModelResourceKey::new(Path::new(required("manifest")?), kind, required("id")?)
        .normalized()?;
    let name = table
        .get("name")
        .map(|item| {
            item.as_str()
                .ok_or_else(|| invalid("name 必须是字符串"))
                .and_then(normalized_name)
        })
        .transpose()?;
    let expression_category = match table.get("category") {
        None => ModelExpressionCategory::Expression,
        Some(_) if kind != ModelResourceKind::Expression => {
            return Err(invalid("category 只能用于 expression 资源"));
        }
        Some(item) => match item.as_str() {
            Some("expression") => ModelExpressionCategory::Expression,
            Some("outfit") => ModelExpressionCategory::Outfit,
            _ => return Err(invalid("category 必须是 expression 或 outfit")),
        },
    };
    if name.is_none() && expression_category == ModelExpressionCategory::Expression {
        return Err(invalid("资源配置必须至少包含 name 或 outfit 分类"));
    }
    Ok((
        key,
        ModelResourceOverride {
            name,
            expression_category,
        },
    ))
}

pub fn write_model_resource_settings(document: &mut DocumentMut, settings: &ModelResourceSettings) {
    ensure_table_like(&mut document["model"]);
    if settings.entries.is_empty() {
        remove_key(document, "model", "resources");
        return;
    }

    let existing = document
        .get("model")
        .and_then(|model| model.get("resources"))
        .and_then(Item::as_array_of_tables)
        .map(|resources| resources.iter().cloned().collect::<Vec<_>>())
        .unwrap_or_default();
    let mut resources = ArrayOfTables::new();
    for (key, entry) in &settings.entries {
        let manifest = key.manifest.to_string_lossy();
        let mut table = existing
            .iter()
            .find(|table| {
                table.get("manifest").and_then(Item::as_str) == Some(manifest.as_ref())
                    && table.get("kind").and_then(Item::as_str) == Some(key.kind.id())
                    && table.get("id").and_then(Item::as_str) == Some(key.resource_id.as_str())
            })
            .cloned()
            .unwrap_or_else(Table::new);
        set_item_value(&mut table["manifest"], Value::from(manifest.into_owned()));
        set_item_value(&mut table["kind"], Value::from(key.kind.id()));
        set_item_value(&mut table["id"], Value::from(key.resource_id.clone()));
        match &entry.name {
            Some(name) => set_item_value(&mut table["name"], Value::from(name.clone())),
            None => {
                table.remove("name");
            }
        }
        if entry.expression_category == ModelExpressionCategory::Outfit {
            set_item_value(&mut table["category"], Value::from("outfit"));
        } else {
            table.remove("category");
        }
        resources.push(table);
    }
    document["model"]["resources"] = Item::ArrayOfTables(resources);
}
