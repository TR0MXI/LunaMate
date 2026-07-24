use rust_i18n::t;

use crate::model::capabilities::{
    ModelDiagnosticCategory, ModelLoadDiagnostic, ModelLoadDiagnostics,
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
