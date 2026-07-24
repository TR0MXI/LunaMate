use crate::model::{
    animation::{AnimationController, MAX_MOTION_COUNT, MotionPlayResult},
    capabilities::{
        AuxiliaryResourceBudget, ModelDiagnosticCategory, ModelLoadDiagnostics,
        ModelResourceResolver,
    },
    live2d::RenderCancellation,
};

use std::{
    fs,
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

use mocari::{assets::load_model_runtime, json::Model3};

struct TestDirectory(PathBuf);

impl TestDirectory {
    fn new() -> Self {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("系统时间必须晚于 Unix 纪元")
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "lunamate-animation-controller-{}-{nonce}",
            std::process::id()
        ));
        fs::create_dir_all(&path).expect("测试动作目录应当可以创建");
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

fn write_motion(path: &Path, duration: f32) {
    fs::write(
        path,
        format!(
            r#"{{"Version":3,"Meta":{{"Duration":{duration},"Fps":30,"Loop":false}},"Curves":[]}}"#
        ),
    )
    .expect("测试动作应当可以创建");
}

fn parse_model(motions: &str) -> Model3 {
    Model3::from_json_str(&format!(
            r#"{{"Version":3,"FileReferences":{{"Moc":"model.moc3","Textures":[],"Motions":{motions}}}}}"#
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
fn mixed_motion_failures_keep_successful_clips() {
    let directory = TestDirectory::new();
    write_motion(&directory.path().join("valid.motion3.json"), 1.5);
    write_motion(&directory.path().join("zero.motion3.json"), 0.0);
    fs::write(directory.path().join("malformed.motion3.json"), "{")
        .expect("损坏测试动作应当可以创建");
    let model = parse_model(
        r#"{"Tap":[
                {"File":"valid.motion3.json"},
                {"File":"missing.motion3.json"},
                {"File":"malformed.motion3.json"},
                {"File":"zero.motion3.json"}
            ]}"#,
    );

    let (mut controller, diagnostics) =
        AnimationController::load_manifest(&model, &directory.resolver());

    assert_eq!(controller.loaded_motion_count("Tap"), Some(1));
    assert_eq!(
        category_count(&diagnostics, ModelDiagnosticCategory::Missing),
        1
    );
    assert_eq!(
        category_count(&diagnostics, ModelDiagnosticCategory::Parse),
        1
    );
    assert_eq!(
        category_count(&diagnostics, ModelDiagnosticCategory::InvalidDuration),
        1
    );
    let missing = diagnostics
        .entries()
        .iter()
        .find(|diagnostic| diagnostic.category() == ModelDiagnosticCategory::Missing)
        .expect("缺失动作应当生成诊断");
    assert_eq!(missing.group(), Some("Tap"));
    assert_eq!(missing.declaration_index(), Some(1));
    assert_eq!(missing.reference(), Some("missing.motion3.json"));
    assert_eq!(
        controller.play_interaction("Tap"),
        MotionPlayResult::Started
    );
}

#[test]
fn declared_group_with_only_bad_motions_is_invalid_instead_of_missing() {
    let directory = TestDirectory::new();
    fs::write(directory.path().join("malformed.motion3.json"), "not-json")
        .expect("损坏测试动作应当可以创建");
    let model = parse_model(
        r#"{
                "Tap":[
                    {"File":"malformed.motion3.json"},
                    {"File":"../outside.motion3.json"}
                ],
                "Empty": []
            }"#,
    );

    let (mut controller, diagnostics) =
        AnimationController::load_manifest(&model, &directory.resolver());

    assert_eq!(controller.loaded_motion_count("Tap"), Some(0));
    assert_eq!(
        category_count(&diagnostics, ModelDiagnosticCategory::Parse),
        1
    );
    assert_eq!(
        category_count(&diagnostics, ModelDiagnosticCategory::InvalidReference),
        1
    );
    assert_eq!(
        controller.play_interaction("Tap"),
        MotionPlayResult::InvalidMotion
    );
    assert_eq!(
        controller.play_interaction("Empty"),
        MotionPlayResult::MissingGroup
    );
    assert_eq!(
        controller.play_interaction("Absent"),
        MotionPlayResult::MissingGroup
    );
}

#[test]
fn motion_limit_keeps_prefix_and_reports_one_aggregate_diagnostic() {
    let directory = TestDirectory::new();
    write_motion(&directory.path().join("valid.motion3.json"), 1.0);
    let references = (0..MAX_MOTION_COUNT + 3)
        .map(|_| r#"{"File":"valid.motion3.json"}"#)
        .collect::<Vec<_>>()
        .join(",");
    let model = parse_model(&format!(r#"{{"Tap":[{references}]}}"#));

    let (mut controller, diagnostics) =
        AnimationController::load_manifest(&model, &directory.resolver());

    assert_eq!(
        controller.loaded_motion_count("Tap"),
        Some(MAX_MOTION_COUNT)
    );
    assert_eq!(
        category_count(&diagnostics, ModelDiagnosticCategory::LimitExceeded),
        1
    );
    let limit = diagnostics
        .entries()
        .iter()
        .find(|diagnostic| diagnostic.category() == ModelDiagnosticCategory::LimitExceeded)
        .expect("动作超限应当生成聚合诊断");
    assert_eq!(limit.group(), Some("Tap"));
    assert_eq!(limit.declaration_index(), Some(MAX_MOTION_COUNT));
    assert_eq!(limit.affected_count(), 3);
    assert_eq!(
        controller.play_interaction("Tap"),
        MotionPlayResult::Started
    );
}

#[test]
fn exhausted_byte_budget_keeps_successes_from_the_current_group() {
    let directory = TestDirectory::new();
    let first_path = directory.path().join("first.motion3.json");
    write_motion(&first_path, 1.0);
    write_motion(&directory.path().join("second.motion3.json"), 1.0);
    let model = parse_model(
        r#"{"Tap":[
                {"File":"first.motion3.json"},
                {"File":"second.motion3.json"}
            ]}"#,
    );
    let first_size = fs::metadata(first_path)
        .expect("首个测试动作元数据应当可以读取")
        .len();
    let mut budget = AuxiliaryResourceBudget::with_limit(first_size);

    let (controller, diagnostics) = AnimationController::load_manifest_with_resources(
        &model,
        &directory.resolver(),
        &mut budget,
        &RenderCancellation::default(),
    );

    assert_eq!(controller.loaded_motion_count("Tap"), Some(1));
    assert_eq!(
        category_count(&diagnostics, ModelDiagnosticCategory::LimitExceeded),
        1
    );
}

#[test]
fn controller_without_motion_does_not_request_continuous_frames() {
    let directory = TestDirectory::new();
    let model = parse_model("{}");
    let (controller, diagnostics) =
        AnimationController::load_manifest(&model, &directory.resolver());

    assert!(diagnostics.entries().is_empty());
    assert!(!controller.needs_continuous_frames());
}

#[test]
fn looping_idle_and_started_interaction_request_continuous_frames() {
    let directory = TestDirectory::new();
    write_motion(&directory.path().join("idle.motion3.json"), 1.0);
    write_motion(&directory.path().join("tap.motion3.json"), 1.0);
    let model = parse_model(
        r#"{
                "Idle":[{"File":"idle.motion3.json"}],
                "Tap":[{"File":"tap.motion3.json"}]
            }"#,
    );
    let (mut controller, diagnostics) =
        AnimationController::load_manifest(&model, &directory.resolver());

    assert!(diagnostics.entries().is_empty());
    assert!(controller.needs_continuous_frames());
    assert_eq!(
        controller.play_interaction("Tap"),
        MotionPlayResult::Started
    );
    assert!(controller.needs_continuous_frames());
}

#[test]
#[ignore = "需要本地授权的 Hiyori 模型；提交最小 fixture 后应移除此标记"]
fn interaction_motion_plays_once_and_restores_idle_when_local_model_is_available() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("models/hiyori_free/runtime/hiyori_free_t08.model3.json");

    let mut model = load_model_runtime(&path).expect("本地测试模型应当可以加载");
    let resolver =
        ModelResourceResolver::for_manifest(&path).expect("本地测试模型目录应当可以解析");
    let (mut controller, _diagnostics) = AnimationController::load(&model, &resolver);
    assert_eq!(controller.active_is_looping(), Some(true));

    assert_eq!(
        controller.play_interaction("Tap@Body"),
        MotionPlayResult::Started
    );
    assert_eq!(controller.active_is_looping(), Some(false));

    controller.finish_active_for_test(model.runtime_mut());
    assert_eq!(controller.active_is_looping(), Some(true));
}
