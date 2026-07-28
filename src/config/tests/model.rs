use std::path::PathBuf;

use toml_edit::DocumentMut;

use crate::config::{
    ModelExpressionCategory, ModelResourceKey, ModelResourceKind,
    model::{parse_model_resource_settings, write_model_resource_settings},
};

fn key(kind: ModelResourceKind, id: &str) -> ModelResourceKey {
    ModelResourceKey::new(PathBuf::from("Hiyori/runtime/model.model3.json"), kind, id)
}

#[test]
fn model_resource_overrides_round_trip_without_touching_files() {
    let settings = parse_model_resource_settings(&DocumentMut::new(), &mut Vec::new())
        .with_name(key(ModelResourceKind::Motion, "Tap"), Some("挥手"))
        .expect("动作名称覆盖应当有效")
        .with_expression_category(
            key(ModelResourceKind::Expression, "external:maid.exp3.json"),
            ModelExpressionCategory::Outfit,
        )
        .expect("根目录表达式应当可以分类为服装");
    let mut document = DocumentMut::new();

    write_model_resource_settings(&mut document, &settings);
    let mut warnings = Vec::new();
    let parsed = parse_model_resource_settings(&document, &mut warnings);

    assert!(warnings.is_empty());
    assert_eq!(parsed, settings);
    assert_eq!(
        parsed.name(&key(ModelResourceKind::Motion, "Tap")),
        Some("挥手")
    );
    assert_eq!(
        parsed.expression_category(&key(
            ModelResourceKind::Expression,
            "external:maid.exp3.json"
        )),
        ModelExpressionCategory::Outfit
    );
}

#[test]
fn restoring_defaults_removes_sparse_entries() {
    let motion = key(ModelResourceKind::Motion, "Tap");
    let expression = key(ModelResourceKind::Expression, "external:maid.exp3.json");
    let settings = parse_model_resource_settings(&DocumentMut::new(), &mut Vec::new())
        .with_name(motion.clone(), Some("挥手"))
        .expect("测试名称应当有效")
        .with_expression_category(expression.clone(), ModelExpressionCategory::Outfit)
        .expect("测试分类应当有效")
        .with_name(motion, None)
        .expect("名称应当可以恢复默认")
        .with_expression_category(expression, ModelExpressionCategory::Expression)
        .expect("分类应当可以恢复默认");

    assert_eq!(settings.entry_count_for_test(), 0);
}

#[test]
fn identical_runtime_ids_in_different_resource_kinds_do_not_share_aliases() {
    let motion = key(ModelResourceKind::Motion, "Smile");
    let expression = key(ModelResourceKind::Expression, "Smile");
    let settings = parse_model_resource_settings(&DocumentMut::new(), &mut Vec::new())
        .with_name(motion.clone(), Some("微笑动作"))
        .expect("动作名称应当有效")
        .with_name(expression.clone(), Some("微笑表情"))
        .expect("表情名称应当有效");

    assert_eq!(settings.name(&motion), Some("微笑动作"));
    assert_eq!(settings.name(&expression), Some("微笑表情"));
}

#[test]
fn invalid_entries_are_skipped_individually() {
    let document = r#"
        [[model.resources]]
        manifest = "../outside.model3.json"
        kind = "motion"
        id = "Tap"
        name = "危险"

        [[model.resources]]
        manifest = "Hiyori/model.model3.json"
        kind = "motion"
        id = "Tap"
        name = "挥手"
    "#
    .parse::<DocumentMut>()
    .expect("测试配置应当可以解析");
    let mut warnings = Vec::new();

    let settings = parse_model_resource_settings(&document, &mut warnings);

    assert_eq!(settings.entry_count_for_test(), 1);
    assert_eq!(warnings.len(), 1);
}
