use std::sync::Arc;

use gpui::RenderImage;

use mocari::moc3::{Moc3DrawableMesh, Moc3DrawableVertex};

use crate::model::{
    capabilities::HitAreaCapability,
    interaction::{
        RenderedHitArea, RenderedModelFrame,
        hit_area::{render_hit_areas, window_point_to_raster, window_point_to_stretched_raster},
    },
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

#[test]
fn inverted_or_degenerate_bounds_do_not_create_a_hit_area() {
    let id: Arc<str> = Arc::from("BodyDrawable");
    let name: Arc<str> = Arc::from("Body");

    assert!(RenderedHitArea::new(id.clone(), name.clone(), [30.0, 0.0, 10.0, 40.0]).is_none());
    assert!(RenderedHitArea::new(id.clone(), name.clone(), [0.0, 40.0, 10.0, 20.0]).is_none());
    assert!(
        RenderedHitArea::new(id.clone(), name.clone(), [0.0, 0.0, f32::INFINITY, 1.0]).is_none()
    );

    // 退化为一个点的包围盒仍然合法：闭区间检测只命中该点。
    let point = RenderedHitArea::new(id, name, [5.0, 5.0, 5.0, 5.0]).expect("单点包围盒应当合法");
    assert!(point.contains([5.0, 5.0]));
    assert!(!point.contains([5.0, 5.000_01]));
}

#[test]
fn non_finite_probe_points_never_hit_an_area() {
    let area = RenderedHitArea::new(
        Arc::from("BodyDrawable"),
        Arc::from("Body"),
        [f32::MIN, f32::MIN, f32::MAX, f32::MAX],
    )
    .expect("覆盖整个平面的包围盒应当合法");

    for point in [
        [f32::NAN, 0.0],
        [0.0, f32::NAN],
        [f32::INFINITY, 0.0],
        [0.0, f32::NEG_INFINITY],
    ] {
        assert!(!area.contains(point), "{point:?} 不应命中任何区域");
    }
}

#[test]
fn activation_carries_the_declared_identifier_and_name() {
    let area = RenderedHitArea::new(
        Arc::from("D_Head"),
        Arc::from("Head"),
        [0.0, 0.0, 10.0, 10.0],
    )
    .expect("测试包围盒应当合法");

    let activation = area.activation();

    assert_eq!(activation.id(), "D_Head");
    assert_eq!(activation.name(), "Head");
}

#[test]
fn stretched_mapping_rejects_degenerate_surfaces_and_outside_points() {
    for (position, viewport, raster) in [
        ([10.0_f32, 10.0], [0.0_f32, 100.0], [100_u32, 100]),
        ([10.0, 10.0], [100.0, -1.0], [100, 100]),
        ([10.0, 10.0], [100.0, 100.0], [0, 100]),
        ([10.0, 10.0], [100.0, 100.0], [100, 0]),
        ([-0.5, 10.0], [100.0, 100.0], [100, 100]),
        ([10.0, 100.5], [100.0, 100.0], [100, 100]),
        ([f32::NAN, 10.0], [100.0, 100.0], [100, 100]),
    ] {
        assert_eq!(
            window_point_to_stretched_raster(position, viewport, raster),
            None,
            "位置 {position:?}、视口 {viewport:?}、光栅 {raster:?} 应当被拒绝"
        );
    }

    // 闭区间边界仍属于 surface 内部。
    assert_eq!(
        window_point_to_stretched_raster([0.0, 0.0], [100.0, 100.0], [200, 400]),
        Some([0.0, 0.0])
    );
    assert_eq!(
        window_point_to_stretched_raster([100.0, 100.0], [100.0, 100.0], [200, 400]),
        Some([200.0, 400.0])
    );
}

#[test]
fn contain_mapping_rejects_empty_raster_dimensions() {
    assert_eq!(
        window_point_to_raster([10.0, 10.0], [100.0, 100.0], [0, 100]),
        None
    );
    assert_eq!(
        window_point_to_raster([10.0, 10.0], [100.0, 100.0], [100, 0]),
        None
    );
    assert_eq!(
        window_point_to_raster([10.0, 10.0], [100.0, f32::NAN], [100, 100]),
        None
    );
}

#[test]
fn frames_without_hit_areas_never_report_a_hit() {
    let image = RenderImage::new(vec![Frame::new(RgbaImage::new(64, 64))]);
    let cpu_frame = RenderedModelFrame::new(image, Vec::new(), [64, 64]);
    let gpu_frame = RenderedModelFrame::gpu(Vec::new(), [64, 64]);

    assert!(cpu_frame.hit_areas().is_empty());
    assert!(gpu_frame.hit_areas().is_empty());
    assert!(cpu_frame.image().is_some());
    assert!(
        cpu_frame
            .hit_area_at_window_point([32.0, 32.0], [64.0, 64.0])
            .is_none()
    );
    assert!(
        gpu_frame
            .hit_area_at_window_point([32.0, 32.0], [64.0, 64.0])
            .is_none()
    );
}

#[test]
fn frames_with_a_degenerate_viewport_report_no_hit() {
    let area = RenderedHitArea::new(
        Arc::from("BodyDrawable"),
        Arc::from("Body"),
        [0.0, 0.0, 64.0, 64.0],
    )
    .expect("测试包围盒应当合法");
    let frame = RenderedModelFrame::gpu(vec![area], [64, 64]);

    assert!(
        frame
            .hit_area_at_window_point([32.0, 32.0], [0.0, 64.0])
            .is_none()
    );
}

#[test]
fn declaring_no_hit_areas_skips_per_frame_vertex_scanning() {
    assert!(render_hit_areas(&[], 4, &[], |position| position).is_empty());
}

fn quad_mesh(opacity: f32, corners: [[f32; 2]; 3]) -> Moc3DrawableMesh {
    Moc3DrawableMesh::from_parts(
        0,
        0,
        opacity,
        0.0,
        corners
            .into_iter()
            .map(|position| Moc3DrawableVertex::new(position, [0.0, 0.0]))
            .collect(),
        vec![0, 1, 2],
        Vec::new(),
    )
}

#[test]
fn bounds_follow_transformed_drawable_vertices() {
    let meshes = [quad_mesh(1.0, [[-1.0, -1.0], [1.0, -1.0], [0.0, 2.0]])];
    let hit_areas = [HitAreaCapability::new_for_test("D_Body", "Body", 0, 0)];

    // 模型坐标经过与渲染一致的变换后才写入包围盒。
    let rendered = render_hit_areas(&hit_areas, 1, &meshes, |[x, y]| {
        [x * 10.0 + 50.0, y * 10.0 + 50.0]
    });

    assert_eq!(rendered.len(), 1);
    assert_eq!(rendered[0].name(), "Body");
    assert!(rendered[0].contains([40.0, 40.0]));
    assert!(rendered[0].contains([60.0, 70.0]));
    assert!(!rendered[0].contains([39.9, 40.0]));
    assert!(!rendered[0].contains([60.0, 70.1]));
}

#[test]
fn fully_transparent_drawables_are_not_clickable() {
    let meshes = [quad_mesh(0.0, [[0.0, 0.0], [1.0, 0.0], [0.0, 1.0]])];
    let hit_areas = [HitAreaCapability::new_for_test("D_Body", "Body", 0, 0)];

    assert!(render_hit_areas(&hit_areas, 1, &meshes, |position| position).is_empty());

    // 非有限透明度同样视为不可见，避免损坏模型产生不可预期的命中区域。
    let broken = [quad_mesh(f32::NAN, [[0.0, 0.0], [1.0, 0.0], [0.0, 1.0]])];
    assert!(render_hit_areas(&hit_areas, 1, &broken, |position| position).is_empty());
}

#[test]
fn non_finite_transform_results_drop_the_whole_drawable() {
    let meshes = [quad_mesh(1.0, [[0.0, 0.0], [1.0, 0.0], [0.0, 1.0]])];
    let hit_areas = [HitAreaCapability::new_for_test("D_Body", "Body", 0, 0)];

    let rendered = render_hit_areas(&hit_areas, 1, &meshes, |[x, y]| {
        if y > 0.5 { [f32::NAN, y] } else { [x, y] }
    });

    assert!(rendered.is_empty());
}

#[test]
fn drawables_shared_by_several_hit_areas_are_scanned_once_per_frame() {
    let meshes = [quad_mesh(1.0, [[0.0, 0.0], [2.0, 0.0], [0.0, 2.0]])];
    let hit_areas = [
        HitAreaCapability::new_for_test("D_Body", "Body", 0, 0),
        HitAreaCapability::new_for_test("D_Body", "Head", 0, 0),
    ];
    let mut transformed = 0_usize;

    let rendered = render_hit_areas(&hit_areas, 1, &meshes, |position| {
        transformed += 1;
        position
    });

    assert_eq!(rendered.len(), 2);
    // 三个顶点只转换一次；第二个区域直接复用缓存的包围盒。
    assert_eq!(transformed, 3);
    assert!(rendered.iter().all(|area| area.contains([1.0, 1.0])));
}

#[test]
fn a_hidden_shared_drawable_is_skipped_for_every_referencing_hit_area() {
    let meshes = [quad_mesh(0.0, [[0.0, 0.0], [1.0, 0.0], [0.0, 1.0]])];
    let hit_areas = [
        HitAreaCapability::new_for_test("D_Body", "Body", 0, 0),
        HitAreaCapability::new_for_test("D_Body", "Head", 0, 0),
    ];
    let mut transformed = 0_usize;

    let rendered = render_hit_areas(&hit_areas, 1, &meshes, |position| {
        transformed += 1;
        position
    });

    assert!(rendered.is_empty());
    assert_eq!(transformed, 0, "透明 Drawable 不应进入顶点扫描");
}

#[test]
fn hit_areas_referencing_missing_drawables_or_slots_are_skipped() {
    let meshes = [quad_mesh(1.0, [[0.0, 0.0], [1.0, 0.0], [0.0, 1.0]])];
    let out_of_range_drawable = [HitAreaCapability::new_for_test(
        "D_Missing",
        "Missing",
        5,
        0,
    )];
    let out_of_range_slot = [HitAreaCapability::new_for_test("D_Body", "Body", 0, 9)];

    assert!(render_hit_areas(&out_of_range_drawable, 1, &meshes, |p| p).is_empty());
    assert!(render_hit_areas(&out_of_range_slot, 1, &meshes, |p| p).is_empty());
}

#[test]
fn drawables_without_vertices_produce_no_bounds() {
    let empty = Moc3DrawableMesh::from_parts(0, 0, 1.0, 0.0, Vec::new(), Vec::new(), Vec::new());
    let hit_areas = [HitAreaCapability::new_for_test("D_Body", "Body", 0, 0)];

    assert!(render_hit_areas(&hit_areas, 1, &[empty], |position| position).is_empty());
}

/// 逐帧包围盒需要 Mocari 从真实 `.moc3` 解析出的 Drawable 网格与顶点。LunaMate 没有
/// Live2D 模型的再分发授权，仓库不包含任何模型；请把自备模型放入 `models/` 后手动运行。
#[test]
#[ignore = "需要自备 Live2D 模型解析 Drawable 网格；无模型分发授权，请在本地放置模型后手动运行"]
fn rendered_hit_area_bounds_follow_visible_drawable_vertices() {
    use std::time::Duration;

    use crate::model::{AnimatedModel, RenderCancellation};

    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("models/hiyori_free/runtime/hiyori_free_t08.model3.json");

    let mut model = AnimatedModel::load(&path, 128, 128, RenderCancellation::default())
        .expect("自备测试模型应当可以加载");
    let first = model
        .render_frame(Duration::ZERO, [0.0, 0.0])
        .expect("首帧应当可以渲染");
    assert!(
        !first.hit_areas().is_empty(),
        "自备模型应当声明至少一个可用 HitArea"
    );

    // 包围盒来自当前帧顶点，命中点必须落在已渲染画面对应的光栅区域内。
    for hit_area in first.hit_areas() {
        assert!(
            first
                .hit_area_at_window_point([64.0, 64.0], [128.0, 128.0])
                .is_some()
                || !hit_area.contains([64.0, 64.0]),
            "命中检测必须与本帧包围盒一致"
        );
    }
}
