//! 验证托盘位图的尺寸与透明边界。

use crate::platform::tray::{TrayIconStyle, tray_icon_rgba};

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
