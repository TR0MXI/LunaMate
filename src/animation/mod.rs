//! 管理 Live2D 动作资源、默认动作选择与逐帧播放状态。
//!
//! 上层只通过本模块暴露的控制器驱动动作，不直接依赖 Mocari 的播放器细节。

use std::collections::BTreeMap;

use mocari::{
    ModelRuntime,
    assets::RuntimeModel,
    json::{Model3, Motion3},
    motion::MotionPlayer,
};

use crate::capabilities::{
    AuxiliaryResourceBudget, MAX_AUXILIARY_RESOURCE_BYTES, ModelDiagnosticCategory,
    ModelLoadDiagnostic, ModelLoadDiagnostics, ModelResourceResolver,
};
use crate::live2d_image::RenderCancellation;

const DEFAULT_MOTION_GROUP: &str = "Idle";
const MAX_MOTION_COUNT: usize = 256;

/// 通过已完成主体预检的解析器加载动作，并保留全部逐项诊断。
pub(crate) fn load(
    model: &RuntimeModel,
    resolver: &ModelResourceResolver,
    budget: &mut AuxiliaryResourceBudget,
    cancellation: &RenderCancellation,
) -> (AnimationController, ModelLoadDiagnostics) {
    AnimationController::load_with_resources(model, resolver, budget, cancellation)
}

/// 描述一次动作播放请求的处理结果。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum MotionPlayResult {
    /// 已启动目标动作。
    Started,
    /// 模型没有声明目标动作组或动作组为空。
    MissingGroup,
    /// 动作组存在，但没有持续时间有效的动作。
    InvalidMotion,
}

struct ActiveMotion {
    group_index: usize,
    index: usize,
}

struct MotionGroup {
    declared_count: usize,
    motions: Vec<MotionPlayer>,
    next_index: usize,
}

/// 保存模型声明的动作，并负责当前动作的播放与应用。
pub(crate) struct AnimationController {
    group_indices: BTreeMap<String, usize>,
    groups: Vec<MotionGroup>,
    active: Option<ActiveMotion>,
    settling: bool,
}

impl AnimationController {
    /// 从已加载模型逐项解析动作；坏项只生成诊断，存在 `Idle` 组时自动循环首个成功项。
    #[cfg(test)]
    pub(crate) fn load(
        model: &RuntimeModel,
        resolver: &ModelResourceResolver,
    ) -> (AnimationController, ModelLoadDiagnostics) {
        let mut budget = AuxiliaryResourceBudget::default();
        Self::load_manifest_with_resources(
            model.runtime().model(),
            resolver,
            &mut budget,
            &RenderCancellation::default(),
        )
    }

    /// 使用 generation 共享预算和取消令牌加载全部动作。
    pub(crate) fn load_with_resources(
        model: &RuntimeModel,
        resolver: &ModelResourceResolver,
        budget: &mut AuxiliaryResourceBudget,
        cancellation: &RenderCancellation,
    ) -> (AnimationController, ModelLoadDiagnostics) {
        Self::load_manifest_with_resources(model.runtime().model(), resolver, budget, cancellation)
    }

    #[cfg(test)]
    fn load_manifest(
        model: &Model3,
        resolver: &ModelResourceResolver,
    ) -> (AnimationController, ModelLoadDiagnostics) {
        let mut budget = AuxiliaryResourceBudget::default();
        Self::load_manifest_with_resources(
            model,
            resolver,
            &mut budget,
            &RenderCancellation::default(),
        )
    }

    fn load_manifest_with_resources(
        model: &Model3,
        resolver: &ModelResourceResolver,
        budget: &mut AuxiliaryResourceBudget,
        cancellation: &RenderCancellation,
    ) -> (AnimationController, ModelLoadDiagnostics) {
        let references = model.motions();
        let group_indices = references
            .iter()
            .enumerate()
            .map(|(index, (group, _))| (group.clone(), index))
            .collect();
        let mut groups = references
            .values()
            .map(|references| MotionGroup {
                declared_count: references.len(),
                motions: Vec::new(),
                next_index: 0,
            })
            .collect::<Vec<_>>();
        let total_count = references
            .values()
            .fold(0_usize, |count, group| count.saturating_add(group.len()));
        let omitted_count = total_count.saturating_sub(MAX_MOTION_COUNT);
        let mut diagnostics = ModelLoadDiagnostics::default();
        let mut processed_count = 0_usize;
        let mut reported_limit = false;

        'groups: for (group_index, (group, group_references)) in references.iter().enumerate() {
            if cancellation.is_cancelled() {
                break;
            }
            let remaining_capacity = MAX_MOTION_COUNT.saturating_sub(processed_count);
            let clips = &mut groups[group_index].motions;
            clips.reserve(group_references.len().min(remaining_capacity));
            let mut budget_exhausted = false;
            for (index, reference) in group_references.iter().enumerate() {
                if cancellation.is_cancelled() {
                    break 'groups;
                }
                if processed_count >= MAX_MOTION_COUNT {
                    if !reported_limit {
                        diagnostics.push(
                            ModelLoadDiagnostic::motion(
                                group,
                                index,
                                reference.file(),
                                ModelDiagnosticCategory::LimitExceeded,
                                format!(
                                    "动作声明总数为 {total_count}，仅处理前 {MAX_MOTION_COUNT} 项"
                                ),
                            )
                            .with_affected_count(omitted_count),
                        );
                        reported_limit = true;
                    }
                    continue;
                }
                processed_count += 1;

                let source = match resolver.read_text_with_budget_and_checkpoint(
                    reference.file(),
                    MAX_AUXILIARY_RESOURCE_BYTES,
                    budget,
                    || cancellation.is_cancelled(),
                ) {
                    Ok(source) => source,
                    Err(error) => {
                        if cancellation.is_cancelled() {
                            break 'groups;
                        }
                        budget_exhausted =
                            error.category() == ModelDiagnosticCategory::LimitExceeded;
                        diagnostics.push(ModelLoadDiagnostic::motion(
                            group,
                            index,
                            reference.file(),
                            error.category(),
                            error.message(),
                        ));
                        if budget_exhausted {
                            break;
                        }
                        continue;
                    }
                };
                if cancellation.is_cancelled() {
                    break 'groups;
                }
                let motion = match Motion3::from_json_str(&source) {
                    Ok(motion) => motion,
                    Err(error) => {
                        diagnostics.push(ModelLoadDiagnostic::motion(
                            group,
                            index,
                            reference.file(),
                            ModelDiagnosticCategory::Parse,
                            format!("动作 JSON 内容无效或版本不受支持：{error}"),
                        ));
                        continue;
                    }
                };
                if cancellation.is_cancelled() {
                    break 'groups;
                }
                let duration = motion.meta().duration();
                if !duration.is_finite() || duration <= 0.0 {
                    diagnostics.push(ModelLoadDiagnostic::motion(
                        group,
                        index,
                        reference.file(),
                        ModelDiagnosticCategory::InvalidDuration,
                        format!("动作时长必须是有限正数，当前值为 {duration}"),
                    ));
                    continue;
                }
                clips.push(MotionPlayer::with_looping(
                    motion,
                    group == DEFAULT_MOTION_GROUP,
                ));
            }
            if budget_exhausted {
                break;
            }
        }

