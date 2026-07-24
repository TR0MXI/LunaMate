use std::sync::Arc;

use gpui::RenderImage;

use crate::model::interaction::{
    RenderedHitArea, RenderedModelFrame,
    hit_area::{window_point_to_raster, window_point_to_stretched_raster},
};

use image::{Frame, RgbaImage};

#[test]
fn maps_matching_aspect_ratio_to_raster_coordinates() {
    assert_eq!(
        window_point_to_raster([180.0, 320.0], [360.0, 640.0], [720, 1280]),
        Some([360.0, 640.0])
    );
}

#[test]
fn rejects_horizontal_contain_letterbox() {
    assert_eq!(
        window_point_to_raster([249.0, 300.0], [800.0, 600.0], [300, 600]),
        None
    );
    assert_eq!(
        window_point_to_raster([400.0, 300.0], [800.0, 600.0], [300, 600]),
        Some([150.0, 300.0])
    );
}

#[test]
fn rejects_vertical_contain_letterbox() {
    assert_eq!(
        window_point_to_raster([150.0, 99.0], [300.0, 800.0], [300, 600]),
        None
    );
    assert_eq!(
        window_point_to_raster([150.0, 400.0], [300.0, 800.0], [300, 600]),
        Some([150.0, 300.0])
    );
}

#[test]
fn hit_area_uses_inclusive_raster_bounds() {
    let area = RenderedHitArea::new(
        Arc::from("HitArea"),
        Arc::from("Body"),
        [10.0, 20.0, 30.0, 40.0],
    )
    .expect("finite bounds should create a hit area");

    assert!(area.contains([10.0, 20.0]));
    assert!(area.contains([30.0, 40.0]));
    assert!(!area.contains([30.1, 40.0]));
}

#[test]
fn invalid_dimensions_and_coordinates_are_rejected() {
    assert_eq!(
        window_point_to_raster([0.0, 0.0], [0.0, 640.0], [360, 640]),
        None
    );
    assert_eq!(
        window_point_to_raster([f32::NAN, 0.0], [360.0, 640.0], [360, 640]),
        None
    );
    assert!(
        RenderedHitArea::new(
            Arc::from("HitArea"),
            Arc::from("Body"),
            [f32::NAN, 0.0, 1.0, 1.0],
        )
        .is_none()
    );
}

#[test]
fn frame_hit_test_preserves_model_declaration_order() {
    let areas = vec![
        RenderedHitArea::new(
            Arc::from("BodyDrawable"),
            Arc::from("Body"),
            [20.0, 20.0, 80.0, 80.0],
        )
        .expect("body bounds should be valid"),
        RenderedHitArea::new(
            Arc::from("HeadDrawable"),
            Arc::from("Head"),
            [40.0, 40.0, 60.0, 60.0],
        )
        .expect("head bounds should be valid"),
    ];
    let image = RenderImage::new(vec![Frame::new(RgbaImage::new(100, 100))]);
    let frame = RenderedModelFrame::new(image, areas, [100, 100]);

    let hit = frame
        .hit_area_at_window_point([50.0, 50.0], [100.0, 100.0])
        .expect("overlapping point should hit the first declared area");
    assert_eq!(hit.name(), "Body");
    assert!(
        frame
            .hit_area_at_window_point([5.0, 5.0], [100.0, 100.0])
            .is_none()
    );
}

#[test]
fn gpu_frame_keeps_hit_testing_without_a_gpui_image() {
    let area = RenderedHitArea::new(
        Arc::from("BodyDrawable"),
        Arc::from("Body"),
        [10.0, 10.0, 90.0, 90.0],
    )
    .expect("GPU 帧命中区域应有效");
    let frame = RenderedModelFrame::gpu(vec![area], [100, 100]);

    assert!(frame.image().is_none());
    assert!(
        frame
            .hit_area_at_window_point([50.0, 50.0], [100.0, 100.0])
            .is_some()
    );
}

#[test]
fn gpu_frame_maps_each_surface_axis_without_contain_letterboxing() {
    assert_eq!(
        window_point_to_stretched_raster([150.0, 250.0], [300.0, 500.0], [375, 626]),
        Some([187.5, 313.0])
    );
}
