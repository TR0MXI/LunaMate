//! 管理 Live2D 表情资源、切换与淡入淡出。
//!
//! 表情参数只在模型逐帧更新阶段写入，UI 与聊天模块无需了解 Mocari 类型。

use std::{
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
};

use mocari::{
    ModelRuntime,
    assets::RuntimeModel,
    expression::ExpressionManager,
    json::{Expression3, Model3},
};

use super::{
    capabilities::{
        AuxiliaryResourceBudget, ExternalExpressionReference, MAX_AUXILIARY_RESOURCE_BYTES,
        ModelDiagnosticCategory, ModelLoadDiagnostic, ModelLoadDiagnostics, ModelResourceResolver,
    },
    live2d::{ModelPreviewExpression, RenderCancellation},
};

const DEFAULT_EXPRESSION_NAMES: [&str; 2] = ["Default", "Idle"];
pub(in crate::model) const MAX_EXPRESSION_COUNT: usize = 128;
const MAX_EXPRESSION_FADE_SECONDS: f32 = 60.0;

/// 通过已完成主体预检的解析器加载表情，并保留全部逐项诊断。
pub(crate) fn load(
    model: &RuntimeModel,
    resolver: &ModelResourceResolver,
    budget: &mut AuxiliaryResourceBudget,
    cancellation: &RenderCancellation,
) -> (ExpressionController, ModelLoadDiagnostics) {
    ExpressionController::load_with_resources(model, resolver, budget, cancellation)
}

/// 保存模型声明的表情，并负责表情切换和混合。
pub(crate) struct ExpressionController {
    expressions: BTreeMap<String, Arc<Expression3>>,
    resources: BTreeMap<String, ModelPreviewExpression>,
    default_expression: Option<String>,
    pub(in crate::model) manager: ExpressionManager,
}

#[derive(Clone)]
struct CachedExpression {
    canonical_path: Option<std::path::PathBuf>,
    result: Result<Arc<Expression3>, CachedExpressionError>,
}

#[derive(Clone)]
struct CachedExpressionError {
    category: ModelDiagnosticCategory,
    message: String,
}

#[derive(Clone, Copy)]
struct ExpressionDeclaration<'a> {
    name: &'a str,
    index: usize,
    reference: &'a str,
    declared: bool,
}

impl ExpressionController {
    /// 从已加载模型逐项解析表情；坏项只生成诊断，默认表情仅从成功项中选择。
    pub(crate) fn load_with_resources(
        model: &RuntimeModel,
        resolver: &ModelResourceResolver,
        budget: &mut AuxiliaryResourceBudget,
        cancellation: &RenderCancellation,
    ) -> (ExpressionController, ModelLoadDiagnostics) {
        if cancellation.is_cancelled() {
            return Self::empty_load_result();
        }
        let external = if model.runtime().model().expressions().len() < MAX_EXPRESSION_COUNT {
            resolver.discover_external_expressions_with_checkpoint(|| cancellation.is_cancelled())
        } else {
            Vec::new()
        };
        Self::load_manifest_with_resources(
            model.runtime().model(),
            resolver,
            &external,
            budget,
            cancellation,
        )
    }

    #[cfg(test)]
    pub(in crate::model) fn load_manifest(
        model: &Model3,
        resolver: &ModelResourceResolver,
    ) -> (ExpressionController, ModelLoadDiagnostics) {
        let external = if model.expressions().len() < MAX_EXPRESSION_COUNT {
            resolver.discover_external_expressions()
        } else {
            Vec::new()
        };
        Self::load_manifest_with_external(model, resolver, &external)
    }

    #[cfg(test)]
    pub(in crate::model) fn load_manifest_with_external(
        model: &Model3,
        resolver: &ModelResourceResolver,
        external: &[ExternalExpressionReference],
    ) -> (ExpressionController, ModelLoadDiagnostics) {
        let mut budget = AuxiliaryResourceBudget::default();
        Self::load_manifest_with_resources(
            model,
            resolver,
            external,
            &mut budget,
            &RenderCancellation::default(),
        )
    }

    /// 以显式预算和取消令牌加载表情，供测试验证 generation 取消路径。
    #[cfg(test)]
    pub(in crate::model) fn load_manifest_with_resources_for_test(
        model: &Model3,
        resolver: &ModelResourceResolver,
        external: &[ExternalExpressionReference],
        budget: &mut AuxiliaryResourceBudget,
        cancellation: &RenderCancellation,
    ) -> (ExpressionController, ModelLoadDiagnostics) {
        Self::load_manifest_with_resources(model, resolver, external, budget, cancellation)
    }

