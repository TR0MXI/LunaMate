use mocari::{
    assets::DecodedTexture,
    moc3::{Moc3DrawableMesh, Moc3DrawableVertex},
};

use crate::model::live2d::{
    RenderCancellation,
    renderer::{
        CANCEL_CHECK_PIXELS, CpuRenderer, MAX_MASK_CONTEXTS, ModelTransform, checked_pixel_count,
        fill_cancelable, sorted_mask_sources, validate_gpu_frame, validate_gpu_model,
    },
};

fn test_mesh(draw_order: f32, render_order: i32, flags: u8, masks: Vec<i32>) -> Moc3DrawableMesh {
    Moc3DrawableMesh::from_parts_with_render_order(
        0,
        flags,
        1.0,
        draw_order,
        render_order,
        vec![
            Moc3DrawableVertex::new([-1.0, -1.0], [0.0, 0.0]),
            Moc3DrawableVertex::new([1.0, -1.0], [1.0, 0.0]),
            Moc3DrawableVertex::new([0.0, 1.0], [0.5, 1.0]),
        ],
        vec![0, 1, 2],
        masks,
    )
}

#[test]
fn mask_sources_ignore_drawable_order() {
    assert_eq!(
        sorted_mask_sources(&[5, 2, 3]).expect("有效蒙版索引应当可以排序"),
        vec![2, 3, 5]
    );
}

#[test]
fn raster_size_limits_reject_excessive_allocations() {
    assert!(checked_pixel_count(1_281, 1_281).is_err());
    assert!(checked_pixel_count(0, 128).is_err());
}

#[test]
fn gpu_transform_is_not_limited_by_cpu_raster_allocation() {
    let mesh = test_mesh(0.0, 0, 0, Vec::new());
    assert!(ModelTransform::fit(&[mesh], 2_000, 2_000, &RenderCancellation::default()).is_ok());
}

#[test]
fn identity_render_ranks_fall_back_to_dynamic_draw_order() {
    let meshes = vec![
        test_mesh(30.0, 0, 0, Vec::new()),
        test_mesh(10.0, 1, 0, Vec::new()),
        test_mesh(20.0, 2, 0, Vec::new()),
    ];
    let texture = DecodedTexture::new(1, 1, vec![255; 4]);
    let cancellation = RenderCancellation::default();
    let mut renderer = CpuRenderer::new(&meshes, &[texture], 4, 4, &cancellation)
        .expect("测试网格应能建立 CPU renderer");

    renderer.update_draw_order(&meshes);

    assert_eq!(renderer.draw_order, vec![1, 2, 0]);
}

#[test]
fn gpu_mask_limit_counts_normal_and_inverted_contexts_separately() {
    let source_count = MAX_MASK_CONTEXTS / 2 + 1;
    let mut meshes = (0..source_count)
        .map(|index| test_mesh(index as f32, index as i32, 0, Vec::new()))
        .collect::<Vec<_>>();
    for source in 0..source_count {
        let mask = vec![source as i32];
        meshes.push(test_mesh(100.0, 0, 0, mask.clone()));
        meshes.push(test_mesh(100.0, 0, 1 << 3, mask));
    }
    let texture = DecodedTexture::new(1, 1, vec![255; 4]);

    assert!(
        validate_gpu_model(
            &meshes,
            &[texture],
            100,
            100,
            &RenderCancellation::default()
        )
        .is_err()
    );
}

#[test]
fn gpu_frame_clamps_the_same_finite_opacity_as_cpu() {
    let mesh = Moc3DrawableMesh::from_parts(
        0,
        0,
        2.0,
        0.0,
        vec![
            Moc3DrawableVertex::new([-1.0, -1.0], [0.0, 0.0]),
            Moc3DrawableVertex::new([1.0, -1.0], [1.0, 0.0]),
            Moc3DrawableVertex::new([0.0, 1.0], [0.5, 1.0]),
        ],
        vec![0, 1, 2],
        Vec::new(),
    );

    assert!(validate_gpu_frame(&[mesh], &RenderCancellation::default()).is_ok());
}

#[test]
fn gpu_frame_still_rejects_non_finite_dynamic_values() {
    let mesh = Moc3DrawableMesh::from_parts(
        0,
        0,
        f32::NAN,
        0.0,
        vec![
            Moc3DrawableVertex::new([-1.0, -1.0], [0.0, 0.0]),
            Moc3DrawableVertex::new([1.0, -1.0], [1.0, 0.0]),
            Moc3DrawableVertex::new([0.0, 1.0], [0.5, 1.0]),
        ],
        vec![0, 1, 2],
        Vec::new(),
    );

    assert!(validate_gpu_frame(&[mesh], &RenderCancellation::default()).is_err());
}

#[test]
fn cancelled_buffer_work_returns_a_distinct_error() {
    let cancellation = RenderCancellation::default();
    let mut pixels = vec![[1.0; 4]; CANCEL_CHECK_PIXELS * 2];
    cancellation.cancel();

    let error = fill_cancelable(&mut pixels, [0.0; 4], &cancellation)
        .expect_err("已取消缓冲处理必须立即退出");
    assert!(error.is_cancelled());
    assert!(pixels.iter().all(|pixel| *pixel == [1.0; 4]));
}
