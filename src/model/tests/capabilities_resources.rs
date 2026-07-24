use std::{fs, path::PathBuf};

use crate::model::capabilities::{
    AuxiliaryResourceBudget, MAX_AUXILIARY_RESOURCE_BYTES, ModelDiagnosticCategory,
    ModelResourceResolver,
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
            "lunamate-resource-resolver-{}-{nonce}",
            std::process::id()
        ));
        fs::create_dir_all(&path).expect("测试资源目录应当可以创建");
        Self(path)
    }

    fn path(&self) -> &std::path::Path {
        &self.0
    }
}

impl Drop for TestDirectory {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[test]
fn rejects_parent_reference_before_reading_file() {
    let directory = TestDirectory::new();
    let resolver = ModelResourceResolver::for_manifest(&directory.path().join("model.model3.json"))
        .expect("测试模型目录应当可以解析");

    let error = resolver
        .read_text("../outside.motion3.json", MAX_AUXILIARY_RESOURCE_BYTES)
        .expect_err("父目录引用必须被拒绝");

    assert_eq!(error.category(), ModelDiagnosticCategory::InvalidReference);
}

#[test]
fn rejects_directory_and_oversized_optional_resource() {
    let directory = TestDirectory::new();
    fs::create_dir(directory.path().join("directory.exp3.json")).expect("测试子目录应当可以创建");
    fs::write(directory.path().join("large.exp3.json"), "{}").expect("测试超限资源应当可以创建");
    let resolver = ModelResourceResolver::for_manifest(&directory.path().join("model.model3.json"))
        .expect("测试模型目录应当可以解析");

    let not_file = resolver
        .read_text("directory.exp3.json", MAX_AUXILIARY_RESOURCE_BYTES)
        .expect_err("目录不能作为可选资源读取");
    let too_large = resolver
        .read_text("large.exp3.json", 1)
        .expect_err("超过单项预算的资源必须被拒绝");

    assert_eq!(not_file.category(), ModelDiagnosticCategory::NotFile);
    assert_eq!(too_large.category(), ModelDiagnosticCategory::TooLarge);
}

#[test]
fn shared_budget_counts_repeated_resource_reads() {
    let directory = TestDirectory::new();
    fs::write(directory.path().join("first.exp3.json"), "{}").expect("测试资源应当可以创建");
    fs::write(directory.path().join("second.exp3.json"), "{}").expect("测试资源应当可以创建");
    let resolver = ModelResourceResolver::for_manifest(&directory.path().join("model.model3.json"))
        .expect("测试模型目录应当可以解析");
    let mut budget = AuxiliaryResourceBudget::with_limit(3);

    resolver
        .read_text_with_budget("first.exp3.json", MAX_AUXILIARY_RESOURCE_BYTES, &mut budget)
        .expect("首个资源应当可以读取");
    let error = resolver
        .read_text_with_budget(
            "second.exp3.json",
            MAX_AUXILIARY_RESOURCE_BYTES,
            &mut budget,
        )
        .expect_err("重复读取必须继续扣减 generation 预算");

    assert_eq!(error.category(), ModelDiagnosticCategory::LimitExceeded);
}

#[test]
fn chunked_read_stops_at_a_cancellation_checkpoint() {
    use std::cell::Cell;

    let directory = TestDirectory::new();
    fs::write(
        directory.path().join("large.motion3.json"),
        vec![b'x'; 128 * 1024],
    )
    .expect("测试分块资源应当可以创建");
    let resolver = ModelResourceResolver::for_manifest(&directory.path().join("model.model3.json"))
        .expect("测试模型目录应当可以解析");
    let mut budget = AuxiliaryResourceBudget::default();
    let checkpoints = Cell::new(0_u8);

    let error = resolver
        .read_text_with_budget_and_checkpoint(
            "large.motion3.json",
            MAX_AUXILIARY_RESOURCE_BYTES,
            &mut budget,
            || {
                let next = checkpoints.get().saturating_add(1);
                checkpoints.set(next);
                next >= 2
            },
        )
        .expect_err("第二个分块前的取消必须停止读取");

    assert_eq!(error.category(), ModelDiagnosticCategory::Read);
    assert_eq!(checkpoints.get(), 2);
}

#[cfg(unix)]
#[test]
fn rejects_symbolic_link_to_file_outside_model_directory() {
    use std::os::unix::fs::symlink;

    let directory = TestDirectory::new();
    let model_dir = directory.path().join("runtime");
    fs::create_dir(&model_dir).expect("测试模型目录应当可以创建");
    let outside = directory.path().join("outside.exp3.json");
    fs::write(&outside, "{}").expect("测试越界资源应当可以创建");
    symlink(&outside, model_dir.join("linked.exp3.json")).expect("测试符号链接应当可以创建");
    let resolver = ModelResourceResolver::for_manifest(&model_dir.join("model.model3.json"))
        .expect("测试模型目录应当可以解析");

    let error = resolver
        .read_text("linked.exp3.json", MAX_AUXILIARY_RESOURCE_BYTES)
        .expect_err("指向目录外的符号链接必须被拒绝");

    assert_eq!(error.category(), ModelDiagnosticCategory::InvalidReference);
}