    fn load_manifest_with_resources(
        model: &Model3,
        resolver: &ModelResourceResolver,
        external: &[ExternalExpressionReference],
        budget: &mut AuxiliaryResourceBudget,
        cancellation: &RenderCancellation,
    ) -> (ExpressionController, ModelLoadDiagnostics) {
        let references = model.expressions();
        let mut expressions = BTreeMap::new();
        let mut resources = BTreeMap::new();
        let mut diagnostics = ModelLoadDiagnostics::default();
        let mut declared_files = BTreeSet::new();
        let mut expression_cache = BTreeMap::<String, CachedExpression>::new();

        for (index, reference) in references.iter().take(MAX_EXPRESSION_COUNT).enumerate() {
            if cancellation.is_cancelled() {
                break;
            }
            let Some(expression) = load_expression(
                ExpressionDeclaration {
                    name: reference.name(),
                    index,
                    reference: reference.file(),
                    declared: true,
                },
                resolver,
                budget,
                cancellation,
                &mut expression_cache,
                &mut declared_files,
                &mut diagnostics,
            ) else {
                if cancellation.is_cancelled()
                    || diagnostics.entries().last().is_some_and(|diagnostic| {
                        diagnostic.category() == ModelDiagnosticCategory::LimitExceeded
                    })
                {
                    break;
                }
                continue;
            };
            if expressions
                .insert(reference.name().to_owned(), expression)
                .is_some()
            {
                diagnostics.push(ModelLoadDiagnostic::expression(
                    reference.name(),
                    index,
                    reference.file(),
                    ModelDiagnosticCategory::DuplicateName,
                    "已使用后声明的成功表情覆盖此前同名成功项",
                ));
            }
            resources.insert(
                reference.name().to_owned(),
                ModelPreviewExpression::new(reference.name(), reference.name(), false),
            );
        }

        if !cancellation.is_cancelled()
            && let Some(reference) = references.get(MAX_EXPRESSION_COUNT)
        {
            diagnostics.push(
                ModelLoadDiagnostic::expression(
                    reference.name(),
                    MAX_EXPRESSION_COUNT,
                    reference.file(),
                    ModelDiagnosticCategory::LimitExceeded,
                    format!(
                        "表情声明总数为 {}，仅处理前 {MAX_EXPRESSION_COUNT} 项",
                        references.len()
                    ),
                )
                .with_affected_count(references.len() - MAX_EXPRESSION_COUNT),
            );
        }

        let default_expression = DEFAULT_EXPRESSION_NAMES
            .into_iter()
            .find(|name| expressions.contains_key(*name))
            .map(str::to_owned);

        let remaining_capacity = MAX_EXPRESSION_COUNT.saturating_sub(references.len());
        let mut filtered_external = Vec::new();
        for reference in external {
            if cancellation.is_cancelled() {
                break;
            }
            if !declared_files.contains(reference.canonical_path()) {
                filtered_external.push(reference);
            }
        }
        let external = filtered_external;
        for (offset, reference) in external.iter().take(remaining_capacity).enumerate() {
            if cancellation.is_cancelled() {
                break;
            }
            let index = references.len().saturating_add(offset);
            let runtime_id = unique_external_runtime_id(reference.runtime_id(), &expressions);
            let Some(expression) = load_expression(
                ExpressionDeclaration {
                    name: reference.name(),
                    index,
                    reference: reference.reference(),
                    declared: false,
                },
                resolver,
                budget,
                cancellation,
                &mut expression_cache,
                &mut declared_files,
                &mut diagnostics,
            ) else {
                if cancellation.is_cancelled()
                    || diagnostics.entries().last().is_some_and(|diagnostic| {
                        diagnostic.category() == ModelDiagnosticCategory::LimitExceeded
                    })
                {
                    break;
                }
                continue;
            };
            expressions.insert(runtime_id.clone(), expression);
            resources.insert(
                runtime_id.clone(),
                ModelPreviewExpression::new(
                    runtime_id,
                    reference.name(),
                    reference.movable_to_outfit(),
                ),
            );
        }
        let external_count = external.len();
        if external_count > remaining_capacity
            && let Some(reference) = external.get(remaining_capacity)
        {
            diagnostics.push(
                ModelLoadDiagnostic::expression(
                    reference.name(),
                    MAX_EXPRESSION_COUNT,
                    reference.reference(),
                    ModelDiagnosticCategory::LimitExceeded,
                    format!(
                        "模型声明与外部表情总数超过 {MAX_EXPRESSION_COUNT}，其余外部表情已忽略"
                    ),
                )
                .with_affected_count(external_count - remaining_capacity),
            );
        }

        let mut controller = Self {
            expressions,
            resources,
            default_expression,
            manager: ExpressionManager::new(),
        };
        if let Some(name) = controller.default_expression.as_deref()
            && let Some(expression) = controller.expressions.get(name).cloned()
        {
            controller.manager.play(expression);
        }
        (controller, diagnostics)
    }

    fn empty_load_result() -> (ExpressionController, ModelLoadDiagnostics) {
        (
            Self {
                expressions: BTreeMap::new(),
                resources: BTreeMap::new(),
                default_expression: None,
                manager: ExpressionManager::new(),
            },
            ModelLoadDiagnostics::default(),
        )
    }

    /// 切换到指定表情，并返回该表情是否存在。
    pub(crate) fn play(&mut self, name: &str) -> bool {
        let Some(expression) = self.expressions.get(name).cloned() else {
            return false;
        };

        self.manager.play(expression);
        true
    }

