//! 扫描本地模型目录，并把同一模型的多个清单组织为服装变体。

use std::{
    collections::BTreeMap,
    error::Error,
    ffi::OsStr,
    fmt, fs, io,
    path::{Path, PathBuf},
};

const MODEL_FILE_SUFFIX: &str = ".model3.json";
pub(in crate::model) const MAX_DISCOVERY_DEPTH: usize = 16;

/// 确保模型根目录存在。
pub(crate) fn ensure_model_directory(root: &Path) -> io::Result<()> {
    fs::create_dir_all(root)
}

/// 表示一个可加载的 Live2D 模型清单；同一模型下的清单作为服装变体展示。
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ModelVariant {
    display_name: String,
    relative_path: PathBuf,
}

impl ModelVariant {
    /// 返回用于服装选择器展示的名称。
    pub(crate) fn display_name(&self) -> &str {
        &self.display_name
    }

    /// 返回相对于 `models/` 根目录的模型清单路径。
    pub(crate) fn relative_path(&self) -> &Path {
        &self.relative_path
    }
}

/// 表示模型目录中的一个模型及其全部服装变体。
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ModelFamily {
    display_name: String,
    variants: Vec<ModelVariant>,
}

impl ModelFamily {
    /// 返回模型列表中展示的稳定名称。
    pub(crate) fn display_name(&self) -> &str {
        &self.display_name
    }

    /// 返回该模型可切换的全部清单变体。
    pub(crate) fn variants(&self) -> &[ModelVariant] {
        &self.variants
    }

    /// 返回模型清单提供的服装变体数量。
    pub(crate) fn outfit_count(&self) -> usize {
        self.variants.len()
    }

    /// 返回当前家族是否包含指定相对清单路径。
    pub(crate) fn contains(&self, relative_path: &Path) -> bool {
        self.variants
            .iter()
            .any(|variant| variant.relative_path == relative_path)
    }
}

/// 保存模型目录、已发现模型家族和当前选择。
#[derive(Clone, Debug)]
pub(crate) struct ModelCatalog {
    root: PathBuf,
    families: Vec<ModelFamily>,
    selected: Option<PathBuf>,
    warning: Option<String>,
}

impl ModelCatalog {
    /// 扫描模型目录，并从统一配置恢复选择。
    ///
    /// 不存在的模型目录会被视为空目录，而不是启动错误。只有一个模型家族时会自动选择
    /// 已配置服装或第一个可用服装。
    ///
    /// # Errors
    ///
    /// 模型根目录存在但无法读取时返回错误；子目录错误只产生可恢复警告。
    pub(crate) fn load(
        root: PathBuf,
        configured_selection: Option<&Path>,
    ) -> Result<Self, ModelCatalogError> {
        let (families, warning) = discover_models(&root)?;
        let selected = choose_selection(&families, configured_selection);
        Ok(Self {
            root,
            families,
            selected,
            warning,
        })
    }

    /// 创建保留目录位置的空目录，用于向界面呈现根目录扫描错误。
    pub(crate) fn empty(root: PathBuf) -> Self {
        Self {
            root,
            families: Vec::new(),
            selected: None,
            warning: None,
        }
    }

    /// 返回已发现的模型家族。
    pub(crate) fn families(&self) -> &[ModelFamily] {
        &self.families
    }

    /// 返回模型家族与服装变体总数。
    pub(crate) fn counts(&self) -> (usize, usize) {
        (
            self.families.len(),
            self.families.iter().map(ModelFamily::outfit_count).sum(),
        )
    }

    /// 返回用于扫描模型的根目录。
    pub(crate) fn root(&self) -> &Path {
        &self.root
    }

    /// 返回不影响已发现模型使用的扫描诊断。
    pub(crate) fn warning(&self) -> Option<&str> {
        self.warning.as_deref()
    }

    /// 返回当前选中的模型家族。
    pub(crate) fn selected_family(&self) -> Option<&ModelFamily> {
        let selected = self.selected.as_deref()?;
        self.families
            .iter()
            .find(|family| family.contains(selected))
    }

    /// 返回当前选择模型清单的绝对路径。
    pub(crate) fn selected_model_path(&self) -> Option<PathBuf> {
        self.selected
            .as_ref()
            .map(|relative_path| self.root.join(relative_path))
    }

    /// 返回当前选择模型清单的相对路径。
    pub(crate) fn selected_relative_path(&self) -> Option<&Path> {
        self.selected.as_deref()
    }

    /// 选择一个模型家族，优先保留该家族中当前服装，否则使用首个服装。
    ///
    /// # Errors
    ///
    /// 索引不在当前扫描结果中，或目标家族没有服装清单时返回错误。
    pub(crate) fn select_family(&mut self, index: usize) -> Result<PathBuf, ModelCatalogError> {
        let family = self.families.get(index).ok_or_else(|| {
            ModelCatalogError::message(format!("模型索引不在当前扫描结果中：{index}"))
        })?;
        let selected = self
            .selected
            .as_deref()
            .filter(|selected| family.contains(selected))
            .map(Path::to_path_buf)
            .or_else(|| {
                family
                    .variants
                    .first()
                    .map(|variant| variant.relative_path.clone())
            })
            .ok_or_else(|| ModelCatalogError::message("模型没有可用服装清单"))?;
        self.selected = Some(selected.clone());
        Ok(self.root.join(selected))
    }

    /// 选择指定服装变体，并返回其绝对模型清单路径。
    ///
    /// # Errors
    ///
    /// 请求路径不属于当前扫描结果时返回错误。
    pub(crate) fn select_variant(
        &mut self,
        relative_path: &Path,
    ) -> Result<PathBuf, ModelCatalogError> {
        if !self
            .families
            .iter()
            .any(|family| family.contains(relative_path))
        {
            return Err(ModelCatalogError::message(format!(
                "模型不在当前目录扫描结果中：{}",
                relative_path.display()
            )));
        }

        self.selected = Some(relative_path.to_path_buf());
        Ok(self.root.join(relative_path))
    }
}

