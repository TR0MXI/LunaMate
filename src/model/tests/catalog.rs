use std::{
    fs,
    path::{Path, PathBuf},
};

use crate::model::catalog::{MAX_DISCOVERY_DEPTH, ModelCatalog};

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
fn external_expression_files_become_outfit_presets() {
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
    fs::write(model_directory.join("luna.model3.json"), "{}").expect("测试模型清单应当可以创建");
    let outside = directory.path().join("outside.exp3.json");
    fs::write(&outside, "{}").expect("测试越界表情应当可以创建");
    symlink(&outside, model_directory.join("linked.exp3.json")).expect("测试符号链接应当可以创建");

    let catalog =
        ModelCatalog::load(directory.path().to_path_buf(), None).expect("测试模型目录应当可以扫描");

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
