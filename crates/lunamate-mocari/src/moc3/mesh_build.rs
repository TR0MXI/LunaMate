use crate::core::Vector2;

use super::{
    Moc3ArtMeshKeyformInfo, Moc3ArtMeshKeyforms, Moc3ArtMeshes, Moc3Deformers, Moc3DrawableMesh,
    Moc3DrawableVertex, Moc3Glues, Moc3Ids, Moc3KeyformBindings, Moc3OffscreenInfo,
    build_moc3_drawable_mesh,
    compose::ComposedDeformers,
    keyform_bindings::{Moc3KeyformScratch, Moc3KeyformSlot},
};

const MAX_ART_MESH_INTERPOLATION_VALUES: usize = 4_000_000;
const MIN_PRUNABLE_VERTICES: usize = 1_024;
const MIN_PRUNABLE_VERTEX_PERCENT: usize = 25;

#[derive(Debug, Default)]
pub(crate) struct Moc3MeshUpdateScratch {
    positions: Vec<Vector2>,
    pub(crate) keyforms: Moc3KeyformScratch,
    composed: ComposedDeformers,
    pub(crate) drawable_part_opacities: Vec<f32>,
    drawable_slots: Vec<Vec<Moc3KeyformSlot>>,
    geometry_required: Vec<bool>,
}

impl Clone for Moc3MeshUpdateScratch {
    fn clone(&self) -> Self {
        // Scratch 不属于模型语义状态，克隆 runtime 时不复制可能达到百万控制点的高水位缓冲。
        Self::default()
    }
}

pub fn build_moc3_drawable_meshes_for_default_pose(
    art_meshes: &Moc3ArtMeshes,
    art_mesh_keyforms: &Moc3ArtMeshKeyforms,
    deformers: &Moc3Deformers,
    bindings: &Moc3KeyformBindings,
) -> Option<Vec<Moc3DrawableMesh>> {
    build_moc3_drawable_meshes_with_parameters(
        art_meshes,
        art_mesh_keyforms,
        deformers,
        bindings,
        bindings.parameter_default_values(),
    )
}

pub fn build_moc3_drawable_meshes_with_parameters(
    art_meshes: &Moc3ArtMeshes,
    art_mesh_keyforms: &Moc3ArtMeshKeyforms,
    deformers: &Moc3Deformers,
    bindings: &Moc3KeyformBindings,
    parameter_values: &[f32],
) -> Option<Vec<Moc3DrawableMesh>> {
    let composed = deformers.compose(bindings, parameter_values)?;
    let mut meshes = Vec::with_capacity(art_meshes.meshes().len());
    for art_mesh_index in 0..art_meshes.meshes().len() {
        meshes.push(build_moc3_drawable_mesh_for_pose(
            art_meshes,
            art_mesh_keyforms,
            &composed,
            bindings,
            parameter_values,
            art_mesh_index,
        )?);
    }

    Some(meshes)
}

pub fn build_moc3_drawable_meshes_for_default_pose_with_offscreen_state(
    art_meshes: &Moc3ArtMeshes,
    art_mesh_keyforms: &Moc3ArtMeshKeyforms,
    deformers: &Moc3Deformers,
    bindings: &Moc3KeyformBindings,
    ids: &Moc3Ids,
    offscreen: &Moc3OffscreenInfo,
) -> Option<Vec<Moc3DrawableMesh>> {
    build_moc3_drawable_meshes_with_parameters_and_offscreen_state(
        art_meshes,
        art_mesh_keyforms,
        deformers,
        bindings,
        ids,
        offscreen,
        bindings.parameter_default_values(),
    )
}

pub fn build_moc3_drawable_meshes_with_parameters_and_offscreen_state(
    art_meshes: &Moc3ArtMeshes,
    art_mesh_keyforms: &Moc3ArtMeshKeyforms,
    deformers: &Moc3Deformers,
    bindings: &Moc3KeyformBindings,
    ids: &Moc3Ids,
    offscreen: &Moc3OffscreenInfo,
    parameter_values: &[f32],
) -> Option<Vec<Moc3DrawableMesh>> {
    build_moc3_drawable_meshes_with_parameters_offscreen_and_part_opacities(
        art_meshes,
        art_mesh_keyforms,
        deformers,
        bindings,
        ids,
        offscreen,
        parameter_values,
        &[],
    )
}

