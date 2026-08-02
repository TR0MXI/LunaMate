//! 管理 Live2D 动作资源、默认动作选择与逐帧播放状态。
//!
//! 上层只通过本模块暴露的控制器驱动动作，不直接依赖 Mocari 的播放器细节。

use std::{
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
};

use mocari::{
    ModelRuntime,
    assets::RuntimeModel,
    json::{Model3, Motion3},
    motion::MotionPlayer,
};

use super::{
    capabilities::{
        AuxiliaryResourceBudget, MAX_AUXILIARY_RESOURCE_BYTES, ModelDiagnosticCategory,
        ModelLoadDiagnostic, ModelLoadDiagnostics, ModelResourceResolver,
    },
    live2d::{ModelPreviewResource, RenderCancellation},
};

const DEFAULT_MOTION_GROUP: &str = "Idle";
pub(in crate::model) const MAX_MOTION_COUNT: usize = 256;

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
    default_name: String,
    idle: bool,
    motions: Vec<MotionPlayer>,
    next_index: usize,
}

#[derive(Clone)]
struct CachedMotion {
    canonical_path: Option<std::path::PathBuf>,
    result: Result<Arc<Motion3>, CachedMotionError>,
}

#[derive(Clone)]
struct CachedMotionError {
    category: ModelDiagnosticCategory,
    message: String,
}

/// 保存模型声明的动作，并负责当前动作的播放与应用。
pub(crate) struct AnimationController {
    group_indices: BTreeMap<String, usize>,
    groups: Vec<MotionGroup>,
    default_idle_group: Option<usize>,
    active: Option<ActiveMotion>,
    settling: bool,
}

