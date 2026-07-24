use crate::model::{
    capabilities::{ModelDiagnosticCategory, ModelLoadDiagnostics, ModelResourceResolver},
    expression::{ExpressionController, MAX_EXPRESSION_COUNT},
};

use std::{
    fs,
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

use mocari::json::Model3;

struct TestDirectory(PathBuf);

impl TestDirectory {
    fn new() -> Self {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("系统时间必须晚于 Unix 纪元")
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "lunamate-expression-controller-{}-{nonce}",
            std::process::id()
        ));
        fs::create_dir_all(&path).expect("测试表情目录应当可以创建");
        Self(path)
    }

    fn path(&self) -> &Path {
        &self.0
    }

    fn resolver(&self) -> ModelResourceResolver {
        ModelResourceResolver::for_manifest(&self.path().join("model.model3.json"))
            .expect("测试模型目录应当可以解析")
    }
}

impl Drop for TestDirectory {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn write_expression(path: &Path, value: f32) {
    fs::write(
            path,
            format!(
                r#"{{"Type":"Live2D Expression","Parameters":[{{"Id":"ParamAngleX","Value":{value},"Blend":"Overwrite"}}]}}"#
            ),
        )
        .expect("测试表情应当可以创建");
}

fn write_expression_with_fades(path: &Path, fade_in: f32, fade_out: f32) {
    fs::write(
            path,
            format!(
                r#"{{"Type":"Live2D Expression","FadeInTime":{fade_in},"FadeOutTime":{fade_out},"Parameters":[]}}"#
            ),
        )
        .expect("测试淡入淡出表情应当可以创建");
}

fn parse_model(expressions: &str) -> Model3 {
    Model3::from_json_str(&format!(
            r#"{{"Version":3,"FileReferences":{{"Moc":"model.moc3","Textures":[],"Expressions":{expressions}}}}}"#
        ))
        .expect("测试模型清单应当可以解析")
}

fn category_count(diagnostics: &ModelLoadDiagnostics, category: ModelDiagnosticCategory) -> usize {
    diagnostics
        .entries()
        .iter()
        .filter(|diagnostic| diagnostic.category() == category)
        .count()
}

#[test]
fn mixed_expression_failures_keep_successful_entries() {
    let directory = TestDirectory::new();
    write_expression(&directory.path().join("default.exp3.json"), 1.0);
    fs::write(directory.path().join("malformed.exp3.json"), "[").expect("损坏测试表情应当可以创建");
    let model = parse_model(
        r#"[
                {"Name":"Default","File":"default.exp3.json"},
                {"Name":"Missing","File":"missing.exp3.json"},
                {"Name":"Malformed","File":"malformed.exp3.json"}
            ]"#,
    );

    let (mut controller, diagnostics) =
        ExpressionController::load_manifest(&model, &directory.resolver());

    assert_eq!(controller.loaded_expression_count(), 1);
    assert!(controller.play("Default"));
    assert!(!controller.play("Missing"));
    assert_eq!(
        category_count(&diagnostics, ModelDiagnosticCategory::Missing),
        1
    );
    assert_eq!(
        category_count(&diagnostics, ModelDiagnosticCategory::Parse),
        1
    );
    let missing = diagnostics
        .entries()
        .iter()
        .find(|diagnostic| diagnostic.category() == ModelDiagnosticCategory::Missing)
        .expect("缺失表情应当生成诊断");
    assert_eq!(missing.name(), Some("Missing"));
    assert_eq!(missing.declaration_index(), Some(1));
    assert_eq!(missing.reference(), Some("missing.exp3.json"));
}

#[test]
fn all_bad_expressions_are_skipped_without_failing_controller() {
    let directory = TestDirectory::new();
    fs::write(directory.path().join("malformed.exp3.json"), "not-json")
        .expect("损坏测试表情应当可以创建");
    let model = parse_model(
        r#"[
                {"Name":"Malformed","File":"malformed.exp3.json"},
                {"Name":"Outside","File":"../outside.exp3.json"}
            ]"#,
    );

    let (mut controller, diagnostics) =
        ExpressionController::load_manifest(&model, &directory.resolver());

    assert_eq!(controller.loaded_expression_count(), 0);
    assert!(!controller.play("Malformed"));
    assert_eq!(
        category_count(&diagnostics, ModelDiagnosticCategory::Parse),
        1
    );
    assert_eq!(
        category_count(&diagnostics, ModelDiagnosticCategory::InvalidReference),
        1
    );
}