#[allow(clippy::too_many_arguments)]
pub fn build_moc3_drawable_meshes_with_parameters_offscreen_and_part_opacities(
    art_meshes: &Moc3ArtMeshes,
    art_mesh_keyforms: &Moc3ArtMeshKeyforms,
    deformers: &Moc3Deformers,
    bindings: &Moc3KeyformBindings,
    ids: &Moc3Ids,
    offscreen: &Moc3OffscreenInfo,
    parameter_values: &[f32],
    drawable_part_opacities: &[f32],
) -> Option<Vec<Moc3DrawableMesh>> {
    let mut meshes = build_moc3_drawable_meshes_with_parameters(
        art_meshes,
        art_mesh_keyforms,
        deformers,
        bindings,
        parameter_values,
    )?;

    for (drawable_index, part_opacity) in drawable_part_opacities.iter().copied().enumerate() {
        let mesh = meshes.get_mut(drawable_index)?;
        mesh.set_opacity(mesh.opacity() * part_opacity);
    }

    for drawable_index in offscreen.effect_source_drawable_indices(ids) {
        meshes.get_mut(drawable_index)?.set_opacity(0.0);
    }

    Some(meshes)
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn update_moc3_drawable_meshes_with_parameters_offscreen_and_part_opacities(
    meshes: &mut [Moc3DrawableMesh],
    scratch: &mut Moc3MeshUpdateScratch,
    art_meshes: &Moc3ArtMeshes,
    art_mesh_keyforms: &Moc3ArtMeshKeyforms,
    deformers: &Moc3Deformers,
    bindings: &Moc3KeyformBindings,
    ids: &Moc3Ids,
    offscreen: &Moc3OffscreenInfo,
    glues: &Moc3Glues,
    parameter_values: &[f32],
) -> Option<()> {
    if meshes.len() != art_meshes.meshes().len() {
        return None;
    }
    if !should_prune_geometry(meshes, offscreen) {
        update_moc3_drawable_meshes_unpruned(
            meshes,
            scratch,
            art_meshes,
            art_mesh_keyforms,
            deformers,
            bindings,
            ids,
            offscreen,
            parameter_values,
        )?;
        return glues.apply_with_scratch(meshes, bindings, parameter_values, &mut scratch.keyforms);
    }

    deformers.compose_into(
        bindings,
        parameter_values,
        &mut scratch.keyforms,
        &mut scratch.composed,
    )?;
    reset_drawable_slots(&mut scratch.drawable_slots, meshes.len())?;
    for (art_mesh_index, mesh) in meshes.iter_mut().enumerate() {
        let keyform_count = art_mesh_keyforms.art_mesh_keyforms(art_mesh_index)?.len();
        let slots = bindings.keyform_slots_into(
            art_meshes.art_mesh_keyform_binding_band_index(art_mesh_index)?,
            keyform_count,
            parameter_values,
            &mut scratch.keyforms,
        )?;
        let drawable_slots = scratch.drawable_slots.get_mut(art_mesh_index)?;
        drawable_slots.clear();
        drawable_slots.try_reserve(slots.len()).ok()?;
        drawable_slots.extend_from_slice(slots);
        update_moc3_drawable_scalars_for_pose(
            mesh,
            art_meshes,
            art_mesh_keyforms,
            &scratch.composed,
            drawable_slots,
            art_mesh_index,
        )?;
    }

    for (drawable_index, part_opacity) in
        scratch.drawable_part_opacities.iter().copied().enumerate()
    {
        let mesh = meshes.get_mut(drawable_index)?;
        mesh.set_opacity(mesh.opacity() * part_opacity);
    }

    for drawable_index in offscreen.effect_source_drawable_indices(ids) {
        meshes.get_mut(drawable_index)?.set_opacity(0.0);
    }

    update_geometry_required(meshes, offscreen, glues, &mut scratch.geometry_required)?;
    for (art_mesh_index, mesh) in meshes.iter_mut().enumerate() {
        if !*scratch.geometry_required.get(art_mesh_index)? {
            continue;
        }
        update_moc3_drawable_geometry_for_pose(
            mesh,
            &mut scratch.positions,
            art_meshes,
            art_mesh_keyforms,
            &scratch.composed,
            scratch.drawable_slots.get(art_mesh_index)?,
            art_mesh_index,
        )?;
    }
    glues.apply_required_with_scratch(
        meshes,
        bindings,
        parameter_values,
        &mut scratch.keyforms,
        &scratch.geometry_required,
    )?;

    Some(())
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn update_moc3_drawable_meshes_unpruned(
    meshes: &mut [Moc3DrawableMesh],
    scratch: &mut Moc3MeshUpdateScratch,
    art_meshes: &Moc3ArtMeshes,
    art_mesh_keyforms: &Moc3ArtMeshKeyforms,
    deformers: &Moc3Deformers,
    bindings: &Moc3KeyformBindings,
    ids: &Moc3Ids,
    offscreen: &Moc3OffscreenInfo,
    parameter_values: &[f32],
) -> Option<()> {
    if meshes.len() != art_meshes.meshes().len() {
        return None;
    }
    deformers.compose_into(
        bindings,
        parameter_values,
        &mut scratch.keyforms,
        &mut scratch.composed,
    )?;
    for (art_mesh_index, mesh) in meshes.iter_mut().enumerate() {
        update_moc3_drawable_mesh_for_pose_unpruned(
            mesh,
            &mut scratch.positions,
            &mut scratch.keyforms,
            art_meshes,
            art_mesh_keyforms,
            &scratch.composed,
            bindings,
            parameter_values,
            art_mesh_index,
        )?;
    }
    for (drawable_index, part_opacity) in
        scratch.drawable_part_opacities.iter().copied().enumerate()
    {
        let mesh = meshes.get_mut(drawable_index)?;
        mesh.set_opacity(mesh.opacity() * part_opacity);
    }
    for drawable_index in offscreen.effect_source_drawable_indices(ids) {
        meshes.get_mut(drawable_index)?.set_opacity(0.0);
    }
    Some(())
}

#[allow(clippy::too_many_arguments)]
fn update_moc3_drawable_mesh_for_pose_unpruned(
    mesh: &mut Moc3DrawableMesh,
    positions: &mut Vec<Vector2>,
    keyform_scratch: &mut Moc3KeyformScratch,
    art_meshes: &Moc3ArtMeshes,
    art_mesh_keyforms: &Moc3ArtMeshKeyforms,
    composed: &ComposedDeformers,
    bindings: &Moc3KeyformBindings,
    parameter_values: &[f32],
    art_mesh_index: usize,
) -> Option<()> {
    let keyform_count = art_mesh_keyforms.art_mesh_keyforms(art_mesh_index)?.len();
    let slots = bindings.keyform_slots_into(
        art_meshes.art_mesh_keyform_binding_band_index(art_mesh_index)?,
        keyform_count,
        parameter_values,
        keyform_scratch,
    )?;
    update_moc3_drawable_scalars_for_pose(
        mesh,
        art_meshes,
        art_mesh_keyforms,
        composed,
        slots,
        art_mesh_index,
    )?;
    update_moc3_drawable_geometry_for_pose(
        mesh,
        positions,
        art_meshes,
        art_mesh_keyforms,
        composed,
        slots,
        art_mesh_index,
    )
}

fn should_prune_geometry(meshes: &[Moc3DrawableMesh], offscreen: &Moc3OffscreenInfo) -> bool {
    if offscreen.offscreen_count() != 0 {
        return false;
    }
    let mut total_vertices = 0_usize;
    let mut hidden_vertices = 0_usize;
    for mesh in meshes {
        let vertices = mesh.vertices().len();
        total_vertices = total_vertices.saturating_add(vertices);
        if mesh.opacity().is_finite() && mesh.opacity() <= 0.0 {
            hidden_vertices = hidden_vertices.saturating_add(vertices);
        }
    }
    hidden_vertices >= MIN_PRUNABLE_VERTICES
        && hidden_vertices.saturating_mul(100)
            >= total_vertices.saturating_mul(MIN_PRUNABLE_VERTEX_PERCENT)
}

#[cfg(test)]
pub(crate) fn should_prune_geometry_for_test(
    meshes: &[Moc3DrawableMesh],
    offscreen: &Moc3OffscreenInfo,
) -> bool {
    should_prune_geometry(meshes, offscreen)
}

fn reset_drawable_slots(slots: &mut Vec<Vec<Moc3KeyformSlot>>, count: usize) -> Option<()> {
    if slots.len() < count {
        slots.try_reserve(count - slots.len()).ok()?;
        slots.resize_with(count, Vec::new);
    } else {
        slots.truncate(count);
    }
    Some(())
}

fn update_geometry_required(
    meshes: &[Moc3DrawableMesh],
    offscreen: &Moc3OffscreenInfo,
    glues: &Moc3Glues,
    required: &mut Vec<bool>,
) -> Option<()> {
    required.clear();
    required.try_reserve(meshes.len()).ok()?;
    required.resize(meshes.len(), offscreen.offscreen_count() != 0);
    if offscreen.offscreen_count() != 0 {
        return Some(());
    }

    // 蒙版源自身可以完全透明，但其纹理 Alpha 和当前几何仍会裁剪可见目标；glue
    // 又会双向修改端点，因此从可见目标扩展出完整几何依赖闭包后才能安全跳过网格。
    for (drawable_index, mesh) in meshes.iter().enumerate() {
        if mesh.opacity() <= 0.0 {
            continue;
        }
        *required.get_mut(drawable_index)? = true;
        for &mask_index in mesh.masks() {
            let mask_index = usize::try_from(mask_index).ok()?;
            *required.get_mut(mask_index)? = true;
        }
    }
    glues.expand_geometry_required(required)
}

#[cfg(test)]
pub(crate) fn geometry_required_for_test(
    meshes: &[Moc3DrawableMesh],
    offscreen: &Moc3OffscreenInfo,
    glues: &Moc3Glues,
) -> Option<Vec<bool>> {
    let mut required = Vec::new();
    update_geometry_required(meshes, offscreen, glues, &mut required)?;
    Some(required)
}

fn build_moc3_drawable_mesh_for_pose(
    art_meshes: &Moc3ArtMeshes,
    art_mesh_keyforms: &Moc3ArtMeshKeyforms,
    composed: &ComposedDeformers,
    bindings: &Moc3KeyformBindings,
    parameter_values: &[f32],
    art_mesh_index: usize,
) -> Option<Moc3DrawableMesh> {
    let keyform_count = art_mesh_keyforms.art_mesh_keyforms(art_mesh_index)?.len();
    let slots = bindings.keyform_slots(
        art_meshes.art_mesh_keyform_binding_band_index(art_mesh_index)?,
        keyform_count,
        parameter_values,
    )?;
    let base_local_keyform_index = slots.first()?.local_index;
    let mesh = build_moc3_drawable_mesh(
        art_meshes,
        art_mesh_keyforms,
        art_mesh_index,
        base_local_keyform_index,
    )?;
    let parent_deformer_index = art_meshes.art_mesh_parent_deformer_index(art_mesh_index)?;
    let opacity = interpolate_art_mesh_opacity(art_mesh_keyforms, art_mesh_index, &slots)?
        * composed.deformer_opacity(parent_deformer_index);
    let draw_order = interpolate_art_mesh_draw_order(art_mesh_keyforms, art_mesh_index, &slots)?;
    let multiply_color =
        interpolate_art_mesh_color(art_mesh_keyforms, art_mesh_index, &slots, |k| {
            k.multiply_color()
        })?;
    let screen_color =
        interpolate_art_mesh_color(art_mesh_keyforms, art_mesh_index, &slots, |k| {
            k.screen_color()
        })?;
    let (parent_multiply_color, parent_screen_color) =
        composed.deformer_colors(parent_deformer_index);
    let multiply_color = combine_multiply_color(multiply_color, parent_multiply_color);
    let screen_color = combine_screen_color(screen_color, parent_screen_color);
    let mut positions = interpolate_art_mesh_positions(art_mesh_keyforms, art_mesh_index, &slots)?;

    composed.transform_vertices(parent_deformer_index, &mut positions)?;

    let vertices = mesh
        .vertices()
        .iter()
        .zip(positions)
        .map(|(vertex, position)| {
            Moc3DrawableVertex::new([position.x(), -position.y()], vertex.uv())
        })
        .collect();

    let mut mesh = Moc3DrawableMesh::from_parts_with_render_order(
        mesh.texture_index(),
        mesh.drawable_flags(),
        opacity,
        draw_order,
        mesh.render_order(),
        vertices,
        mesh.indices().to_vec(),
        mesh.masks().to_vec(),
    );
    mesh.set_multiply_color(multiply_color);
    mesh.set_screen_color(screen_color);
    Some(mesh)
}

#[allow(clippy::too_many_arguments)]
fn update_moc3_drawable_scalars_for_pose(
    mesh: &mut Moc3DrawableMesh,
    art_meshes: &Moc3ArtMeshes,
    art_mesh_keyforms: &Moc3ArtMeshKeyforms,
    composed: &ComposedDeformers,
    slots: &[Moc3KeyformSlot],
    art_mesh_index: usize,
) -> Option<()> {
    let parent_deformer_index = art_meshes.art_mesh_parent_deformer_index(art_mesh_index)?;
    let opacity = interpolate_art_mesh_opacity(art_mesh_keyforms, art_mesh_index, slots)?
        * composed.deformer_opacity(parent_deformer_index);
    let draw_order = interpolate_art_mesh_draw_order(art_mesh_keyforms, art_mesh_index, slots)?;
    let multiply_color =
        interpolate_art_mesh_color(art_mesh_keyforms, art_mesh_index, slots, |k| {
            k.multiply_color()
        })?;
    let screen_color = interpolate_art_mesh_color(art_mesh_keyforms, art_mesh_index, slots, |k| {
        k.screen_color()
    })?;
    let (parent_multiply_color, parent_screen_color) =
        composed.deformer_colors(parent_deformer_index);
    let multiply_color = combine_multiply_color(multiply_color, parent_multiply_color);
    let screen_color = combine_screen_color(screen_color, parent_screen_color);

    mesh.set_opacity(opacity);
    mesh.set_draw_order(draw_order);
    mesh.set_render_order(art_meshes.art_mesh_render_order(art_mesh_index)?);
    mesh.set_multiply_color(multiply_color);
    mesh.set_screen_color(screen_color);
    Some(())
}

fn update_moc3_drawable_geometry_for_pose(
    mesh: &mut Moc3DrawableMesh,
    positions: &mut Vec<Vector2>,
    art_meshes: &Moc3ArtMeshes,
    art_mesh_keyforms: &Moc3ArtMeshKeyforms,
    composed: &ComposedDeformers,
    slots: &[Moc3KeyformSlot],
    art_mesh_index: usize,
) -> Option<()> {
    interpolate_art_mesh_positions_into(art_mesh_keyforms, art_mesh_index, slots, positions)?;
    composed.transform_vertices(
        art_meshes.art_mesh_parent_deformer_index(art_mesh_index)?,
        positions,
    )?;
    if mesh.vertices().len() != positions.len() {
        return None;
    }
    for (vertex, position) in mesh.vertices_mut().iter_mut().zip(positions) {
        let uv = vertex.uv();
        *vertex = Moc3DrawableVertex::new([position.x(), -position.y()], uv);
    }
    Some(())
}

fn combine_multiply_color(local: [f32; 3], parent: [f32; 3]) -> [f32; 3] {
    [
        clamp01(local[0] * parent[0]),
        clamp01(local[1] * parent[1]),
        clamp01(local[2] * parent[2]),
    ]
}

fn combine_screen_color(local: [f32; 3], parent: [f32; 3]) -> [f32; 3] {
    [
        clamp01(local[0] + parent[0] - local[0] * parent[0]),
        clamp01(local[1] + parent[1] - local[1] * parent[1]),
        clamp01(local[2] + parent[2] - local[2] * parent[2]),
    ]
}

fn clamp01(value: f32) -> f32 {
    value.clamp(0.0, 1.0)
}

fn interpolate_art_mesh_color(
    keyforms: &Moc3ArtMeshKeyforms,
    art_mesh_index: usize,
    slots: &[Moc3KeyformSlot],
    channels: impl Fn(Moc3ArtMeshKeyformInfo) -> [f32; 3],
) -> Option<[f32; 3]> {
    let keyforms = keyforms.art_mesh_keyforms(art_mesh_index)?;
    let mut color = [0.0f32; 3];
    for slot in slots {
        let value = channels(*keyforms.get(slot.local_index)?);
        for (acc, channel) in color.iter_mut().zip(value) {
            *acc += channel * slot.weight;
        }
    }
    Some(color)
}

fn interpolate_art_mesh_positions(
    keyforms: &Moc3ArtMeshKeyforms,
    art_mesh_index: usize,
    slots: &[Moc3KeyformSlot],
) -> Option<Vec<Vector2>> {
    let mut out = Vec::new();
    interpolate_art_mesh_positions_into(keyforms, art_mesh_index, slots, &mut out)?;
    Some(out)
}

fn interpolate_art_mesh_positions_into(
    keyforms: &Moc3ArtMeshKeyforms,
    art_mesh_index: usize,
    slots: &[Moc3KeyformSlot],
    out: &mut Vec<Vector2>,
) -> Option<()> {
    let first = keyforms.art_mesh_keyform_positions(art_mesh_index, slots.first()?.local_index)?;
    let vertex_count = first.len().checked_div(2)?;
    if vertex_count.checked_mul(slots.len())? > MAX_ART_MESH_INTERPOLATION_VALUES {
        return None;
    }
    out.clear();
    out.try_reserve(vertex_count).ok()?;
    out.resize(vertex_count, Vector2::default());

    for slot in slots {
        let positions = keyforms.art_mesh_keyform_positions(art_mesh_index, slot.local_index)?;
        if positions.len() != first.len() || positions.len() % 2 != 0 {
            return None;
        }
        for (target, position) in out.iter_mut().zip(positions.chunks_exact(2)) {
            *target = Vector2::new(
                target.x() + position[0] * slot.weight,
                target.y() + position[1] * slot.weight,
            );
        }
    }

    Some(())
}

fn interpolate_art_mesh_opacity(
    keyforms: &Moc3ArtMeshKeyforms,
    art_mesh_index: usize,
    slots: &[Moc3KeyformSlot],
) -> Option<f32> {
    interpolate_art_mesh_scalar(keyforms, art_mesh_index, slots, |keyform| keyform.opacity())
}

fn interpolate_art_mesh_draw_order(
    keyforms: &Moc3ArtMeshKeyforms,
    art_mesh_index: usize,
    slots: &[Moc3KeyformSlot],
) -> Option<f32> {
    interpolate_art_mesh_scalar(keyforms, art_mesh_index, slots, |keyform| {
        keyform.draw_order()
    })
}

fn interpolate_art_mesh_scalar(
    keyforms: &Moc3ArtMeshKeyforms,
    art_mesh_index: usize,
    slots: &[Moc3KeyformSlot],
    value: impl Fn(Moc3ArtMeshKeyformInfo) -> f32,
) -> Option<f32> {
    let keyforms = keyforms.art_mesh_keyforms(art_mesh_index)?;
    let mut out = 0.0f32;
    for slot in slots {
        out += value(*keyforms.get(slot.local_index)?) * slot.weight;
    }
    Some(out)
}
