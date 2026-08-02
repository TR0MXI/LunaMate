use std::{fs, io::Read as _, path::PathBuf};

use crate::model::capabilities::{
    AuxiliaryResourceBudget, ExternalExpressionReference, MAX_AUXILIARY_RESOURCE_BYTES,
    ModelDiagnosticCategory, ModelResourceResolver,
};
use crate::model::catalog::ModelCatalog;

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

#[test]
fn empty_and_absolute_references_are_rejected_before_touching_the_filesystem() {
    let directory = TestDirectory::new();
    let resolver = ModelResourceResolver::for_manifest(&directory.path().join("model.model3.json"))
        .expect("测试模型目录应当可以解析");

    for reference in ["", ".", "./", "/etc/hostname", "a/../../escape.exp3.json"] {
        let error = resolver
            .read_text(reference, MAX_AUXILIARY_RESOURCE_BYTES)
            .expect_err(&format!("{reference:?} 应当被拒绝"));
        assert_eq!(
            error.category(),
            ModelDiagnosticCategory::InvalidReference,
            "{reference:?} 应当归类为引用无效"
        );
    }
}

#[test]
fn missing_references_are_classified_as_missing_resources() {
    let directory = TestDirectory::new();
    let resolver = ModelResourceResolver::for_manifest(&directory.path().join("model.model3.json"))
        .expect("测试模型目录应当可以解析");

    let error = resolver
        .read_text("absent.exp3.json", MAX_AUXILIARY_RESOURCE_BYTES)
        .expect_err("缺失资源必须被拒绝");

    assert_eq!(error.category(), ModelDiagnosticCategory::Missing);
    assert!(error.message().contains("路径不存在"));
    assert!(error.to_string().starts_with("资源缺失"));
}

#[test]
fn nested_relative_references_inside_the_model_directory_are_accepted() {
    let directory = TestDirectory::new();
    let nested = directory.path().join("motions/idle");
    fs::create_dir_all(&nested).expect("测试子目录应当可以创建");
    fs::write(nested.join("idle.motion3.json"), "{\"ok\":true}").expect("测试嵌套资源应当可以创建");
    let resolver = ModelResourceResolver::for_manifest(&directory.path().join("model.model3.json"))
        .expect("测试模型目录应当可以解析");

    let source = resolver
        .read_text(
            "./motions/idle/idle.motion3.json",
            MAX_AUXILIARY_RESOURCE_BYTES,
        )
        .expect("目录内的嵌套相对引用应当可以读取");

    assert_eq!(source, "{\"ok\":true}");
}

#[test]
fn non_utf8_resource_contents_are_reported_as_parse_failures() {
    let directory = TestDirectory::new();
    fs::write(
        directory.path().join("broken.exp3.json"),
        [0xFF, 0xFE, 0xFD],
    )
    .expect("测试非 UTF-8 资源应当可以创建");
    let resolver = ModelResourceResolver::for_manifest(&directory.path().join("model.model3.json"))
        .expect("测试模型目录应当可以解析");

    let error = resolver
        .read_text("broken.exp3.json", MAX_AUXILIARY_RESOURCE_BYTES)
        .expect_err("非 UTF-8 资源必须被拒绝");

    assert_eq!(error.category(), ModelDiagnosticCategory::Parse);
}

#[test]
fn a_resource_that_exactly_fills_the_budget_is_still_read() {
    let directory = TestDirectory::new();
    fs::write(directory.path().join("exact.exp3.json"), "{}").expect("测试资源应当可以创建");
    let resolver = ModelResourceResolver::for_manifest(&directory.path().join("model.model3.json"))
        .expect("测试模型目录应当可以解析");
    let mut budget = AuxiliaryResourceBudget::with_limit(2);

    let source = resolver
        .read_text_with_budget("exact.exp3.json", MAX_AUXILIARY_RESOURCE_BYTES, &mut budget)
        .expect("恰好用尽预算的资源应当可以读取");

    assert_eq!(source, "{}");
    // 预算已归零，后续读取即使是同一个文件也必须失败。
    assert_eq!(
        resolver
            .read_text_with_budget("exact.exp3.json", MAX_AUXILIARY_RESOURCE_BYTES, &mut budget)
            .expect_err("预算耗尽后不应继续读取")
            .category(),
        ModelDiagnosticCategory::LimitExceeded
    );
}

#[test]
fn manifests_without_a_parent_directory_resolve_against_the_working_directory() {
    let resolver = ModelResourceResolver::for_manifest(std::path::Path::new("model.model3.json"))
        .expect("无父目录的清单应当回退到工作目录");

    // 工作目录内不存在该资源，但边界建立本身必须成功。
    assert_eq!(
        resolver
            .read_text("definitely-absent.exp3.json", MAX_AUXILIARY_RESOURCE_BYTES)
            .expect_err("工作目录中不存在该资源")
            .category(),
        ModelDiagnosticCategory::Missing
    );
}

