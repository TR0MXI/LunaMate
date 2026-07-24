use crate::platform::window::WindowPositionController;

#[test]
fn reset_request_suppresses_bounds_before_move_is_applied() {
    let mut controller = WindowPositionController::default();
    controller.request_reset();

    assert!(!controller.observe_bounds());
}
