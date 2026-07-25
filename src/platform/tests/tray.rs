//! 验证托盘位图的尺寸与透明边界。

use crate::platform::tray::tray_icon_rgba;

#[test]
fn tray_icon_has_expected_rgba_shape() {
    let pixels = tray_icon_rgba();
    assert_eq!(pixels.len(), 32 * 32 * 4);
    assert!(pixels.chunks_exact(4).any(|pixel| pixel[3] == 0));
    assert!(pixels.chunks_exact(4).any(|pixel| pixel[3] == 255));
}
