//! 扫描本地模型目录，并把同一模型的多个清单组织为服装变体。

use std::{
    collections::{BTreeMap, BTreeSet},
    error::Error,
    ffi::OsStr,
    fmt, fs, io,
    path::{Path, PathBuf},
};

use crate::capabilities::ModelResourceResolver;

const MODEL_FILE_SUFFIX: &str = ".model3.json";
const MAX_DISCOVERY_DEPTH: usize = 16;

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

/// 表示与模型清单同目录、由外部表达式提供的服装预设。
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ModelOutfit {
    expression_name: String,
    relative_path: PathBuf,
}

impl ModelOutfit {
    /// 返回用于服装选择器展示的名称。
    pub(crate) fn display_name(&self) -> &str {
        &self.expression_name
    }

    /// 返回表达式控制器使用的稳定名称。
    pub(crate) fn expression_name(&self) -> &str {
        &self.expression_name
    }
}

/// 表示模型目录中的一个模型及其全部服装变体。
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ModelFamily {
    display_name: String,
    variants: Vec<ModelVariant>,
    outfits: Vec<ModelOutfit>,
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

    /// 返回该模型目录中发现的外部服装表达式。
    pub(crate) fn outfits(&self) -> &[ModelOutfit] {
        &self.outfits
    }

    /// 返回模型清单和外部表达式组成的服装总数。
    pub(crate) fn outfit_count(&self) -> usize {
        self.variants.len().saturating_add(self.outfits.len())
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
    let mut outfit_groups = BTreeMap::<String, Vec<ModelOutfit>>::new();
    let mut outfits_by_directory = BTreeMap::<PathBuf, Vec<ModelOutfit>>::new();
    let mut linked_outfit_directories = BTreeSet::<(String, PathBuf)>::new();
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
            if let Some(parent) = path.parent() {
                let link_key = (family_name.clone(), parent.to_path_buf());
                if linked_outfit_directories.insert(link_key) {
                    let outfits = outfits_by_directory
                        .entry(parent.to_path_buf())
                        .or_insert_with(|| discover_external_outfits(root, parent, &mut warning));
                    outfit_groups
                        .entry(family_name)
                        .or_default()
                        .extend(outfits.iter().cloned());
                }
            }
        }
    }

    let families = grouped
        .into_iter()
        .map(|(display_name, mut variants)| {
            variants.sort_by(|left, right| left.relative_path.cmp(&right.relative_path));
            let outfits =
                deduplicate_outfits(outfit_groups.remove(&display_name).unwrap_or_default());
            ModelFamily {
                display_name,
                variants,
                outfits,
            }
        })
        .collect();
    Ok((families, warning))
}

fn discover_external_outfits(
    root: &Path,
    directory: &Path,
    warning: &mut Option<String>,
) -> Vec<ModelOutfit> {
    let resolver = match ModelResourceResolver::for_manifest(&directory.join("catalog.model3.json"))
    {
        Ok(resolver) => resolver,
        Err(error) => {
            append_warning(
                warning,
                format!("跳过无法解析的服装目录 {}：{error}", directory.display()),
            );
            return Vec::new();
        }
    };
    let expressions = match resolver.try_discover_external_expressions() {
        Ok(expressions) => expressions,
        Err(error) => {
            append_warning(
                warning,
                format!("跳过无法扫描的服装目录 {}：{error}", directory.display()),
            );
            return Vec::new();
        }
    };

    expressions
        .into_iter()
        .filter_map(|reference| {
            let path = directory.join(reference.reference());
            let relative_path = path.strip_prefix(root).ok()?.to_path_buf();
            let expression_name = reference.name().to_owned();
            Some(ModelOutfit {
                expression_name,
                relative_path,
            })
        })
        .collect()
}

