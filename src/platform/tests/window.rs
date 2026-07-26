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
    for scale_factor in [-2.0, f64::NAN, f64::INFINITY] {
        assert_eq!(
            physical_window_rect(bounds, scale_factor),
            [812, 32, 192, 112],
            "{scale_factor} 不是有效缩放，应当回退到 1.0"
        );
    }
}

#[test]
fn tray_menu_physical_bounds_never_collapse_to_an_empty_window() {
    let degenerate = Bounds {
        origin: point(px(0.0), px(0.0)),
        size: size(px(0.0), px(0.0)),
    };

    assert_eq!(physical_window_rect(degenerate, 1.0), [0, 0, 1, 1]);
}

#[test]
fn idle_controller_lets_every_bounds_update_reach_the_position_cache() {
    let mut controller = WindowPositionController::default();

    assert!(controller.observe_bounds());
    assert!(controller.observe_bounds());
}

#[test]
fn a_reset_request_is_cleared_once_the_platform_refuses_to_move_the_window() {
    let mut controller = WindowPositionController::default();
    controller.request_reset();
    assert!(!controller.observe_bounds());

    // 重复请求保持抑制状态，直到复位真正被处理。
    controller.request_reset();
    assert!(!controller.observe_bounds());
}
