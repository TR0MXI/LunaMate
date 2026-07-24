use rust_i18n::t;

use crate::{
    model::{ModelDiagnosticCategory, ModelLoadDiagnostic, ModelLoadDiagnostics},
    ui::desktop_pet::ModelLoadState,
};

#[test]
fn no_model_state_does_not_render_a_notice() {
    assert!(ModelLoadState::NoModel.message().is_none());
}

#[test]
fn ready_state_keeps_partial_capability_failures_non_fatal() {
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
    let state = ModelLoadState::ready(diagnostics);

    assert!(state.message().is_none());
    let summary = [
        t!("model_state.unavailable_motions", count = 2).to_string(),
        t!("model_state.unavailable_expressions", count = 1).to_string(),
    ]
    .join(t!("model_state.summary_separator").as_ref());
    let expected = t!("model_state.loaded_with_warnings", summary = summary).to_string();
    assert_eq!(
        state.diagnostics_message().as_deref(),
        Some(expected.as_str())
    );
}