fn deduplicate_outfits(outfits: Vec<ModelOutfit>) -> Vec<ModelOutfit> {
    let mut outfits = outfits;
    outfits.sort_by(|left, right| left.relative_path.cmp(&right.relative_path));
    outfits.dedup_by(|left, right| left.relative_path == right.relative_path);
    outfits
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

#[cfg(test)]
mod tests {
    use std::time::{SystemTime, UNIX_EPOCH};

    use super::*;

    struct TestDirectory(PathBuf);

    impl TestDirectory {
        fn new() -> Self {
            let nonce = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("系统时间必须晚于 Unix 纪元")
                .as_nanos();
            let path = std::env::temp_dir().join(format!(
                "lunamate-model-catalog-{}-{nonce}",
                std::process::id()
            ));
            fs::create_dir_all(&path).expect("测试模型目录应当可以创建");
            Self(path)
        }

        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TestDirectory {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn manifests_under_one_model_directory_become_outfits() {
        let directory = TestDirectory::new();
        let runtime = directory.path().join("luna/runtime");
        fs::create_dir_all(&runtime).expect("测试模型子目录应当可以创建");
        fs::write(runtime.join("luna-default.model3.json"), "{}")
            .expect("默认服装清单应当可以创建");
        fs::write(runtime.join("luna-summer.model3.json"), "{}").expect("夏季服装清单应当可以创建");

        let catalog = ModelCatalog::load(directory.path().to_path_buf(), None)
            .expect("测试模型目录应当可以扫描");

        assert_eq!(catalog.families().len(), 1);
        assert_eq!(catalog.families()[0].display_name(), "luna");
        assert_eq!(catalog.families()[0].variants().len(), 2);
        assert!(catalog.selected_model_path().is_some());
    }

    #[test]
    fn external_expression_files_become_outfit_presets() {
        let directory = TestDirectory::new();
        let model_directory = directory.path().join("20260614");
        fs::create_dir_all(&model_directory).expect("测试模型目录应当可以创建");
        fs::write(model_directory.join("20260614.model3.json"), "{}")
            .expect("测试模型清单应当可以创建");
        fs::write(model_directory.join("侦探.exp3.json"), "{}")
            .expect("测试服装表达式应当可以创建");
        fs::write(model_directory.join("女仆.exp3.json"), "{}")
            .expect("测试服装表达式应当可以创建");

        let catalog = ModelCatalog::load(directory.path().to_path_buf(), None)
            .expect("测试模型目录应当可以扫描");
        let family = &catalog.families()[0];

        assert_eq!(family.outfit_count(), 3);
        assert_eq!(family.outfits().len(), 2);
        assert!(
            family
                .outfits()
                .iter()
                .any(|outfit| outfit.expression_name() == "侦探")
        );
        assert!(
            family
                .outfits()
                .iter()
                .any(|outfit| outfit.expression_name() == "女仆")
        );
    }

    #[cfg(unix)]
    #[test]
    fn external_outfit_symlink_outside_model_directory_is_not_catalogued() {
        use std::os::unix::fs::symlink;

        let directory = TestDirectory::new();
        let model_directory = directory.path().join("luna");
        fs::create_dir_all(&model_directory).expect("测试模型目录应当可以创建");
        fs::write(model_directory.join("luna.model3.json"), "{}")
            .expect("测试模型清单应当可以创建");
        let outside = directory.path().join("outside.exp3.json");
        fs::write(&outside, "{}").expect("测试越界表情应当可以创建");
        symlink(&outside, model_directory.join("linked.exp3.json"))
            .expect("测试符号链接应当可以创建");

        let catalog = ModelCatalog::load(directory.path().to_path_buf(), None)
            .expect("测试模型目录应当可以扫描");

        assert!(catalog.families()[0].outfits().is_empty());
    }

    #[test]
    fn configured_outfit_is_restored_when_available() {
        let directory = TestDirectory::new();
        let runtime = directory.path().join("luna/runtime");
        fs::create_dir_all(&runtime).expect("测试模型子目录应当可以创建");
        fs::write(runtime.join("default.model3.json"), "{}").expect("默认服装清单应当可以创建");
        fs::write(runtime.join("summer.model3.json"), "{}").expect("夏季服装清单应当可以创建");
        let selected = Path::new("luna/runtime/summer.model3.json");

        let catalog = ModelCatalog::load(directory.path().to_path_buf(), Some(selected))
            .expect("测试模型目录应当可以扫描");

        assert_eq!(catalog.selected_relative_path(), Some(selected));
    }

    #[test]
    fn multiple_model_families_require_a_valid_configured_selection() {
        let directory = TestDirectory::new();
        for family in ["luna", "mate"] {
            let runtime = directory.path().join(family);
            fs::create_dir_all(&runtime).expect("测试模型目录应当可以创建");
            fs::write(runtime.join(format!("{family}.model3.json")), "{}")
                .expect("测试模型清单应当可以创建");
        }

        let catalog = ModelCatalog::load(directory.path().to_path_buf(), None)
            .expect("测试模型目录应当可以扫描");
        assert_eq!(catalog.families().len(), 2);
        assert_eq!(catalog.selected_model_path(), None);
    }

    #[test]
    fn excessive_discovery_depth_warns_without_hiding_root_model() {
        let directory = TestDirectory::new();
        fs::write(directory.path().join("luna.model3.json"), "{}")
            .expect("根目录模型清单应当可以创建");
        let mut nested = directory.path().to_path_buf();
        for depth in 0..=MAX_DISCOVERY_DEPTH {
            nested.push(format!("nested-{depth}"));
            fs::create_dir(&nested).expect("嵌套测试目录应当可以创建");
        }

        let catalog = ModelCatalog::load(directory.path().to_path_buf(), None)
            .expect("超过扫描深度不应丢弃已发现模型");
        assert_eq!(catalog.counts(), (1, 1));
        assert!(
            catalog
                .warning()
                .is_some_and(|warning| warning.contains("扫描深度"))
        );
    }
}