#[test]
fn resolver_creation_fails_when_the_model_directory_is_unreachable() {
    let directory = TestDirectory::new();
    let missing = directory.path().join("absent/model.model3.json");

    let error = ModelResourceResolver::for_manifest(&missing)
        .expect_err("不存在的模型目录不能建立解析边界");

    assert_eq!(error.category(), ModelDiagnosticCategory::Missing);
}

#[test]
fn external_expression_discovery_is_sorted_and_skips_unusable_entries() {
    let directory = TestDirectory::new();
    for name in ["女仆", "侦探", "Default"] {
        fs::write(directory.path().join(format!("{name}.exp3.json")), "{}")
            .expect("测试外部表情应当可以创建");
    }
    // 目录、空名与非表情文件都不应进入候选集合。
    fs::create_dir(directory.path().join("group.exp3.json")).expect("测试目录应当可以创建");
    fs::write(directory.path().join(".exp3.json"), "{}").expect("测试空名表情应当可以创建");
    fs::write(directory.path().join("notes.json"), "{}").expect("测试干扰文件应当可以创建");
    let resolver = ModelResourceResolver::for_manifest(&directory.path().join("model.model3.json"))
        .expect("测试模型目录应当可以解析");

    let discovered = resolver.discover_external_expressions();
    let names = discovered
        .iter()
        .map(ExternalExpressionReference::name)
        .collect::<Vec<_>>();

    // 候选按文件名字节序排序，保证服装列表在不同文件系统上顺序稳定。
    assert_eq!(names, ["Default", "侦探", "女仆"]);
    assert_eq!(discovered[0].reference(), "Default.exp3.json");
    assert!(
        discovered
            .iter()
            .all(|reference| reference.movable_to_outfit())
    );
}

#[test]
fn dedicated_directories_are_scanned_without_recursing() {
    let directory = TestDirectory::new();
    fs::create_dir_all(directory.path().join("motions/nested")).expect("测试动作目录应当可以创建");
    fs::create_dir_all(directory.path().join("expressions/nested"))
        .expect("测试表情目录应当可以创建");
    for reference in [
        "root.motion3.json",
        "motions/dedicated.motion3.json",
        "motions/nested/ignored.motion3.json",
    ] {
        fs::write(directory.path().join(reference), "{}").expect("测试动作候选应当可以创建");
    }
    for reference in [
        "root.exp3.json",
        "expressions/dedicated.exp3.json",
        "expressions/nested/ignored.exp3.json",
    ] {
        fs::write(directory.path().join(reference), "{}").expect("测试表情候选应当可以创建");
    }
    let resolver = ModelResourceResolver::for_manifest(&directory.path().join("model.model3.json"))
        .expect("测试模型目录应当可以解析");

    let motions = resolver.discover_external_motions();
    assert_eq!(
        motions
            .iter()
            .map(|reference| reference.reference())
            .collect::<Vec<_>>(),
        ["motions/dedicated.motion3.json", "root.motion3.json"]
    );
    assert_eq!(
        motions[0].runtime_id(),
        "external:motions/dedicated.motion3.json"
    );

    let expressions = resolver.discover_external_expressions();
    assert_eq!(
        expressions
            .iter()
            .map(|reference| reference.reference())
            .collect::<Vec<_>>(),
        ["expressions/dedicated.exp3.json", "root.exp3.json"]
    );
    assert!(!expressions[0].movable_to_outfit());
    assert!(expressions[1].movable_to_outfit());
}