fn discover_models(root: &Path) -> Result<(Vec<ModelFamily>, Option<String>), ModelCatalogError> {
    let mut grouped = BTreeMap::<String, Vec<ModelVariant>>::new();
    let mut warning = None;
    let mut directories = vec![(root.to_path_buf(), 0_usize)];

    while let Some((directory, depth)) = directories.pop() {
        let entries = match fs::read_dir(&directory) {
            Ok(entries) => entries,
            Err(error) if error.kind() == io::ErrorKind::NotFound && directory == root => {
                return Ok((Vec::new(), None));
            }
            Err(source) if directory == root => {
                return Err(ModelCatalogError::io(
                    format!("无法扫描模型目录 {}", directory.display()),
                    source,
                ));
            }
            Err(source) => {
                append_warning(
                    &mut warning,
                    format!("跳过无法扫描的模型子目录 {}：{source}", directory.display()),
                );
                continue;
            }
        };

        for entry in entries {
            let entry = match entry {
                Ok(entry) => entry,
                Err(source) => {
                    append_warning(
                        &mut warning,
                        format!("跳过无法读取的模型目录项 {}：{source}", directory.display()),
                    );
                    continue;
                }
            };
            let path = entry.path();
            let file_type = match entry.file_type() {
                Ok(file_type) => file_type,
                Err(source) => {
                    append_warning(
                        &mut warning,
                        format!("跳过无法识别的模型目录项 {}：{source}", path.display()),
                    );
                    continue;
                }
            };

            if file_type.is_dir() {
                if depth < MAX_DISCOVERY_DEPTH {
                    directories.push((path, depth + 1));
                } else {
                    append_warning(
                        &mut warning,
                        format!(
                            "跳过超过最大扫描深度 {MAX_DISCOVERY_DEPTH} 的目录 {}",
                            path.display()
                        ),
                    );
                }
                continue;
            }
            if !file_type.is_file() || !is_model_manifest(&path) {
                continue;
            }

            let relative_path = match path.strip_prefix(root) {
                Ok(relative_path) => relative_path,
                Err(error) => {
                    append_warning(
                        &mut warning,
                        format!("跳过模型目录外路径 {}：{error}", path.display()),
                    );
                    continue;
                }
            };
            if relative_path.to_str().is_none() {
                append_warning(
                    &mut warning,
                    format!("跳过非 UTF-8 模型路径 {}", path.display()),
                );
                continue;
            }

            let family_name = model_family_name(relative_path);
            let variant_name = variant_display_name(&path, &family_name);
            grouped
                .entry(family_name.clone())
                .or_default()
                .push(ModelVariant {
                    display_name: variant_name,
                    relative_path: relative_path.to_path_buf(),
                });
        }
    }

    let families = grouped
        .into_iter()
        .map(|(display_name, mut variants)| {
            variants.sort_by(|left, right| left.relative_path.cmp(&right.relative_path));
            ModelFamily {
                display_name,
                variants,
            }
        })
        .collect();
    Ok((families, warning))
}

fn is_model_manifest(path: &Path) -> bool {
    path.file_name()
        .and_then(OsStr::to_str)
        .is_some_and(|name| name.ends_with(MODEL_FILE_SUFFIX))
}

fn model_family_name(relative_path: &Path) -> String {
    let mut components = relative_path.components();
    if let (Some(first), Some(_)) = (components.next(), components.next()) {
        return first.as_os_str().to_string_lossy().into_owned();
    }
    model_manifest_stem(relative_path).to_owned()
}

fn variant_display_name(path: &Path, family_name: &str) -> String {
    let stem = model_manifest_stem(path);
    let shortened = stem
        .strip_prefix(family_name)
        .map(|suffix| suffix.trim_start_matches(['_', '-', ' ']))
        .filter(|suffix| !suffix.is_empty());
    shortened.unwrap_or(stem).to_owned()
}

fn model_manifest_stem(path: &Path) -> &str {
    path.file_name()
        .and_then(OsStr::to_str)
        .and_then(|name| name.strip_suffix(MODEL_FILE_SUFFIX))
        .filter(|name| !name.is_empty())
        .unwrap_or("未命名模型")
}

fn choose_selection(
    families: &[ModelFamily],
    configured_selection: Option<&Path>,
) -> Option<PathBuf> {
    if let Some(configured_selection) = configured_selection
        && families
            .iter()
            .any(|family| family.contains(configured_selection))
    {
        return Some(configured_selection.to_path_buf());
    }

    let [family] = families else {
        return None;
    };
    family
        .variants
        .first()
        .map(|variant| variant.relative_path.clone())
}

fn append_warning(warning: &mut Option<String>, message: String) {
    match warning {
        Some(warning) => {
            warning.push('；');
            warning.push_str(&message);
        }
        None => *warning = Some(message),
    }
}

/// 描述模型目录扫描或选择阶段的错误。
#[derive(Debug)]
pub(crate) struct ModelCatalogError {
    message: String,
    source: Option<io::Error>,
}

impl ModelCatalogError {
    fn message(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            source: None,
        }
    }

    fn io(message: impl Into<String>, source: io::Error) -> Self {
        Self {
            message: message.into(),
            source: Some(source),
        }
    }
}

impl fmt::Display for ModelCatalogError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)?;
        if let Some(source) = &self.source {
            write!(formatter, "：{source}")?;
        }
        Ok(())
    }
}

impl Error for ModelCatalogError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        self.source
            .as_ref()
            .map(|source| source as &(dyn Error + 'static))
    }
}
