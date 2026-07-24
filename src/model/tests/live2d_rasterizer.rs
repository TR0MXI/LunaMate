use mocari::{assets::DecodedTexture, moc3::Moc3DrawableBlendMode};

use crate::model::live2d::renderer::rasterizer::{blend, sample_texture};

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
