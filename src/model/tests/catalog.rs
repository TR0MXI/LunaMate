use std::{
    fs,
    path::{Path, PathBuf},
};

use crate::model::catalog::{
    MAX_DISCOVERY_DEPTH, ModelCatalog, ModelFamily, ensure_model_directory,
};

use std::time::{SystemTime, UNIX_EPOCH};

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
    fs::write(runtime.join("luna-default.model3.json"), "{}").expect("默认服装清单应当可以创建");
    fs::write(runtime.join("luna-summer.model3.json"), "{}").expect("夏季服装清单应当可以创建");

    let catalog =
        ModelCatalog::load(directory.path().to_path_buf(), None).expect("测试模型目录应当可以扫描");

    assert_eq!(catalog.families().len(), 1);
    assert_eq!(catalog.families()[0].display_name(), "luna");
    assert_eq!(catalog.families()[0].variants().len(), 2);
    assert!(catalog.selected_model_path().is_some());
}

#[test]
fn external_expression_files_do_not_become_manifest_variants() {
    let directory = TestDirectory::new();
    let model_directory = directory.path().join("20260614");
    fs::create_dir_all(&model_directory).expect("测试模型目录应当可以创建");
    fs::write(model_directory.join("20260614.model3.json"), "{}")
        .expect("测试模型清单应当可以创建");
    fs::write(model_directory.join("侦探.exp3.json"), "{}").expect("测试服装表达式应当可以创建");
    fs::write(model_directory.join("女仆.exp3.json"), "{}").expect("测试服装表达式应当可以创建");

    let catalog =
        ModelCatalog::load(directory.path().to_path_buf(), None).expect("测试模型目录应当可以扫描");
    let family = &catalog.families()[0];

    // 参数服装必须等模型成功加载后再由设置分类，目录扫描只统计完整模型清单。
    assert_eq!(family.outfit_count(), 1);
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

    let catalog =
        ModelCatalog::load(directory.path().to_path_buf(), None).expect("测试模型目录应当可以扫描");
    assert_eq!(catalog.families().len(), 2);
    assert_eq!(catalog.selected_model_path(), None);
}