        let mut controller = Self {
            group_indices,
            groups,
            active: None,
            settling: false,
        };
        controller.start_idle();
        (controller, diagnostics)
    }

    /// 以一次性动作播放指定交互组，并在组内轮换可用动作。
    pub(crate) fn play_interaction(&mut self, group: &str) -> MotionPlayResult {
        self.start_next(group)
    }

    /// 返回至少包含一个成功加载动作的动作组名称。
    pub(crate) fn available_groups(&self) -> Vec<String> {
        self.group_indices
            .iter()
            .filter(|(_, index)| !self.groups[**index].motions.is_empty())
            .map(|(group, _)| group.clone())
            .collect()
    }

    /// 推进当前动作并把采样结果应用到模型参数。
    pub(crate) fn update(&mut self, runtime: &mut ModelRuntime, delta_seconds: f32) {
        let Some(active) = &self.active else {
            self.settling = false;
            return;
        };
        let player = self
            .groups
            .get_mut(active.group_index)
            .and_then(|group| group.motions.get_mut(active.index))
            .expect("活动动作必须引用控制器持有的播放器");
        player.tick(delta_seconds);
        player.apply(runtime);
        if player.is_finished() {
            self.active = None;
            self.start_idle();
            self.settling = self.active.is_none();
        }
    }

    /// 返回动作是否仍会随时间变化，或是否还需要一帧恢复模型默认参数。
    pub(crate) fn needs_continuous_frames(&self) -> bool {
        self.active.is_some() || self.settling
    }

    fn start_idle(&mut self) {
        let _ = self.start_next(DEFAULT_MOTION_GROUP);
    }

    fn start_next(&mut self, group: &str) -> MotionPlayResult {
        let Some(group_index) = self.group_indices.get(group).copied() else {
            return MotionPlayResult::MissingGroup;
        };
        let group = &mut self.groups[group_index];
        if group.declared_count == 0 {
            return MotionPlayResult::MissingGroup;
        }
        if group.motions.is_empty() {
            return MotionPlayResult::InvalidMotion;
        }

        let start_index = group.next_index % group.motions.len();
        let index = start_index;

        group.motions[index].restart();
        self.settling = false;
        group.next_index = (index + 1) % group.motions.len();
        self.active = Some(ActiveMotion { group_index, index });
        MotionPlayResult::Started
    }

    #[cfg(test)]
    fn active_is_looping(&self) -> Option<bool> {
        self.active
            .as_ref()
            .and_then(|active| {
                self.groups
                    .get(active.group_index)?
                    .motions
                    .get(active.index)
            })
            .map(MotionPlayer::is_looping)
    }

    #[cfg(test)]
    fn active_duration(&self) -> Option<f32> {
        self.active
            .as_ref()
            .and_then(|active| {
                self.groups
                    .get(active.group_index)?
                    .motions
                    .get(active.index)
            })
            .map(|player| player.motion().meta().duration())
    }

    #[cfg(test)]
    fn finish_active_for_test(&mut self, runtime: &mut ModelRuntime) {
        if let Some(duration) = self.active_duration() {
            self.update(runtime, duration + 0.001);
        }
    }

    #[cfg(test)]
    fn loaded_motion_count(&self, group: &str) -> Option<usize> {
        self.group_indices
            .get(group)
            .map(|index| self.groups[*index].motions.len())
    }
}

#[cfg(test)]
mod tests {
    use std::{
        fs,
        path::{Path, PathBuf},
        time::{SystemTime, UNIX_EPOCH},
    };

    use mocari::{assets::load_model_runtime, json::Model3};

    use super::*;

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

    fn category_count(
        diagnostics: &ModelLoadDiagnostics,
        category: ModelDiagnosticCategory,
    ) -> usize {
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
}
