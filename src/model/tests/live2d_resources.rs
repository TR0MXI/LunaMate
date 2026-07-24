use std::{
    fs,
    path::{Path, PathBuf},
};

use crate::model::live2d::resources::validate_model_resources;

use std::time::{SystemTime, UNIX_EPOCH};

struct TestDirectory(PathBuf);

impl TestDirectory {
    fn new() -> Self {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("系统时间必须晚于 Unix 纪元")
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "lunamate-resource-validation-{}-{nonce}",
            std::process::id()
        ));
        fs::create_dir_all(&path).expect("测试资源目录应当可以创建");
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

fn write_manifest(directory: &Path, moc: &str) -> PathBuf {
    let path = directory.join("model.model3.json");
    fs::write(
        &path,
        format!(r#"{{"Version":3,"FileReferences":{{"Moc":"{moc}","Textures":[]}}}}"#),
    )
    .expect("测试模型清单应当可以创建");
    path
}

#[test]
fn accepts_regular_resources_inside_manifest_directory() {
    let directory = TestDirectory::new();
    fs::write(directory.path().join("model.moc3"), []).expect("测试 MOC 文件应当可以创建");
    let manifest = write_manifest(directory.path(), "model.moc3");

    let _resolver = validate_model_resources(&manifest).expect("目录内普通资源应当通过预检");
}

#[test]
fn rejects_parent_directory_references() {
    let directory = TestDirectory::new();
    let runtime = directory.path().join("runtime");
    fs::create_dir(&runtime).expect("测试运行时目录应当可以创建");
    fs::write(directory.path().join("outside.moc3"), []).expect("测试越界资源应当可以创建");
    let manifest = write_manifest(&runtime, "../outside.moc3");

    let error = validate_model_resources(&manifest)
        .expect_err("父目录引用必须被拒绝")
        .to_string();
    assert!(error.contains("相对路径"));
}

#[cfg(unix)]
#[test]
fn rejects_symbolic_links_outside_manifest_directory() {
    use std::os::unix::fs::symlink;

    let directory = TestDirectory::new();
    let runtime = directory.path().join("runtime");
    fs::create_dir(&runtime).expect("测试运行时目录应当可以创建");
    let outside = directory.path().join("outside.moc3");
    fs::write(&outside, []).expect("测试越界资源应当可以创建");
    symlink(&outside, runtime.join("linked.moc3")).expect("测试符号链接应当可以创建");
    let manifest = write_manifest(&runtime, "linked.moc3");

    let error = validate_model_resources(&manifest)
        .expect_err("指向目录外的符号链接必须被拒绝")
        .to_string();
    assert!(error.contains("越出模型目录"));
}

#[test]
fn optional_resources_do_not_fail_required_preflight() {
    let directory = TestDirectory::new();
    fs::write(directory.path().join("model.moc3"), []).expect("测试 MOC 文件应当可以创建");
    let manifest = directory.path().join("model.model3.json");
    fs::write(
        &manifest,
        r#"{
                "Version": 3,
                "FileReferences": {
                    "Moc": "model.moc3",
                    "Textures": [],
                    "DisplayInfo": "../outside.cdi3.json",
                    "Motions": {"Tap": [{"File": "missing.motion3.json"}]},
                    "Expressions": [{"Name": "Broken", "File": "missing.exp3.json"}]
                }
            }"#,
    )
    .expect("测试模型清单应当可以创建");

    let _resolver =
        validate_model_resources(&manifest).expect("可选资源损坏不应阻止主体必需资源预检");
}