impl AnimationController {
    /// 从已加载模型逐项解析动作；坏项只生成诊断，并按待机优先级启动首个有效动作。
    #[cfg(test)]
    pub(in crate::model) fn load(
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
    pub(in crate::model) fn load_manifest(
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

    pub(in crate::model) fn load_manifest_with_resources(
        model: &Model3,
        resolver: &ModelResourceResolver,
        budget: &mut AuxiliaryResourceBudget,
        cancellation: &RenderCancellation,
    ) -> (AnimationController, ModelLoadDiagnostics) {
        let references = model.motions();
        let mut group_indices = references
            .iter()
            .enumerate()
            .map(|(index, (group, _))| (group.clone(), index))
            .collect::<BTreeMap<String, usize>>();
        let mut groups = references
            .iter()
            .map(|(group, references)| MotionGroup {
                declared_count: references.len(),
                default_name: group.clone(),
                idle: group == DEFAULT_MOTION_GROUP,
                motions: Vec::new(),
                next_index: 0,
            })
            .collect::<Vec<_>>();
        let declared_count = references
            .values()
            .fold(0_usize, |count, group| count.saturating_add(group.len()));
        let mut diagnostics = ModelLoadDiagnostics::default();
        let mut declared_files = BTreeSet::new();
        let mut motion_cache = BTreeMap::<String, CachedMotion>::new();
        let mut processed_count = 0_usize;
        let mut reported_limit = false;
        let mut resource_budget_exhausted = false;
        let mut named_external_idle = None;
        let mut first_external_idle = None;

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
                                    "动作声明与外部动作总数为 {declared_count}，仅处理前 {MAX_MOTION_COUNT} 项"
                                ),
                            )
                            .with_affected_count(
                                declared_count.saturating_sub(MAX_MOTION_COUNT),
                            ),
                        );
                        reported_limit = true;
                    }
                    break 'groups;
                }
                processed_count += 1;

                let cached = match motion_cache.get(reference.file()).cloned() {
                    Some(cached) => cached,
                    None => {
                        let Some(cached) =
                            load_cached_motion(reference.file(), resolver, budget, cancellation)
                        else {
                            break 'groups;
                        };
                        motion_cache.insert(reference.file().to_owned(), cached.clone());
                        cached
                    }
                };
                if let Some(path) = cached.canonical_path {
                    declared_files.insert(path);
                }
                let motion = match cached.result {
                    Ok(motion) => motion,
                    Err(error) => {
                        budget_exhausted = error.category == ModelDiagnosticCategory::LimitExceeded;
                        resource_budget_exhausted |= budget_exhausted;
                        diagnostics.push(ModelLoadDiagnostic::motion(
                            group,
                            index,
                            reference.file(),
                            error.category,
                            error.message,
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
                clips.push(MotionPlayer::with_looping(
                    motion,
                    group == DEFAULT_MOTION_GROUP,
                ));
            }
            if budget_exhausted {
                break;
            }
        }

        let mut external = Vec::new();
        if !cancellation.is_cancelled()
            && !resource_budget_exhausted
            && declared_count < MAX_MOTION_COUNT
        {
            for reference in
                resolver.discover_external_motions_with_checkpoint(|| cancellation.is_cancelled())
            {
                if cancellation.is_cancelled() {
                    break;
                }
                if !declared_files.contains(reference.canonical_path()) {
                    external.push(reference);
                }
            }
        }
        let total_count = declared_count.saturating_add(external.len());
        let omitted_count = total_count.saturating_sub(MAX_MOTION_COUNT);

        if !cancellation.is_cancelled() && !resource_budget_exhausted {
            for reference in external {
                if cancellation.is_cancelled() {
                    break;
                }
                if processed_count >= MAX_MOTION_COUNT {
                    if !reported_limit {
                        diagnostics.push(
                            ModelLoadDiagnostic::motion(
                                reference.name(),
                                0,
                                reference.reference(),
                                ModelDiagnosticCategory::LimitExceeded,
                                format!(
                                    "动作声明与外部动作总数为 {total_count}，仅处理前 {MAX_MOTION_COUNT} 项"
                                ),
                            )
                            .with_affected_count(omitted_count),
                        );
                        reported_limit = true;
                    }
                    continue;
                }
                processed_count += 1;
                let runtime_id = unique_external_runtime_id(reference.runtime_id(), &group_indices);
                let group_index = groups.len();
                group_indices.insert(runtime_id.clone(), group_index);
                groups.push(MotionGroup {
                    declared_count: 1,
                    default_name: reference.name().to_owned(),
                    idle: false,
                    motions: Vec::new(),
                    next_index: 0,
                });

                let source = match resolver.read_text_with_budget_and_checkpoint(
                    reference.reference(),
                    MAX_AUXILIARY_RESOURCE_BYTES,
                    budget,
                    || cancellation.is_cancelled(),
                ) {
                    Ok(source) => source,
                    Err(error) => {
                        if cancellation.is_cancelled() {
                            break;
                        }
                        diagnostics.push(ModelLoadDiagnostic::motion(
                            reference.name(),
                            0,
                            reference.reference(),
                            error.category(),
                            error.message(),
                        ));
                        if error.category() == ModelDiagnosticCategory::LimitExceeded {
                            break;
                        }
                        continue;
                    }
                };
                if cancellation.is_cancelled() {
                    break;
                }
                let motion = match Motion3::from_json_str(&source) {
                    Ok(motion) => motion,
                    Err(error) => {
                        diagnostics.push(ModelLoadDiagnostic::motion(
                            reference.name(),
                            0,
                            reference.reference(),
                            ModelDiagnosticCategory::Parse,
                            format!("动作 JSON 内容无效或版本不受支持：{error}"),
                        ));
                        continue;
                    }
                };
                if cancellation.is_cancelled() {
                    break;
                }
                let duration = motion.meta().duration();
                if !duration.is_finite() || duration <= 0.0 {
                    diagnostics.push(ModelLoadDiagnostic::motion(
                        reference.name(),
                        0,
                        reference.reference(),
                        ModelDiagnosticCategory::InvalidDuration,
                        format!("动作时长必须是有限正数，当前值为 {duration}"),
                    ));
                    continue;
                }
                let looping = motion.meta().is_looping();
                groups[group_index].idle = looping;
                groups[group_index]
                    .motions
                    .push(MotionPlayer::with_looping(motion, looping));
                if looping {
                    first_external_idle.get_or_insert(group_index);
                    if reference.name().eq_ignore_ascii_case("idle") {
                        named_external_idle.get_or_insert(group_index);
                    }
                }
            }
        }

        let manifest_idle = group_indices
            .get(DEFAULT_MOTION_GROUP)
            .copied()
            .filter(|index| !groups[*index].motions.is_empty());
        let mut controller = Self {
            group_indices,
            groups,
            default_idle_group: manifest_idle
                .or(named_external_idle)
                .or(first_external_idle),
            active: None,
            settling: false,
        };
        controller.start_idle();
        (controller, diagnostics)
    }

    /// 播放指定动作组，并在组内轮换可用动作。
    pub(crate) fn play_interaction(&mut self, group: &str) -> MotionPlayResult {
        self.start_next(group)
    }

    /// 返回至少包含一个成功加载动作的稳定 ID 与默认显示名。
    pub(crate) fn available_resources(&self) -> Vec<ModelPreviewResource> {
        self.group_indices
            .iter()
            .filter(|(_, index)| !self.groups[**index].motions.is_empty())
            .map(|(runtime_id, index)| {
                ModelPreviewResource::new(
                    runtime_id.clone(),
                    self.groups[*index].default_name.clone(),
                    self.groups[*index].idle,
                )
            })
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
        if let Some(group_index) = self.default_idle_group {
            let _ = self.start_next_by_index(group_index);
        }
    }

    fn start_next(&mut self, group: &str) -> MotionPlayResult {
        let Some(group_index) = self.group_indices.get(group).copied() else {
            return MotionPlayResult::MissingGroup;
        };
        self.start_next_by_index(group_index)
    }

    fn start_next_by_index(&mut self, group_index: usize) -> MotionPlayResult {
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
    pub(in crate::model) fn active_is_looping(&self) -> Option<bool> {
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
    pub(in crate::model) fn active_group_for_test(&self) -> Option<&str> {
        let group_index = self.active.as_ref()?.group_index;
        self.group_indices
            .iter()
            .find_map(|(runtime_id, index)| (*index == group_index).then_some(runtime_id.as_str()))
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
    pub(in crate::model) fn finish_active_for_test(&mut self, runtime: &mut ModelRuntime) {
        if let Some(duration) = self.active_duration() {
            self.update(runtime, duration + 0.001);
        }
    }

    #[cfg(test)]
    pub(in crate::model) fn loaded_motion_count(&self, group: &str) -> Option<usize> {
        self.group_indices
            .get(group)
            .map(|index| self.groups[*index].motions.len())
    }

    #[cfg(test)]
    pub(in crate::model) fn parsed_motion_identities_for_test(
        &self,
        group: &str,
    ) -> Option<Vec<*const Motion3>> {
        self.group_indices.get(group).map(|index| {
            self.groups[*index]
                .motions
                .iter()
                .map(|player| std::ptr::from_ref(player.motion()))
                .collect()
        })
    }
}

fn load_cached_motion(
    reference: &str,
    resolver: &ModelResourceResolver,
    budget: &mut AuxiliaryResourceBudget,
    cancellation: &RenderCancellation,
) -> Option<CachedMotion> {
    if cancellation.is_cancelled() {
        return None;
    }
    let (canonical_path, source) = match resolver.read_text_with_path_and_budget_and_checkpoint(
        reference,
        MAX_AUXILIARY_RESOURCE_BYTES,
        budget,
        || cancellation.is_cancelled(),
    ) {
        Ok(resource) => resource,
        Err(_) if cancellation.is_cancelled() => return None,
        Err(error) => {
            return Some(CachedMotion {
                canonical_path: None,
                result: Err(CachedMotionError {
                    category: error.category(),
                    message: error.message().to_owned(),
                }),
            });
        }
    };
    if cancellation.is_cancelled() {
        return None;
    }
    let result = Motion3::from_json_str(&source)
        .map_err(|error| CachedMotionError {
            category: ModelDiagnosticCategory::Parse,
            message: format!("动作 JSON 内容无效或版本不受支持：{error}"),
        })
        .and_then(|motion| {
            let duration = motion.meta().duration();
            if duration.is_finite() && duration > 0.0 {
                Ok(Arc::new(motion))
            } else {
                Err(CachedMotionError {
                    category: ModelDiagnosticCategory::InvalidDuration,
                    message: format!("动作时长必须是有限正数，当前值为 {duration}"),
                })
            }
        });
    if cancellation.is_cancelled() {
        return None;
    }
    Some(CachedMotion {
        canonical_path: Some(canonical_path),
        result,
    })
}

fn unique_external_runtime_id(base: String, occupied: &BTreeMap<String, usize>) -> String {
    if !occupied.contains_key(&base) {
        return base;
    }
    for suffix in 2_u32.. {
        let candidate = format!("{base}#{suffix}");
        if !occupied.contains_key(&candidate) {
            return candidate;
        }
    }
    unreachable!("无界递增后缀必须能生成唯一动作 ID")
}
