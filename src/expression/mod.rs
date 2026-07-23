//! 管理 Live2D 表情资源、切换与淡入淡出。
//!
//! 表情参数只在模型逐帧更新阶段写入，UI 与聊天模块无需了解 Mocari 类型。

use std::collections::{BTreeMap, BTreeSet};

use mocari::{
    ModelRuntime,
    assets::RuntimeModel,
    expression::ExpressionManager,
    json::{Expression3, Model3},
};

use crate::capabilities::{
    AuxiliaryResourceBudget, ExternalExpressionReference, MAX_AUXILIARY_RESOURCE_BYTES,
    ModelDiagnosticCategory, ModelLoadDiagnostic, ModelLoadDiagnostics, ModelResourceResolver,
};
use crate::live2d_image::RenderCancellation;

const DEFAULT_EXPRESSION_NAMES: [&str; 2] = ["Default", "Idle"];
const MAX_EXPRESSION_COUNT: usize = 128;
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
    expressions: BTreeMap<String, Expression3>,
    outfits: Vec<String>,
    default_expression: Option<String>,
    manager: ExpressionManager,
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
            resolver.discover_external_expressions()
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
    fn load_manifest(
        model: &Model3,
        resolver: &ModelResourceResolver,
    ) -> (ExpressionController, ModelLoadDiagnostics) {
        Self::load_manifest_with_external(model, resolver, &[])
    }

    #[cfg(test)]
    fn load_manifest_with_external(
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

    fn load_manifest_with_resources(
        model: &Model3,
        resolver: &ModelResourceResolver,
        external: &[ExternalExpressionReference],
        budget: &mut AuxiliaryResourceBudget,
        cancellation: &RenderCancellation,
    ) -> (ExpressionController, ModelLoadDiagnostics) {
        let references = model.expressions();
        let mut expressions = BTreeMap::new();
        let mut outfits = Vec::new();
        let mut diagnostics = ModelLoadDiagnostics::default();

        for (index, reference) in references.iter().take(MAX_EXPRESSION_COUNT).enumerate() {
            if cancellation.is_cancelled() {
                break;
            }
            let Some(expression) = load_expression(
                reference.name(),
                index,
                reference.file(),
                resolver,
                budget,
                cancellation,
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
        }

        if let Some(reference) = references.get(MAX_EXPRESSION_COUNT) {
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

        let declared_files = references
            .iter()
            .map(|reference| reference.file())
            .collect::<BTreeSet<_>>();
        let remaining_capacity = MAX_EXPRESSION_COUNT.saturating_sub(references.len());
        let external = external
            .iter()
            .filter(|reference| !declared_files.contains(reference.reference()));
        for (offset, reference) in external.clone().take(remaining_capacity).enumerate() {
            if cancellation.is_cancelled() {
                break;
            }
            let index = references.len().saturating_add(offset);
            if expressions.contains_key(reference.name()) {
                diagnostics.push(ModelLoadDiagnostic::expression(
                    reference.name(),
                    index,
                    reference.reference(),
                    ModelDiagnosticCategory::DuplicateName,
                    "外部表情与模型清单中的名称重复，已保留清单声明",
                ));
                continue;
            }
            let Some(expression) = load_expression(
                reference.name(),
                index,
                reference.reference(),
                resolver,
                budget,
                cancellation,
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
            expressions.insert(reference.name().to_owned(), expression);
            outfits.push(reference.name().to_owned());
        }
        let external_count = external.clone().count();
        if external_count > remaining_capacity
            && let Some(reference) = external.clone().nth(remaining_capacity)
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
            outfits,
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
                outfits: Vec::new(),
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

    /// 返回全部成功加载且可供预览的表情名称。
    pub(crate) fn available_names(&self) -> Vec<String> {
        self.expressions.keys().cloned().collect()
    }

    /// 返回从模型目录外部表达式发现的服装名称。
    pub(crate) fn available_outfits(&self) -> Vec<String> {
        self.outfits.clone()
    }

    /// 推进表情淡入淡出并把混合结果应用到模型参数。
    pub(crate) fn update(&mut self, runtime: &mut ModelRuntime, delta_seconds: f32) {
        self.tick(delta_seconds);
        self.manager.apply(runtime);
    }

    fn tick(&mut self, delta_seconds: f32) {
        self.manager.tick(delta_seconds);
    }

    /// 返回当前表情管理器是否仍在执行淡入或淡出。
    pub(crate) fn needs_continuous_frames(&self) -> bool {
        self.manager.needs_tick()
    }

    #[cfg(test)]
    fn loaded_expression_count(&self) -> usize {
        self.expressions.len()
    }

    #[cfg(test)]
    fn first_parameter_value(&self, name: &str) -> Option<f32> {
        self.expressions
            .get(name)
            .and_then(|expression| expression.parameters().first())
            .map(|parameter| parameter.value())
    }
}

fn load_expression(
    name: &str,
    index: usize,
    reference: &str,
    resolver: &ModelResourceResolver,
    budget: &mut AuxiliaryResourceBudget,
    cancellation: &RenderCancellation,
    diagnostics: &mut ModelLoadDiagnostics,
) -> Option<Expression3> {
    if cancellation.is_cancelled() {
        return None;
    }
    let source = match resolver.read_text_with_budget_and_checkpoint(
        reference,
        MAX_AUXILIARY_RESOURCE_BYTES,
        budget,
        || cancellation.is_cancelled(),
    ) {
        Ok(source) => source,
        Err(error) => {
            if cancellation.is_cancelled() {
                return None;
            }
            diagnostics.push(ModelLoadDiagnostic::expression(
                name,
                index,
                reference,
                error.category(),
                error.message(),
            ));
            return None;
        }
    };
    if cancellation.is_cancelled() {
        return None;
    }
    match Expression3::from_json_str(&source) {
        Ok(expression)
            if expression.resolved_fade_in_time().is_finite()
                && expression.resolved_fade_in_time() <= MAX_EXPRESSION_FADE_SECONDS
                && expression.resolved_fade_out_time().is_finite()
                && expression.resolved_fade_out_time() <= MAX_EXPRESSION_FADE_SECONDS =>
        {
            Some(expression)
        }
        Ok(_) => {
            diagnostics.push(ModelLoadDiagnostic::expression(
                name,
                index,
                reference,
                ModelDiagnosticCategory::InvalidDuration,
                format!("表情淡入淡出时长必须是有限数且不超过 {MAX_EXPRESSION_FADE_SECONDS} 秒"),
            ));
            None
        }
        Err(error) => {
            diagnostics.push(ModelLoadDiagnostic::expression(
                name,
                index,
                reference,
                ModelDiagnosticCategory::Parse,
                format!("表情 JSON 内容无效：{error}"),
            ));
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use std::{
        fs,
        path::{Path, PathBuf},
        time::{SystemTime, UNIX_EPOCH},
    };

    use mocari::json::Model3;

    use super::*;

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
    fn mixed_expression_failures_keep_successful_entries() {
        let directory = TestDirectory::new();
        write_expression(&directory.path().join("default.exp3.json"), 1.0);
        fs::write(directory.path().join("malformed.exp3.json"), "[")
            .expect("损坏测试表情应当可以创建");
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
}
