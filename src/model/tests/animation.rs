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
    write_motion_with_loop(path, duration, false);
}

fn write_motion_with_loop(path: &Path, duration: f32, looping: bool) {
    fs::write(
        path,
        format!(
            r#"{{"Version":3,"Meta":{{"Duration":{duration},"Fps":60,"Loop":{looping}}},"Curves":[]}}"#
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
fn available_groups_only_report_groups_with_usable_clips() {
    let directory = TestDirectory::new();
    write_motion(&directory.path().join("idle.motion3.json"), 1.0);
    write_motion(&directory.path().join("zero.motion3.json"), 0.0);
    let model = parse_model(
        r#"{
                "Idle":[{"File":"idle.motion3.json"}],
                "Tap":[{"File":"idle.motion3.json"}],
                "Broken":[{"File":"zero.motion3.json"}],
                "Empty":[]
            }"#,
    );

    let (controller, _diagnostics) =
        AnimationController::load_manifest(&model, &directory.resolver());

    // 名称按 BTreeMap 顺序返回，只暴露真正可播放的动作组。
    let resources = controller.available_resources();
    assert_eq!(
        resources
            .iter()
            .map(|resource| resource.runtime_id())
            .collect::<Vec<_>>(),
        ["Idle", "Tap"]
    );
}

#[test]
fn looping_external_motions_load_as_idle_motions() {
    let directory = TestDirectory::new();
    fs::create_dir(directory.path().join("motions")).expect("动作专属目录应当可以创建");
    write_motion(&directory.path().join("declared.motion3.json"), 1.0);
    write_motion_with_loop(&directory.path().join("wave.motion3.json"), 1.0, true);
    write_motion(&directory.path().join("motions/dance.motion3.json"), 1.0);
    let model = parse_model(r#"{"Tap":[{"File":"declared.motion3.json"}]}"#);

    let (mut controller, diagnostics) =
        AnimationController::load_manifest(&model, &directory.resolver());

    assert!(diagnostics.entries().is_empty());
    let resources = controller.available_resources();
    assert_eq!(
        resources
            .iter()
            .map(|resource| (resource.runtime_id(), resource.default_name()))
            .collect::<Vec<_>>(),
        [
            ("Tap", "Tap"),
            ("external:motions/dance.motion3.json", "dance"),
            ("external:wave.motion3.json", "wave"),
        ]
    );
    assert_eq!(
        controller.play_interaction("external:wave.motion3.json"),
        MotionPlayResult::Started
    );
    assert_eq!(controller.active_is_looping(), Some(true));
    assert!(
        resources
            .iter()
            .find(|resource| resource.runtime_id() == "external:wave.motion3.json")
            .is_some_and(|resource| resource.is_idle())
    );
    assert!(
        resources
            .iter()
            .find(|resource| resource.runtime_id() == "external:motions/dance.motion3.json")
            .is_some_and(|resource| !resource.is_idle())
    );
}

#[test]
fn vts_version_zero_looping_external_motion_loads_as_idle_motion() {
    let directory = TestDirectory::new();
    fs::write(
        directory.path().join("standby.motion3.json"),
        r#"{
            "Version": 0,
            "Meta": {
                "Duration": 29.986647,
                "Fps": 60,
                "Loop": true,
                "CurveCount": 1,
                "TotalSegmentCount": 1,
                "TotalPointCount": 2,
                "UserDataCount": 0,
                "TotalUserDataSize": 0
            },
            "Curves": [{
                "Target": "Parameter",
                "Id": "ParamAngleX",
                "Segments": [0, 0, 0, 29.986647, 1]
            }]
        }"#,
    )
    .expect("VTS Version 0 测试动作应当可以创建");
    let model = parse_model("{}");

    let (mut controller, diagnostics) =
        AnimationController::load_manifest(&model, &directory.resolver());

    assert!(diagnostics.entries().is_empty());
    assert_eq!(
        controller
            .available_resources()
            .iter()
            .map(|resource| (resource.runtime_id(), resource.default_name()))
            .collect::<Vec<_>>(),
        [("external:standby.motion3.json", "standby")]
    );
    assert_eq!(
        controller.play_interaction("external:standby.motion3.json"),
        MotionPlayResult::Started
    );
    assert_eq!(controller.active_is_looping(), Some(true));
    assert_eq!(
        controller.active_group_for_test(),
        Some("external:standby.motion3.json")
    );
}

#[test]
fn manifest_idle_has_priority_over_named_and_ordered_external_idle_motions() {
    let directory = TestDirectory::new();
    write_motion(&directory.path().join("declared.motion3.json"), 1.0);
    write_motion_with_loop(&directory.path().join("first.motion3.json"), 1.0, true);
    write_motion_with_loop(&directory.path().join("idle.motion3.json"), 1.0, true);
    let model = parse_model(r#"{"Idle":[{"File":"declared.motion3.json"}]}"#);

    let (controller, diagnostics) =
        AnimationController::load_manifest(&model, &directory.resolver());

    assert!(diagnostics.entries().is_empty());
    assert_eq!(controller.active_group_for_test(), Some("Idle"));
}

#[test]
fn named_external_idle_has_priority_over_other_looping_external_motions() {
    let directory = TestDirectory::new();
    write_motion_with_loop(&directory.path().join("first.motion3.json"), 1.0, true);
    write_motion_with_loop(&directory.path().join("IDLE.motion3.json"), 1.0, true);
    let model = parse_model("{}");

    let (controller, diagnostics) =
        AnimationController::load_manifest(&model, &directory.resolver());

    assert!(diagnostics.entries().is_empty());
    assert_eq!(
        controller.active_group_for_test(),
        Some("external:IDLE.motion3.json")
    );
}

#[test]
fn first_looping_external_motion_is_the_last_idle_fallback() {
    let directory = TestDirectory::new();
    write_motion(&directory.path().join("idle.motion3.json"), 1.0);
    write_motion_with_loop(&directory.path().join("alpha.motion3.json"), 1.0, true);
    write_motion_with_loop(&directory.path().join("zeta.motion3.json"), 1.0, true);
    let model = parse_model("{}");

    let (controller, diagnostics) =
        AnimationController::load_manifest(&model, &directory.resolver());

    assert!(diagnostics.entries().is_empty());
    assert_eq!(
        controller.active_group_for_test(),
        Some("external:alpha.motion3.json")
    );
}

#[test]
fn same_stem_external_motions_keep_distinct_runtime_ids() {
    let directory = TestDirectory::new();
    fs::create_dir(directory.path().join("motions")).expect("动作专属目录应当可以创建");
    write_motion(&directory.path().join("wave.motion3.json"), 1.0);
    write_motion(&directory.path().join("motions/wave.motion3.json"), 1.0);
    let model = parse_model("{}");

    let (controller, diagnostics) =
        AnimationController::load_manifest(&model, &directory.resolver());

    assert!(diagnostics.entries().is_empty());
    assert_eq!(controller.available_resources().len(), 2);
    assert_eq!(
        controller
            .available_resources()
            .iter()
            .map(|resource| resource.default_name())
            .collect::<Vec<_>>(),
        ["wave", "wave"]
    );
}

#[test]
fn manifest_group_wins_when_it_matches_an_external_runtime_id() {
    let directory = TestDirectory::new();
    write_motion(&directory.path().join("declared.motion3.json"), 1.0);
    write_motion(&directory.path().join("wave.motion3.json"), 1.0);
    let model = parse_model(r#"{"external:wave.motion3.json":[{"File":"declared.motion3.json"}]}"#);

    let (controller, diagnostics) =
        AnimationController::load_manifest(&model, &directory.resolver());

    assert!(diagnostics.entries().is_empty());
    assert_eq!(
        controller
            .available_resources()
            .iter()
            .map(|resource| resource.runtime_id())
            .collect::<Vec<_>>(),
        ["external:wave.motion3.json", "external:wave.motion3.json#2"]
    );
}

#[test]
fn cancellation_before_loading_skips_every_motion_declaration() {
    let directory = TestDirectory::new();
    write_motion(&directory.path().join("idle.motion3.json"), 1.0);
    let model = parse_model(
        r#"{
                "Idle":[{"File":"idle.motion3.json"}],
                "Tap":[{"File":"idle.motion3.json"}]
            }"#,
    );
    let cancellation = RenderCancellation::default();
    cancellation.cancel();
    let mut budget = AuxiliaryResourceBudget::default();

    let (mut controller, diagnostics) = AnimationController::load_manifest_with_resources(
        &model,
        &directory.resolver(),
        &mut budget,
        &cancellation,
    );

    assert!(diagnostics.entries().is_empty());
    assert_eq!(controller.loaded_motion_count("Idle"), Some(0));
    assert!(controller.available_resources().is_empty());
    assert!(!controller.needs_continuous_frames());
    assert_eq!(
        controller.play_interaction("Idle"),
        MotionPlayResult::InvalidMotion
    );
}

#[test]
fn motions_outside_the_model_directory_are_rejected_before_reading() {
    let directory = TestDirectory::new();
    let outside = directory.path().join("outside.motion3.json");
    write_motion(&outside, 1.0);
    let nested = directory.path().join("runtime");
    fs::create_dir_all(&nested).expect("测试子目录应当可以创建");
    let resolver = ModelResourceResolver::for_manifest(&nested.join("model.model3.json"))
        .expect("测试模型目录应当可以解析");
    let model = parse_model(
        r#"{"Tap":[
                {"File":"../outside.motion3.json"},
                {"File":"/etc/hostname"}
            ]}"#,
    );

    let (controller, diagnostics) = AnimationController::load_manifest(&model, &resolver);

    assert_eq!(controller.loaded_motion_count("Tap"), Some(0));
    assert_eq!(
        category_count(&diagnostics, ModelDiagnosticCategory::InvalidReference),
        2
    );
}

#[test]
fn directories_declared_as_motions_are_reported_without_panicking() {
    let directory = TestDirectory::new();
    fs::create_dir_all(directory.path().join("group.motion3.json"))
        .expect("同名测试目录应当可以创建");
    let model = parse_model(r#"{"Tap":[{"File":"group.motion3.json"}]}"#);

    let (controller, diagnostics) =
        AnimationController::load_manifest(&model, &directory.resolver());

    assert_eq!(controller.loaded_motion_count("Tap"), Some(0));
    assert_eq!(
        category_count(&diagnostics, ModelDiagnosticCategory::NotFile),
        1
    );
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
