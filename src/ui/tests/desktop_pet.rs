use gpui::{point, px, size};
use lunamate_agent::tools::{AgentOutfitRequest, AgentOutfitResult};
use rust_i18n::t;

use crate::{
    model::{ModelDiagnosticCategory, ModelLoadDiagnostic, ModelLoadDiagnostics},
    ui::desktop_pet::{DesktopPetView, ModelLoadState, look_target_for_position},
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

#[test]
fn look_target_maps_the_window_center_to_neutral() {
    let viewport = size(px(200.0), px(100.0));

    assert_eq!(
        look_target_for_position(point(px(100.0), px(50.0)), viewport),
        [0.0, 0.0]
    );
}

#[test]
fn look_target_clamps_cursor_positions_outside_the_window() {
    let viewport = size(px(200.0), px(100.0));

    assert_eq!(
        look_target_for_position(point(px(-80.0), px(-40.0)), viewport),
        [-1.0, 1.0]
    );
    assert_eq!(
        look_target_for_position(point(px(320.0), px(180.0)), viewport),
        [1.0, -1.0]
    );
}

#[test]
fn outfit_completion_reports_whether_the_immediate_load_command_was_issued() {
    let (not_issued, failed) = AgentOutfitRequest::channel("variant:coat".to_owned(), 1);
    not_issued.complete(DesktopPetView::model_load_command_was_issued_for_test(7, 7));
    assert_eq!(failed.try_recv(), Ok(AgentOutfitResult::Failed));

    let (issued, applied) = AgentOutfitRequest::channel("variant:coat".to_owned(), 1);
    issued.complete(DesktopPetView::model_load_command_was_issued_for_test(7, 8));
    assert_eq!(applied.try_recv(), Ok(AgentOutfitResult::Applied));
}
