use mocari::{assets::DecodedTexture, moc3::Moc3DrawableBlendMode};

use crate::model::live2d::renderer::rasterizer::{PixelBounds, blend, sample_texture};

#[test]
fn triangles_outside_the_raster_are_culled_instead_of_clamped() {
    // 完全位于左侧或上方的三角形若被钳制，会在边缘留下 1 像素条带并扩大脏区域。
    assert!(
        PixelBounds::from_triangle([[-50.0, 8.0], [-20.0, 8.0], [-35.0, 20.0]], 64, 64).is_none()
    );
    assert!(
        PixelBounds::from_triangle([[8.0, -50.0], [20.0, -50.0], [14.0, -20.0]], 64, 64).is_none()
    );
    assert!(PixelBounds::from_triangle([[70.0, 8.0], [90.0, 8.0], [80.0, 20.0]], 64, 64).is_none());
    // 部分相交的三角形仍应保留被钳制到光栅内的区间。
    assert!(PixelBounds::from_triangle([[-10.0, 8.0], [10.0, 8.0], [0.0, 20.0]], 64, 64).is_some());
}

#[test]
fn normal_blend_uses_premultiplied_alpha() {
    let mut destination = [0.0, 0.0, 1.0, 1.0];
    blend(
        &mut destination,
        [0.5, 0.0, 0.0, 0.5],
        Moc3DrawableBlendMode::Normal,
    );
    assert_eq!(destination, [0.5, 0.0, 0.5, 1.0]);
}

#[test]
fn bilinear_sampling_uses_horizontal_weight_on_the_bottom_row() {
    let texture = DecodedTexture::new(
        2,
        2,
        vec![0, 0, 0, 255, 0, 0, 0, 255, 0, 0, 0, 255, 255, 0, 0, 255],
    );

    let sampled = sample_texture(&texture, [0.25, 0.75]);
    assert!((sampled[0] - 0.1875).abs() < f32::EPSILON);
    assert_eq!(&sampled[1..], &[0.0, 0.0, 1.0]);
}
