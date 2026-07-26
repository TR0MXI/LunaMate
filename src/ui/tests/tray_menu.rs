//! 验证托盘菜单在不同系统栏位置下始终贴近图标且留在可用区域。

use gpui::{Bounds, Pixels, point, px, size};

use crate::ui::tray_menu::tray_menu_bounds_for_display;

#[test]
fn bottom_tray_places_menu_above_icon() {
    let bounds = place(
        rect(1800.0, 1042.0, 24.0, 24.0),
        rect(0.0, 0.0, 1920.0, 1080.0),
        rect(0.0, 0.0, 1920.0, 1040.0),
    );

    assert_eq!(f32::from(bounds.origin.x), 1716.0);
    assert_eq!(f32::from(bounds.origin.y), 920.0);
}

#[test]
fn top_tray_places_menu_below_icon() {
    let bounds = place(
        rect(900.0, 0.0, 24.0, 24.0),
        rect(0.0, 0.0, 1920.0, 1080.0),
        rect(0.0, 24.0, 1920.0, 1056.0),
    );

    assert_eq!(f32::from(bounds.origin.x), 816.0);
    assert_eq!(f32::from(bounds.origin.y), 32.0);
}

#[test]
fn right_tray_places_menu_left_of_icon() {
    let bounds = place(
        rect(1896.0, 500.0, 24.0, 24.0),
        rect(0.0, 0.0, 1920.0, 1080.0),
        rect(0.0, 0.0, 1896.0, 1080.0),
    );

    assert_eq!(f32::from(bounds.origin.x), 1696.0);
    assert_eq!(f32::from(bounds.origin.y), 456.0);
}

#[test]
fn menu_origin_is_clamped_to_visible_margin() {
    let bounds = place(
        rect(0.0, 0.0, 24.0, 24.0),
        rect(0.0, 0.0, 1280.0, 720.0),
        rect(0.0, 24.0, 1280.0, 696.0),
    );

    assert_eq!(f32::from(bounds.origin.x), 8.0);
    assert_eq!(f32::from(bounds.origin.y), 32.0);
}

#[test]
fn tray_menu_uses_compact_fixed_dimensions() {
    let bounds = place(
        rect(900.0, 0.0, 24.0, 24.0),
        rect(0.0, 0.0, 1920.0, 1080.0),
        rect(0.0, 24.0, 1920.0, 1056.0),
    );

    assert_eq!(f32::from(bounds.size.width), 192.0);
    assert_eq!(f32::from(bounds.size.height), 112.0);
}

#[test]
fn left_tray_places_menu_right_of_icon() {
    let bounds = place(
        rect(0.0, 500.0, 24.0, 24.0),
        rect(0.0, 0.0, 1920.0, 1080.0),
        rect(24.0, 0.0, 1896.0, 1080.0),
    );

    assert_eq!(f32::from(bounds.origin.x), 32.0);
    assert_eq!(f32::from(bounds.origin.y), 456.0);
}

#[test]
fn menu_stays_inside_displays_smaller_than_the_menu_itself() {
    // 可用区域比菜单还小时，夹取上界会低于下界；菜单应贴住左上边距而不是跳到负坐标。
    let bounds = place(
        rect(40.0, 40.0, 24.0, 24.0),
        rect(0.0, 0.0, 160.0, 100.0),
        rect(0.0, 0.0, 160.0, 100.0),
    );

    assert_eq!(f32::from(bounds.origin.x), 8.0);
    assert_eq!(f32::from(bounds.origin.y), 8.0);
    assert_eq!(f32::from(bounds.size.width), 192.0);
    assert_eq!(f32::from(bounds.size.height), 112.0);
}

#[test]
fn menu_position_accounts_for_secondary_display_origins() {
    let bounds = place(
        rect(3820.0, 1042.0, 24.0, 24.0),
        rect(1920.0, 0.0, 1920.0, 1080.0),
        rect(1920.0, 0.0, 1920.0, 1040.0),
    );

    // 图标贴近副屏底边，菜单应当在同一屏内上移且不越过右侧留白。
    assert_eq!(f32::from(bounds.origin.y), 920.0);
    assert!(f32::from(bounds.origin.x) >= 1928.0);
    assert!(f32::from(bounds.origin.x) + 192.0 <= 3832.0);
}

fn place(icon: Bounds<Pixels>, display: Bounds<Pixels>, visible: Bounds<Pixels>) -> Bounds<Pixels> {
    tray_menu_bounds_for_display(icon, display, visible)
}

fn rect(x: f32, y: f32, width: f32, height: f32) -> Bounds<Pixels> {
    Bounds {
        origin: point(px(x), px(y)),
        size: size(px(width), px(height)),
    }
}
