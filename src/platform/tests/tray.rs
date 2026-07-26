//! 验证托盘位图与菜单锚点的尺寸、颜色和缩放换算。

use crate::platform::tray::{TrayIconStyle, TrayMenuAnchor, tray_icon_rgba};

#[test]
fn tray_icon_has_expected_rgba_shape() {
    let pixels = tray_icon_rgba(TrayIconStyle::default());
    assert_eq!(pixels.len(), 32 * 32 * 4);
    assert!(pixels.chunks_exact(4).any(|pixel| pixel[3] == 0));
    assert!(pixels.chunks_exact(4).any(|pixel| pixel[3] == 255));
}

#[test]
fn tray_icon_uses_the_current_theme_semantic_colors() {
    let style = TrayIconStyle::new([1, 2, 3], [4, 5, 6]);
    let pixels = tray_icon_rgba(style);

    assert!(pixels.chunks_exact(4).any(|pixel| pixel == [1, 2, 3, 255]));
    assert!(pixels.chunks_exact(4).any(|pixel| pixel == [4, 5, 6, 255]));
}

#[test]
fn tray_menu_anchor_converts_physical_coordinates_with_display_scale() {
    let anchor = TrayMenuAnchor::from_physical([2424.0, 104.0], [2400.0, 80.0], [48, 48], 2.0);

    assert_eq!(anchor.icon_origin, [1200.0, 40.0]);
    assert_eq!(anchor.icon_size, [24.0, 24.0]);
    assert_eq!(anchor.scale_factor, 2.0);
}

#[test]
fn tray_menu_anchor_rejects_invalid_display_scale() {
    let anchor = TrayMenuAnchor::from_physical([132.0, 52.0], [120.0, 40.0], [24, 24], 0.0);

    assert_eq!(anchor.icon_origin, [120.0, 40.0]);
    assert_eq!(anchor.icon_size, [24.0, 24.0]);
    assert_eq!(anchor.scale_factor, 1.0);
}

#[test]
fn tray_menu_anchor_uses_click_when_shell_reports_a_stale_icon_rect() {
    let anchor = TrayMenuAnchor::from_physical([900.0, 12.0], [1800.0, 1042.0], [24, 24], 1.0);

    assert_eq!(anchor.icon_origin, [888.0, 0.0]);
    assert_eq!(anchor.icon_size, [24.0, 24.0]);
}