#[test]
fn external_expression_is_loaded_as_an_outfit() {
    let directory = TestDirectory::new();
    write_expression(&directory.path().join("侦探.exp3.json"), 1.0);
    write_expression(&directory.path().join("女仆.exp3.json"), 0.0);
    let model = parse_model("[]");
    let resolver = directory.resolver();
    let external = resolver.discover_external_expressions();

    let (mut controller, diagnostics) =
        ExpressionController::load_manifest_with_external(&model, &resolver, &external);

    assert!(diagnostics.entries().is_empty());
    assert_eq!(controller.available_outfits(), vec!["侦探", "女仆"]);
    assert!(controller.play("侦探"));
    assert!(controller.play("女仆"));
}

#[test]
fn resetting_outfit_without_default_expression_clears_active_players() {
    let directory = TestDirectory::new();
    write_expression(&directory.path().join("侦探.exp3.json"), 1.0);
    let model = parse_model("[]");
    let resolver = directory.resolver();
    let external = resolver.discover_external_expressions();
    let (mut controller, diagnostics) =
        ExpressionController::load_manifest_with_external(&model, &resolver, &external);

    assert!(diagnostics.entries().is_empty());
    assert!(controller.play("侦探"));
    assert_eq!(controller.manager.active_expression_count(), 1);
    assert!(controller.reset_to_default());
    assert!(controller.manager.is_empty());
}

#[test]
fn external_default_expression_is_not_used_as_the_manifest_default() {
    let directory = TestDirectory::new();
    write_expression(&directory.path().join("Default.exp3.json"), 1.0);
    let model = parse_model("[]");
    let resolver = directory.resolver();
    let external = resolver.discover_external_expressions();
    let (mut controller, diagnostics) =
        ExpressionController::load_manifest_with_external(&model, &resolver, &external);

    assert!(diagnostics.entries().is_empty());
    assert!(controller.manager.is_empty());
    assert!(controller.play("Default"));
    assert!(controller.reset_to_default());
    assert!(controller.manager.is_empty());
}

#[test]
fn later_successful_duplicate_overrides_but_later_failure_keeps_success() {
    let directory = TestDirectory::new();
    write_expression(&directory.path().join("first.exp3.json"), 1.0);
    write_expression(&directory.path().join("second.exp3.json"), 2.0);
    let model = parse_model(
        r#"[
                {"Name":"Smile","File":"first.exp3.json"},
                {"Name":"Smile","File":"second.exp3.json"},
                {"Name":"Smile","File":"missing.exp3.json"}
            ]"#,
    );

    let (mut controller, diagnostics) =
        ExpressionController::load_manifest(&model, &directory.resolver());

    assert_eq!(controller.loaded_expression_count(), 1);
    assert_eq!(controller.first_parameter_value("Smile"), Some(2.0));
    assert!(controller.play("Smile"));
    assert_eq!(
        category_count(&diagnostics, ModelDiagnosticCategory::DuplicateName),
        1
    );
    assert_eq!(
        category_count(&diagnostics, ModelDiagnosticCategory::Missing),
        1
    );
}