#[test]
fn excessive_discovery_depth_warns_without_hiding_root_model() {
    let directory = TestDirectory::new();
    fs::write(directory.path().join("luna.model3.json"), "{}").expect("根目录模型清单应当可以创建");
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

#[test]
fn missing_model_root_is_treated_as_an_empty_catalog() {
    let directory = TestDirectory::new();
    let root = directory.path().join("never-created");

    let catalog = ModelCatalog::load(root.clone(), None).expect("不存在的模型目录不应视为启动错误");

    assert_eq!(catalog.counts(), (0, 0));
    assert_eq!(catalog.root(), root);
    assert!(catalog.warning().is_none());
    assert!(catalog.selected_model_path().is_none());
    assert!(catalog.selected_family().is_none());
}

#[test]
fn ensuring_a_missing_model_root_creates_it() {
    let directory = TestDirectory::new();
    let root = directory.path().join("models");
    assert!(!root.exists());

    ensure_model_directory(&root).expect("缺失的模型目录应当可以创建");

    assert!(root.is_dir());
}

#[test]
fn model_root_that_is_not_a_directory_is_a_load_error() {
    let directory = TestDirectory::new();
    let root = directory.path().join("models.txt");
    fs::write(&root, "not a directory").expect("测试文件应当可以创建");

    let error = ModelCatalog::load(root, None).expect_err("根目录不可扫描时应当返回错误");

    assert!(error.to_string().contains("无法扫描模型目录"));
    assert!(std::error::Error::source(&error).is_some());
}

#[test]
fn empty_catalog_preserves_the_configured_root() {
    let directory = TestDirectory::new();

    let catalog = ModelCatalog::empty(directory.path().to_path_buf());

    assert_eq!(catalog.root(), directory.path());
    assert_eq!(catalog.counts(), (0, 0));
    assert!(catalog.families().is_empty());
    assert!(catalog.selected_relative_path().is_none());
    assert!(catalog.warning().is_none());
}

#[test]
fn non_manifest_files_are_ignored_during_discovery() {
    let directory = TestDirectory::new();
    let model_directory = directory.path().join("luna");
    fs::create_dir_all(&model_directory).expect("测试模型目录应当可以创建");
    fs::write(model_directory.join("luna.model3.json"), "{}").expect("模型清单应当可以创建");
    for noise in ["luna.moc3", "texture.png", "model3.json", "readme.md"] {
        fs::write(model_directory.join(noise), "{}").expect("干扰文件应当可以创建");
    }

    let catalog =
        ModelCatalog::load(directory.path().to_path_buf(), None).expect("测试模型目录应当可以扫描");

    assert_eq!(catalog.counts(), (1, 1));
    assert!(catalog.warning().is_none());
}

#[test]
fn variant_names_drop_the_redundant_family_prefix() {
    let directory = TestDirectory::new();
    let model_directory = directory.path().join("luna");
    fs::create_dir_all(&model_directory).expect("测试模型目录应当可以创建");
    for stem in ["luna_summer", "luna-winter", "luna", "alt"] {
        fs::write(model_directory.join(format!("{stem}.model3.json")), "{}")
            .expect("模型清单应当可以创建");
    }

    let catalog =
        ModelCatalog::load(directory.path().to_path_buf(), None).expect("测试模型目录应当可以扫描");
    let names = catalog.families()[0]
        .variants()
        .iter()
        .map(|variant| variant.display_name().to_owned())
        .collect::<Vec<_>>();

    // 变体按相对路径排序；与家族同名的清单没有可用后缀，保留完整名称以免出现空标签。
    assert_eq!(names, ["alt", "winter", "luna", "summer"]);
}

#[test]
fn selecting_a_family_keeps_the_current_outfit_when_it_still_belongs_to_it() {
    let directory = TestDirectory::new();
    for family in ["luna", "mate"] {
        let model_directory = directory.path().join(family);
        fs::create_dir_all(&model_directory).expect("测试模型目录应当可以创建");
        for stem in ["a", "b"] {
            fs::write(
                model_directory.join(format!("{family}-{stem}.model3.json")),
                "{}",
            )
            .expect("模型清单应当可以创建");
        }
    }
    let selected = Path::new("luna/luna-b.model3.json");
    let mut catalog = ModelCatalog::load(directory.path().to_path_buf(), Some(selected))
        .expect("测试模型目录应当可以扫描");

    let luna = catalog
        .families()
        .iter()
        .position(|family| family.display_name() == "luna")
        .expect("扫描结果应当包含 luna 家族");
    let mate = catalog
        .families()
        .iter()
        .position(|family| family.display_name() == "mate")
        .expect("扫描结果应当包含 mate 家族");

    assert_eq!(
        catalog.select_family(luna).expect("重选当前家族应当成功"),
        directory.path().join(selected)
    );
    assert_eq!(
        catalog.select_family(mate).expect("切换家族应当成功"),
        directory.path().join("mate/mate-a.model3.json")
    );
    assert_eq!(
        catalog
            .selected_family()
            .map(ModelFamily::display_name)
            .expect("切换后应当有选中家族"),
        "mate"
    );
}

#[test]
fn selecting_a_family_outside_the_scan_result_is_rejected() {
    let directory = TestDirectory::new();
    fs::write(directory.path().join("luna.model3.json"), "{}").expect("模型清单应当可以创建");
    let mut catalog =
        ModelCatalog::load(directory.path().to_path_buf(), None).expect("测试模型目录应当可以扫描");

    let error = catalog.select_family(9).expect_err("越界索引应当被拒绝");

    assert!(error.to_string().contains("模型索引不在当前扫描结果中"));
    assert!(std::error::Error::source(&error).is_none());
}

#[test]
fn selecting_a_variant_outside_the_scan_result_is_rejected() {
    let directory = TestDirectory::new();
    let model_directory = directory.path().join("luna");
    fs::create_dir_all(&model_directory).expect("测试模型目录应当可以创建");
    fs::write(model_directory.join("luna.model3.json"), "{}").expect("模型清单应当可以创建");
    let mut catalog =
        ModelCatalog::load(directory.path().to_path_buf(), None).expect("测试模型目录应当可以扫描");

    let error = catalog
        .select_variant(Path::new("../escape.model3.json"))
        .expect_err("目录外路径应当被拒绝");
    assert!(error.to_string().contains("模型不在当前目录扫描结果中"));

    let selected = Path::new("luna/luna.model3.json");
    assert_eq!(
        catalog
            .select_variant(selected)
            .expect("扫描结果内的清单应当可以选择"),
        directory.path().join(selected)
    );
    assert_eq!(catalog.selected_relative_path(), Some(selected));
}

#[test]
fn stale_configured_selection_falls_back_to_the_only_family() {
    let directory = TestDirectory::new();
    let model_directory = directory.path().join("luna");
    fs::create_dir_all(&model_directory).expect("测试模型目录应当可以创建");
    fs::write(model_directory.join("luna.model3.json"), "{}").expect("模型清单应当可以创建");

    let catalog = ModelCatalog::load(
        directory.path().to_path_buf(),
        Some(Path::new("removed/removed.model3.json")),
    )
    .expect("测试模型目录应当可以扫描");

    assert_eq!(
        catalog.selected_relative_path(),
        Some(Path::new("luna/luna.model3.json"))
    );
}

#[test]
fn manifests_without_a_stem_fall_back_to_a_placeholder_name() {
    let directory = TestDirectory::new();
    fs::write(directory.path().join(".model3.json"), "{}").expect("无名清单应当可以创建");

    let catalog =
        ModelCatalog::load(directory.path().to_path_buf(), None).expect("测试模型目录应当可以扫描");

    assert_eq!(catalog.families()[0].display_name(), "未命名模型");
}

#[test]
fn expression_files_do_not_change_variant_count() {
    let directory = TestDirectory::new();
    let model_directory = directory.path().join("luna");
    fs::create_dir_all(&model_directory).expect("测试模型目录应当可以创建");
    for stem in ["luna-a", "luna-b"] {
        fs::write(model_directory.join(format!("{stem}.model3.json")), "{}")
            .expect("模型清单应当可以创建");
    }
    fs::write(model_directory.join("女仆.exp3.json"), "{}").expect("服装表达式应当可以创建");

    let catalog =
        ModelCatalog::load(directory.path().to_path_buf(), None).expect("测试模型目录应当可以扫描");
    let family = &catalog.families()[0];

    assert_eq!(family.variants().len(), 2);
    assert_eq!(family.outfit_count(), 2);
}