#[test]
fn discovery_returns_an_empty_list_when_the_model_directory_disappears() {
    let directory = TestDirectory::new();
    let model_dir = directory.path().join("runtime");
    fs::create_dir(&model_dir).expect("测试模型目录应当可以创建");
    let resolver = ModelResourceResolver::for_manifest(&model_dir.join("model.model3.json"))
        .expect("测试模型目录应当可以解析");
    fs::remove_dir_all(&model_dir).expect("测试模型目录应当可以删除");

    assert!(resolver.discover_external_expressions().is_empty());
    assert_eq!(
        resolver
            .try_discover_external_expressions()
            .expect_err("目录消失后应当保留诊断")
            .category(),
        ModelDiagnosticCategory::InvalidReference
    );
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

#[cfg(unix)]
#[test]
fn optional_resource_replaced_by_external_symlink_before_open_is_rejected() {
    use std::os::unix::fs::symlink;

    let directory = TestDirectory::new();
    let model_dir = directory.path().join("runtime");
    fs::create_dir(&model_dir).expect("测试模型目录应当可以创建");
    let resource = model_dir.join("idle.exp3.json");
    let outside = directory.path().join("outside.exp3.json");
    fs::write(&resource, "{\"inside\":true}").expect("模型内可选资源应当可以创建");
    fs::write(&outside, "{\"outside\":true}").expect("目录外替代资源应当可以创建");
    let canonical_resource = fs::canonicalize(&resource).expect("可选资源规范路径应当可以取得");
    let resolver = ModelResourceResolver::for_manifest(&model_dir.join("model.model3.json"))
        .expect("测试模型目录应当可以解析");
    let mut hook_called = false;

    let error = resolver
        .read_text_with_open_hook_for_test(
            "idle.exp3.json",
            MAX_AUXILIARY_RESOURCE_BYTES,
            |canonical_path| {
                assert_eq!(canonical_path, canonical_resource);
                fs::remove_file(canonical_path).expect("已校验可选资源应当可以移除");
                symlink(&outside, canonical_path).expect("目录外替代符号链接应当可以创建");
                hook_called = true;
            },
        )
        .expect_err("父目录句柄锚定后、final 打开前换入的符号链接必须被拒绝");

    assert!(hook_called, "测试替换必须发生在最终打开之前");
    assert_eq!(error.category(), ModelDiagnosticCategory::InvalidReference);
    assert!(error.message().contains("打开期间发生变化"));
}

#[cfg(unix)]
#[test]
fn rejects_symbolic_link_in_an_intermediate_directory_component() {
    use std::os::unix::fs::symlink;

    let directory = TestDirectory::new();
    let model_dir = directory.path().join("runtime");
    let outside = directory.path().join("outside-motions");
    fs::create_dir(&model_dir).expect("测试模型目录应当可以创建");
    fs::create_dir(&outside).expect("模型外动作目录应当可以创建");
    fs::write(outside.join("idle.motion3.json"), "{}").expect("模型外动作应当可以创建");
    symlink(&outside, model_dir.join("motions")).expect("测试目录符号链接应当可以创建");
    let resolver = ModelResourceResolver::for_manifest(&model_dir.join("model.model3.json"))
        .expect("测试模型目录应当可以解析");

    let error = resolver
        .read_text("motions/idle.motion3.json", MAX_AUXILIARY_RESOURCE_BYTES)
        .expect_err("中间目录符号链接必须被逐分量 NOFOLLOW 拒绝");

    assert_eq!(error.category(), ModelDiagnosticCategory::InvalidReference);
}

#[cfg(target_os = "windows")]
#[test]
fn rejects_reparse_point_in_an_intermediate_directory_component() {
    let directory = TestDirectory::new();
    let model_dir = directory.path().join("runtime");
    let outside = directory.path().join("outside-motions");
    let junction = model_dir.join("motions");
    fs::create_dir(&model_dir).expect("测试模型目录应当可以创建");
    fs::create_dir(&outside).expect("模型外动作目录应当可以创建");
    fs::write(outside.join("idle.motion3.json"), "{}").expect("模型外动作应当可以创建");
    let status = std::process::Command::new("cmd")
        .args(["/C", "mklink", "/J"])
        .arg(&junction)
        .arg(&outside)
        .status()
        .expect("Windows 测试环境应当可以启动 cmd");
    assert!(status.success(), "测试目录 junction 应当可以创建");
    let resolver = ModelResourceResolver::for_manifest(&model_dir.join("model.model3.json"))
        .expect("测试模型目录应当可以解析");

    let error = resolver
        .read_text("motions/idle.motion3.json", MAX_AUXILIARY_RESOURCE_BYTES)
        .expect_err("中间目录重解析点必须被逐分量句柄校验拒绝");

    assert_eq!(error.category(), ModelDiagnosticCategory::InvalidReference);
    fs::remove_dir(&junction).expect("测试 junction 应当可以移除");
}

#[cfg(unix)]
#[test]
fn optional_resource_replaced_by_fifo_before_open_is_rejected_without_blocking() {
    let directory = TestDirectory::new();
    let resource = directory.path().join("idle.exp3.json");
    fs::write(&resource, "{}").expect("模型内可选资源应当可以创建");
    let resolver = ModelResourceResolver::for_manifest(&directory.path().join("model.model3.json"))
        .expect("测试模型目录应当可以解析");

    let error = resolver
        .read_text_with_open_hook_for_test(
            "idle.exp3.json",
            MAX_AUXILIARY_RESOURCE_BYTES,
            |canonical_path| {
                fs::remove_file(canonical_path).expect("已校验可选资源应当可以移除");
                let status = std::process::Command::new("mkfifo")
                    .arg(canonical_path)
                    .status()
                    .expect("测试环境应当提供 mkfifo");
                assert!(status.success(), "测试 FIFO 应当可以创建");
            },
        )
        .expect_err("final 打开前替换的 FIFO 必须无阻塞地被拒绝");

    assert_eq!(error.category(), ModelDiagnosticCategory::NotFile);
}

#[cfg(unix)]
#[test]
fn discovery_skips_dedicated_directory_symlinks_outside_model_root() {
    use std::os::unix::fs::symlink;

    let directory = TestDirectory::new();
    let model_dir = directory.path().join("runtime");
    let outside = directory.path().join("outside-motions");
    fs::create_dir(&model_dir).expect("测试模型目录应当可以创建");
    fs::create_dir(&outside).expect("测试外部目录应当可以创建");
    fs::write(outside.join("wave.motion3.json"), "{}").expect("测试外部动作应当可以创建");
    symlink(&outside, model_dir.join("motions")).expect("测试目录符号链接应当可以创建");
    let resolver = ModelResourceResolver::for_manifest(&model_dir.join("model.model3.json"))
        .expect("测试模型目录应当可以解析");

    assert!(resolver.discover_external_motions().is_empty());
}

#[cfg(unix)]
#[test]
fn scanned_manifest_root_swap_and_restore_cannot_return_malicious_handle() {
    let directory = TestDirectory::new();
    let models_root = directory.path().join("models");
    let family = models_root.join("luna");
    let malicious_root = directory.path().join("malicious-models");
    let malicious_family = malicious_root.join("luna");
    let saved_root = directory.path().join("models-original");
    fs::create_dir_all(&family).expect("扫描内模型目录应当可以创建");
    fs::create_dir_all(&malicious_family).expect("恶意替代模型目录应当可以创建");
    fs::write(family.join("luna.model3.json"), "{\"trusted\":true}")
        .expect("扫描内清单应当可以创建");
    fs::write(
        malicious_family.join("luna.model3.json"),
        "{\"malicious\":true}",
    )
    .expect("恶意替代清单应当可以创建");
    let catalog = ModelCatalog::load(models_root.clone(), None).expect("模型目录应当可以扫描");
    let manifest = catalog
        .model_path(std::path::Path::new("luna/luna.model3.json"))
        .expect("扫描结果应当携带清单根快照");

    let mut swapped = false;
    let mut restored = false;
    let (_, mut opened_manifest) = ModelResourceResolver::open_manifest_with_open_hooks_for_test(
        &manifest,
        MAX_AUXILIARY_RESOURCE_BYTES,
        &mut |path| {
            assert_eq!(path, manifest.path());
            fs::rename(&models_root, &saved_root).expect("原扫描根目录应当可以移走");
            fs::rename(&malicious_root, &models_root).expect("恶意普通目录应当可以换入");
            swapped = true;
        },
        &mut |path| {
            let malicious_source =
                fs::read_to_string(path).expect("换入后的路径必须能打开恶意普通 manifest");
            assert_eq!(malicious_source, "{\"malicious\":true}");
            fs::rename(&models_root, &malicious_root).expect("恶意普通目录应当可以移回");
            fs::rename(&saved_root, &models_root).expect("原扫描根目录应当可以恢复");
            restored = true;
        },
    )
    .expect("路径替换不能改变已打开的 root 与父目录句柄锚点");
    let mut source = String::new();
    opened_manifest
        .read_to_string(&mut source)
        .expect("锚定的清单句柄应当可以读取");

    assert!(swapped, "恶意 root 必须在 final 打开前换入");
    assert!(restored, "原 root 必须在 final 打开后恢复");
    assert_eq!(source, "{\"trusted\":true}");
}

#[cfg(any(unix, target_os = "windows"))]
#[test]
fn scanned_manifest_rejects_models_root_replacement() {
    let directory = TestDirectory::new();
    let models_root = directory.path().join("models");
    let family = models_root.join("luna");
    let outside = directory.path().join("outside-root");
    let original = directory.path().join("models-original");
    fs::create_dir_all(&family).expect("扫描内模型目录应当可以创建");
    fs::create_dir_all(outside.join("luna")).expect("扫描外模型目录应当可以创建");
    fs::write(family.join("luna.model3.json"), "{}").expect("扫描内清单应当可以创建");
    fs::write(outside.join("luna/luna.model3.json"), "{}").expect("扫描外清单应当可以创建");
    let catalog = ModelCatalog::load(models_root.clone(), None).expect("模型目录应当可以扫描");
    let manifest = catalog
        .model_path(std::path::Path::new("luna/luna.model3.json"))
        .expect("扫描结果应当携带清单根快照");

    fs::rename(&models_root, &original).expect("扫描根目录应当可以移走");
    fs::rename(&outside, &models_root).expect("普通替代根目录应当可以换入");

    let error = ModelResourceResolver::open_manifest(&manifest, MAX_AUXILIARY_RESOURCE_BYTES)
        .expect_err("扫描后替换的模型根目录必须被拒绝");
    assert_eq!(error.category(), ModelDiagnosticCategory::InvalidReference);
    assert!(error.message().contains("目录扫描后发生变化"));
}
