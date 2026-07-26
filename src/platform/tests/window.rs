use gpui::{Bounds, point, px, size};

use crate::platform::window::{WindowPositionController, physical_window_rect};

#[test]
fn reset_request_suppresses_bounds_before_move_is_applied() {
    let mut controller = WindowPositionController::default();
    controller.request_reset();

    assert!(!controller.observe_bounds());
}

#[test]
fn tray_menu_bounds_scale_to_target_monitor_physical_pixels() {
    let bounds = Bounds {
        origin: point(px(1200.0), px(40.0)),
        size: size(px(192.0), px(112.0)),
    };

    assert_eq!(physical_window_rect(bounds, 2.0), [2400, 80, 384, 224]);
}

#[test]
fn tray_menu_physical_bounds_preserve_negative_monitor_origins() {
    let bounds = Bounds {
        origin: point(px(-1000.0), px(120.0)),
        size: size(px(192.0), px(112.0)),
    };

    assert_eq!(physical_window_rect(bounds, 1.5), [-1500, 180, 288, 168]);
}

#[test]
fn tray_menu_physical_bounds_reject_invalid_scale() {
    let bounds = Bounds {
        origin: point(px(812.0), px(32.0)),
        size: size(px(192.0), px(112.0)),
    };

    assert_eq!(physical_window_rect(bounds, 0.0), [812, 32, 192, 112]);
}