#[test]
fn expression_limit_keeps_prefix_and_reports_one_aggregate_diagnostic() {
    let directory = TestDirectory::new();
    write_expression(&directory.path().join("valid.exp3.json"), 1.0);
    let references = (0..MAX_EXPRESSION_COUNT + 2)
        .map(|index| format!(r#"{{"Name":"Expression{index}","File":"valid.exp3.json"}}"#))
        .collect::<Vec<_>>()
        .join(",");
    let model = parse_model(&format!("[{references}]"));

    let (controller, diagnostics) =
        ExpressionController::load_manifest(&model, &directory.resolver());

    assert_eq!(controller.loaded_expression_count(), MAX_EXPRESSION_COUNT);
    assert_eq!(
        category_count(&diagnostics, ModelDiagnosticCategory::LimitExceeded),
        1
    );
    let limit = diagnostics
        .entries()
        .iter()
        .find(|diagnostic| diagnostic.category() == ModelDiagnosticCategory::LimitExceeded)
        .expect("表情超限应当生成聚合诊断");
    assert_eq!(limit.name(), Some("Expression128"));
    assert_eq!(limit.declaration_index(), Some(MAX_EXPRESSION_COUNT));
    assert_eq!(limit.affected_count(), 2);
}

#[test]
fn excessive_fade_duration_is_skipped_as_an_invalid_expression() {
    let directory = TestDirectory::new();
    write_expression_with_fades(&directory.path().join("slow.exp3.json"), 61.0, 1.0);
    let model = parse_model(r#"[{"Name":"Slow","File":"slow.exp3.json"}]"#);

    let (controller, diagnostics) =
        ExpressionController::load_manifest(&model, &directory.resolver());

    assert_eq!(controller.loaded_expression_count(), 0);
    assert_eq!(
        category_count(&diagnostics, ModelDiagnosticCategory::InvalidDuration),
        1
    );
}

#[test]
fn repeated_replacements_keep_the_expression_stack_bounded() {
    let directory = TestDirectory::new();
    write_expression(&directory.path().join("default.exp3.json"), 1.0);
    let model = parse_model(r#"[{"Name":"Default","File":"default.exp3.json"}]"#);
    let (mut controller, diagnostics) =
        ExpressionController::load_manifest(&model, &directory.resolver());

    for _ in 0..32 {
        assert!(controller.play("Default"));
    }

    assert!(diagnostics.entries().is_empty());
    assert!(controller.manager.active_expression_count() <= 8);
}

#[test]
fn expression_transition_only_requests_frames_until_fade_in_finishes() {
    let directory = TestDirectory::new();
    write_expression(&directory.path().join("default.exp3.json"), 1.0);
    let model = parse_model(r#"[{"Name":"Default","File":"default.exp3.json"}]"#);
    let (mut controller, diagnostics) =
        ExpressionController::load_manifest(&model, &directory.resolver());

    assert!(diagnostics.entries().is_empty());
    assert!(controller.needs_continuous_frames());
    controller.tick(0.5);
    assert!(controller.needs_continuous_frames());
    controller.tick(0.5);
    assert!(!controller.needs_continuous_frames());
}

#[test]
fn replacement_keeps_requesting_frames_until_the_old_fade_out_finishes() {
    let directory = TestDirectory::new();
    write_expression_with_fades(&directory.path().join("default.exp3.json"), 0.1, 1.0);
    write_expression_with_fades(&directory.path().join("next.exp3.json"), 0.1, 0.1);
    let model = parse_model(
        r#"[
                {"Name":"Default","File":"default.exp3.json"},
                {"Name":"Next","File":"next.exp3.json"}
            ]"#,
    );
    let (mut controller, diagnostics) =
        ExpressionController::load_manifest(&model, &directory.resolver());
    controller.tick(0.1);
    assert!(!controller.needs_continuous_frames());

    assert!(controller.play("Next"));
    controller.tick(0.1);

    assert_eq!(controller.manager.active_expression_count(), 2);
    assert!(controller.needs_continuous_frames());
    controller.tick(1.0);
    assert_eq!(controller.manager.active_expression_count(), 1);
    assert!(!controller.needs_continuous_frames());
    assert!(diagnostics.entries().is_empty());
}

#[test]
fn controller_without_expression_does_not_request_continuous_frames() {
    let directory = TestDirectory::new();
    let model = parse_model("[]");
    let (controller, diagnostics) =
        ExpressionController::load_manifest(&model, &directory.resolver());

    assert!(diagnostics.entries().is_empty());
    assert!(!controller.needs_continuous_frames());
}