    /// 恢复清单声明的默认表情，用于结束外部服装预览。
    pub(crate) fn reset_to_default(&mut self) -> bool {
        self.manager = ExpressionManager::new();
        if let Some(name) = self.default_expression.as_deref()
            && let Some(expression) = self.expressions.get(name).cloned()
        {
            self.manager.play(expression);
        }
        true
    }

    /// 返回全部成功加载表情的稳定 ID、默认显示名与分类约束。
    pub(crate) fn available_resources(&self) -> Vec<ModelPreviewExpression> {
        self.resources.values().cloned().collect()
    }

    /// 推进表情淡入淡出并把混合结果应用到模型参数。
    pub(crate) fn update(&mut self, runtime: &mut ModelRuntime, delta_seconds: f32) {
        self.tick(delta_seconds);
        self.manager.apply(runtime);
    }

    pub(in crate::model) fn tick(&mut self, delta_seconds: f32) {
        self.manager.tick(delta_seconds);
    }

    /// 返回当前表情管理器是否仍在执行淡入或淡出。
    pub(crate) fn needs_continuous_frames(&self) -> bool {
        self.manager.needs_tick()
    }

    #[cfg(test)]
    pub(in crate::model) fn loaded_expression_count(&self) -> usize {
        self.expressions.len()
    }

    #[cfg(test)]
    pub(in crate::model) fn first_parameter_value(&self, name: &str) -> Option<f32> {
        self.expressions
            .get(name)
            .and_then(|expression| expression.parameters().first())
            .map(|parameter| parameter.value())
    }

    #[cfg(test)]
    pub(in crate::model) fn parsed_expression_identities_for_test(
        &self,
    ) -> Vec<*const Expression3> {
        self.expressions.values().map(Arc::as_ptr).collect()
    }
}

fn unique_external_runtime_id(
    base: String,
    occupied: &BTreeMap<String, Arc<Expression3>>,
) -> String {
    if !occupied.contains_key(&base) {
        return base;
    }
    for suffix in 2_u32.. {
        let candidate = format!("{base}#{suffix}");
        if !occupied.contains_key(&candidate) {
            return candidate;
        }
    }
    unreachable!("无界递增后缀必须能生成唯一表情 ID")
}

fn load_expression(
    declaration: ExpressionDeclaration<'_>,
    resolver: &ModelResourceResolver,
    budget: &mut AuxiliaryResourceBudget,
    cancellation: &RenderCancellation,
    cache: &mut BTreeMap<String, CachedExpression>,
    declared_files: &mut BTreeSet<std::path::PathBuf>,
    diagnostics: &mut ModelLoadDiagnostics,
) -> Option<Arc<Expression3>> {
    if cancellation.is_cancelled() {
        return None;
    }
    let cached = match cache.get(declaration.reference).cloned() {
        Some(cached) => cached,
        None => {
            let cached =
                load_cached_expression(declaration.reference, resolver, budget, cancellation)?;
            cache.insert(declaration.reference.to_owned(), cached.clone());
            cached
        }
    };
    if declaration.declared
        && let Some(path) = cached.canonical_path
    {
        declared_files.insert(path);
    }
    match cached.result {
        Ok(expression) => Some(expression),
        Err(error) => {
            diagnostics.push(ModelLoadDiagnostic::expression(
                declaration.name,
                declaration.index,
                declaration.reference,
                error.category,
                error.message,
            ));
            None
        }
    }
}

fn load_cached_expression(
    reference: &str,
    resolver: &ModelResourceResolver,
    budget: &mut AuxiliaryResourceBudget,
    cancellation: &RenderCancellation,
) -> Option<CachedExpression> {
    let (canonical_path, source) = match resolver.read_text_with_path_and_budget_and_checkpoint(
        reference,
        MAX_AUXILIARY_RESOURCE_BYTES,
        budget,
        || cancellation.is_cancelled(),
    ) {
        Ok(resource) => resource,
        Err(_) if cancellation.is_cancelled() => return None,
        Err(error) => {
            return Some(CachedExpression {
                canonical_path: None,
                result: Err(CachedExpressionError {
                    category: error.category(),
                    message: error.message().to_owned(),
                }),
            });
        }
    };
    if cancellation.is_cancelled() {
        return None;
    }
    let result = Expression3::from_json_str(&source)
        .map_err(|error| CachedExpressionError {
            category: ModelDiagnosticCategory::Parse,
            message: format!("表情 JSON 内容无效：{error}"),
        })
        .and_then(|expression| {
            if expression.resolved_fade_in_time().is_finite()
                && expression.resolved_fade_in_time() <= MAX_EXPRESSION_FADE_SECONDS
                && expression.resolved_fade_out_time().is_finite()
                && expression.resolved_fade_out_time() <= MAX_EXPRESSION_FADE_SECONDS
            {
                Ok(Arc::new(expression))
            } else {
                Err(CachedExpressionError {
                    category: ModelDiagnosticCategory::InvalidDuration,
                    message: format!(
                        "表情淡入淡出时长必须是有限数且不超过 {MAX_EXPRESSION_FADE_SECONDS} 秒"
                    ),
                })
            }
        });
    if cancellation.is_cancelled() {
        return None;
    }
    Some(CachedExpression {
        canonical_path: Some(canonical_path),
        result,
    })
}
