use crate::moc3::{
    Moc3DrawableMesh, Moc3DrawableVertex, Moc3Glues, Moc3OffscreenInfo, geometry_required_for_test,
    should_prune_geometry_for_test,
};

#[test]
fn invisible_mask_source_keeps_current_geometry() {
    let meshes = vec![
        mesh(1.0, vec![1]),
        mesh(0.0, Vec::new()),
        mesh(0.0, Vec::new()),
    ];
    let offscreen = Moc3OffscreenInfo::from_parts(
        vec![-1; meshes.len()],
        vec![-1; meshes.len()],
        vec![-1; meshes.len()],
        Vec::new(),
    );
    let required = geometry_required_for_test(&meshes, &offscreen, &empty_glues())
        .expect("固定测试几何依赖应有效");

    assert_eq!(required, vec![true, true, false]);
}

#[test]
fn glue_dependency_expands_transitively_from_visible_mesh() {
    let glues = Moc3Glues::from_parts(
        vec![0, 0, 0],
        vec![0, 0, 0],
        vec![0, 0, 0],
        vec![0, 1, 3],
        vec![1, 2, 4],
        vec![0, 2, 4],
        vec![2, 2, 2],
        vec![1.0; 6],
        vec![0; 6],
        Vec::new(),
    )
    .expect("固定测试 glue 元数据应有效");
    let mut required = vec![true, false, false, false, false];

    glues
        .expand_geometry_required(&mut required)
        .expect("固定测试 glue 闭包应有效");

    assert_eq!(required, vec![true, true, true, false, false]);
}

#[test]
fn offscreen_model_conservatively_keeps_all_geometry() {
    let meshes = vec![mesh(0.0, Vec::new()), mesh(0.0, Vec::new())];
    let offscreen = Moc3OffscreenInfo::from_parts(
        vec![-1; meshes.len()],
        vec![-1; meshes.len()],
        vec![0, -1],
        vec![0],
    );
    let required = geometry_required_for_test(&meshes, &offscreen, &empty_glues())
        .expect("固定测试 offscreen 依赖应有效");

    assert_eq!(required, vec![true, true]);
}

#[test]
fn pruning_requires_substantial_hidden_geometry() {
    let offscreen = no_offscreen(10);
    let all_visible = (0..10)
        .map(|_| mesh_with_vertices(1.0, 500))
        .collect::<Vec<_>>();
    let lightly_hidden = (0..10)
        .map(|index| mesh_with_vertices(if index < 2 { 0.0 } else { 1.0 }, 500))
        .collect::<Vec<_>>();
    let substantially_hidden = (0..10)
        .map(|index| mesh_with_vertices(if index < 3 { 0.0 } else { 1.0 }, 500))
        .collect::<Vec<_>>();

    assert!(!should_prune_geometry_for_test(&all_visible, &offscreen));
    assert!(!should_prune_geometry_for_test(&lightly_hidden, &offscreen));
    assert!(should_prune_geometry_for_test(
        &substantially_hidden,
        &offscreen
    ));
}

fn mesh(opacity: f32, masks: Vec<i32>) -> Moc3DrawableMesh {
    Moc3DrawableMesh::from_parts(0, 0, opacity, 0.0, Vec::new(), Vec::new(), masks)
}

fn mesh_with_vertices(opacity: f32, vertex_count: usize) -> Moc3DrawableMesh {
    Moc3DrawableMesh::from_parts(
        0,
        0,
        opacity,
        0.0,
        vec![Moc3DrawableVertex::new([0.0; 2], [0.0; 2]); vertex_count],
        Vec::new(),
        Vec::new(),
    )
}

fn no_offscreen(drawable_count: usize) -> Moc3OffscreenInfo {
    Moc3OffscreenInfo::from_parts(
        vec![-1; drawable_count],
        vec![-1; drawable_count],
        vec![-1; drawable_count],
        Vec::new(),
    )
}

fn empty_glues() -> Moc3Glues {
    Moc3Glues::from_parts(
        Vec::new(),
        Vec::new(),
        Vec::new(),
        Vec::new(),
        Vec::new(),
        Vec::new(),
        Vec::new(),
        Vec::new(),
        Vec::new(),
        Vec::new(),
    )
    .expect("空 glue 元数据应有效")
}
