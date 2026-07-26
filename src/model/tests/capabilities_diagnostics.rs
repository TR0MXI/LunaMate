use rust_i18n::t;

use crate::model::capabilities::{
    ModelDiagnosticCategory, ModelDiagnosticResource, ModelLoadDiagnostic, ModelLoadDiagnostics,
};

#[test]
fn summary_counts_aggregate_and_individual_diagnostics() {
    let mut diagnostics = ModelLoadDiagnostics::default();
    diagnostics.push(
        ModelLoadDiagnostic::motion(
            "Idle",
            0,
            "missing.motion3.json",
            ModelDiagnosticCategory::Missing,
            "路径不存在",
        )
        .with_affected_count(2),
    );
    diagnostics.push(ModelLoadDiagnostic::expression(
        "Smile",
        0,
        "broken.exp3.json",
        ModelDiagnosticCategory::Parse,
        "表情 JSON 内容无效",
    ));

    let expected = [
        t!("model_state.unavailable_motions", count = 2).to_string(),
        t!("model_state.unavailable_expressions", count = 1).to_string(),
    ]
    .join(t!("model_state.summary_separator").as_ref());
    assert_eq!(diagnostics.summary().as_deref(), Some(expected.as_str()));
}

#[test]
fn empty_diagnostics_have_no_summary() {
    assert!(ModelLoadDiagnostics::default().summary().is_none());
}

#[test]
fn every_category_renders_a_distinct_chinese_label() {
    let categories = [
        ModelDiagnosticCategory::InvalidReference,
        ModelDiagnosticCategory::Missing,
        ModelDiagnosticCategory::NotFile,
        ModelDiagnosticCategory::TooLarge,
        ModelDiagnosticCategory::Read,
        ModelDiagnosticCategory::Parse,
        ModelDiagnosticCategory::InvalidDuration,
        ModelDiagnosticCategory::LimitExceeded,
        ModelDiagnosticCategory::DuplicateName,
    ];

    let mut labels = Vec::with_capacity(categories.len());
    for category in categories {
        let label = category.to_string();
        assert!(!label.is_empty(), "{category:?} 应当有可展示标签");
        assert!(!labels.contains(&label), "{category:?} 的标签应当唯一");
        labels.push(label);
    }
}

#[test]
fn motion_diagnostics_expose_group_and_declaration_position() {
    let diagnostic = ModelLoadDiagnostic::motion(
        "TapBody",
        3,
        "motions/tap.motion3.json",
        ModelDiagnosticCategory::Read,
        "读取被拒绝",
    );

    assert_eq!(diagnostic.resource(), ModelDiagnosticResource::Motion);
    assert_eq!(diagnostic.category(), ModelDiagnosticCategory::Read);
    assert_eq!(diagnostic.group(), Some("TapBody"));
    assert_eq!(diagnostic.name(), None);
    assert_eq!(diagnostic.declaration_index(), Some(3));
    assert_eq!(diagnostic.reference(), Some("motions/tap.motion3.json"));
    assert_eq!(diagnostic.affected_count(), 1);
    assert_eq!(diagnostic.message(), "读取被拒绝");

    let rendered = diagnostic.to_string();
    assert!(rendered.starts_with("动作组 TapBody[3]（motions/tap.motion3.json）"));
    assert!(rendered.contains("读取失败"));
    assert!(rendered.contains("读取被拒绝"));
    assert!(!rendered.contains("共影响"));
}

#[test]
fn expression_and_hit_area_diagnostics_use_their_own_prefixes() {
    let expression = ModelLoadDiagnostic::expression(
        "Smile",
        1,
        "exp/smile.exp3.json",
        ModelDiagnosticCategory::Parse,
        "JSON 无效",
    );
    let hit_area = ModelLoadDiagnostic::hit_area(
        "Head",
        0,
        "D_Head",
        ModelDiagnosticCategory::Missing,
        "Drawable 不存在",
    );

    assert_eq!(expression.resource(), ModelDiagnosticResource::Expression);
    assert_eq!(expression.name(), Some("Smile"));
    assert_eq!(expression.group(), None);
    assert!(
        expression
            .to_string()
            .starts_with("表情 Smile[1]（exp/smile.exp3.json）")
    );

    assert_eq!(hit_area.resource(), ModelDiagnosticResource::HitArea);
    assert_eq!(hit_area.name(), Some("Head"));
    assert!(
        hit_area
            .to_string()
            .starts_with("HitArea Head[0]（D_Head）")
    );
}

#[test]
fn aggregate_diagnostics_report_the_affected_declaration_count() {
    let aggregate = ModelLoadDiagnostic::expression(
        "Smile",
        0,
        "exp/smile.exp3.json",
        ModelDiagnosticCategory::LimitExceeded,
        "超过表情数量上限",
    )
    .with_affected_count(7);

    assert_eq!(aggregate.affected_count(), 7);
    assert!(aggregate.to_string().ends_with("；共影响 7 项"));

    // 聚合诊断至少代表一项声明，零值会让状态摘要少算不可用能力。
    let clamped = aggregate.with_affected_count(0);
    assert_eq!(clamped.affected_count(), 1);
    assert!(!clamped.to_string().contains("共影响"));
}

#[test]
fn summary_counts_hit_areas_and_preserves_discovery_order() {
    let mut diagnostics = ModelLoadDiagnostics::default();
    assert!(diagnostics.is_empty());
    assert!(diagnostics.entries().is_empty());

    diagnostics.extend([
        ModelLoadDiagnostic::hit_area(
            "Head",
            0,
            "D_Head",
            ModelDiagnosticCategory::Missing,
            "Drawable 不存在",
        ),
        ModelLoadDiagnostic::motion(
            "Idle",
            0,
            "idle.motion3.json",
            ModelDiagnosticCategory::InvalidDuration,
            "淡入时长为负",
        ),
    ]);
    diagnostics.push(ModelLoadDiagnostic::hit_area(
        "Body",
        1,
        "D_Body",
        ModelDiagnosticCategory::InvalidReference,
        "引用越界",
    ));

    assert!(!diagnostics.is_empty());
    assert_eq!(diagnostics.entries().len(), 3);
    assert_eq!(diagnostics.entries()[0].name(), Some("Head"));
    assert_eq!(diagnostics.entries()[1].group(), Some("Idle"));

    let summary = diagnostics.summary().expect("非空诊断应当有摘要");
    assert!(summary.contains(&t!("model_state.unavailable_motions", count = 1).to_string()));
    assert!(summary.contains(&t!("model_state.unavailable_hit_areas", count = 2).to_string()));

    let collected = diagnostics.into_iter().collect::<Vec<_>>();
    assert_eq!(collected.len(), 3);
    assert_eq!(
        collected[2].category(),
        ModelDiagnosticCategory::InvalidReference
    );
}
