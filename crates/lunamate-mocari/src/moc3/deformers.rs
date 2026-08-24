use crate::{Result, core::Vector2};

use super::{
    Moc3CountInfo, Moc3Header, Moc3SectionOffsets,
    compose::{
        ComposedDeformer, ComposedDeformers, ComposedRotation, ComposedWarp, composed_parent_apply,
        parent_colors, parent_opacity_accum, parent_rotation_angle, parent_scale_accum,
    },
    keyform_bindings::{Moc3KeyformBindings, Moc3KeyformScratch, Moc3KeyformSlot},
    parse::{
        invalid_moc3, read_bool_section, read_f32_section, read_f32_section_or_default,
        read_i32_section, read_i32_section_or_default, to_usize,
    },
};

const MAX_WARP_GRID_VERTICES: usize = 1_000_000;
const MAX_TOTAL_WARP_GRID_VERTICES: usize = 1_000_000;
const MAX_WARP_INTERPOLATION_VALUES: usize = 4_000_000;

const DEFORMER_PARENT_DEFORMER_INDICES_SLOT: usize = 16;
const DEFORMER_TYPES_SLOT: usize = 17;
const DEFORMER_SPECIFIC_INDICES_SLOT: usize = 18;
const WARP_KEYFORM_BINDING_BAND_INDICES_SLOT: usize = 19;
const WARP_KEYFORM_BEGIN_INDICES_SLOT: usize = 20;
const WARP_KEYFORM_COUNTS_SLOT: usize = 21;
const WARP_VERTEX_COUNTS_SLOT: usize = 22;
const WARP_ROWS_SLOT: usize = 23;
const WARP_COLS_SLOT: usize = 24;
const ROTATION_KEYFORM_BINDING_BAND_INDICES_SLOT: usize = 25;
const ROTATION_KEYFORM_BEGIN_INDICES_SLOT: usize = 26;
const ROTATION_KEYFORM_COUNTS_SLOT: usize = 27;
const ROTATION_BASE_ANGLES_SLOT: usize = 28;
const WARP_KEYFORM_OPACITIES_SLOT: usize = 59;
const WARP_KEYFORM_POSITION_BEGIN_INDICES_SLOT: usize = 60;
const ROTATION_KEYFORM_OPACITIES_SLOT: usize = 61;
const ROTATION_KEYFORM_ANGLES_SLOT: usize = 62;
const ROTATION_KEYFORM_ORIGIN_XS_SLOT: usize = 63;
const ROTATION_KEYFORM_ORIGIN_YS_SLOT: usize = 64;
const ROTATION_KEYFORM_SCALES_SLOT: usize = 65;
const ROTATION_KEYFORM_REFLECT_XS_SLOT: usize = 66;
const ROTATION_KEYFORM_REFLECT_YS_SLOT: usize = 67;
const KEYFORM_POSITION_XYS_SLOT: usize = 71;
const WARP_KEYFORM_COLOR_BEGIN_INDICES_SLOT: usize = 105;
const ROTATION_KEYFORM_COLOR_BEGIN_INDICES_SLOT: usize = 106;
const KEYFORM_MULTIPLY_COLOR_SLOTS: [usize; 3] = [108, 109, 110];
const KEYFORM_SCREEN_COLOR_SLOTS: [usize; 3] = [111, 112, 113];

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
enum Moc3DeformerKind {
    Warp,
    Rotation,
}

impl Moc3DeformerKind {
    fn from_raw(value: i32) -> Option<Self> {
        match value {
            0 => Some(Self::Warp),
            1 => Some(Self::Rotation),
            _ => None,
        }
    }
}

#[derive(Debug, Copy, Clone, PartialEq)]
struct InterpolatedRotation {
    angle_degrees: f32,
    translation: Vector2,
    scale: f32,
    flip_x: bool,
    flip_y: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Moc3Deformers {
    parent_deformer_indices: Vec<i32>,
    composition_order: Vec<usize>,
    deformer_kinds: Vec<Moc3DeformerKind>,
    specific_indices: Vec<i32>,
    warp_keyform_binding_band_indices: Vec<i32>,
    warp_keyform_begin_indices: Vec<i32>,
    warp_keyform_counts: Vec<i32>,
    warp_vertex_counts: Vec<i32>,
    warp_rows: Vec<i32>,
    warp_cols: Vec<i32>,
    warp_keyform_opacities: Vec<f32>,
    rotation_keyform_binding_band_indices: Vec<i32>,
    rotation_keyform_begin_indices: Vec<i32>,
    rotation_keyform_counts: Vec<i32>,
    rotation_base_angles: Vec<f32>,
    warp_keyform_position_begin_indices: Vec<i32>,
    rotation_keyform_angles: Vec<f32>,
    rotation_keyform_origin_xs: Vec<f32>,
    rotation_keyform_origin_ys: Vec<f32>,
    rotation_keyform_scales: Vec<f32>,
    rotation_keyform_reflect_xs: Vec<bool>,
    rotation_keyform_reflect_ys: Vec<bool>,
    rotation_keyform_opacities: Vec<f32>,
    keyform_position_xys: Vec<f32>,
    warp_keyform_color_begin_indices: Vec<i32>,
    rotation_keyform_color_begin_indices: Vec<i32>,
    keyform_multiply_colors: [Vec<f32>; 3],
    keyform_screen_colors: [Vec<f32>; 3],
}

impl Moc3Deformers {
    pub fn parse(bytes: &[u8]) -> Result<Self> {
        let header = Moc3Header::parse(bytes)?;
        let offsets = Moc3SectionOffsets::parse(bytes)?;
        let counts = Moc3CountInfo::parse(bytes)?;
        let endianness = header.endianness();
        let deformer_count = to_usize(counts.deformers(), "deformer count")?;
        let warp_count = to_usize(counts.warp_deformers(), "warp deformer count")?;
        let rotation_count = to_usize(counts.rotation_deformers(), "rotation deformer count")?;
        let warp_keyform_count = to_usize(
            counts.warp_deformer_keyforms(),
            "warp deformer keyform count",
        )?;
        let rotation_keyform_count = to_usize(
            counts.rotation_deformer_keyforms(),
            "rotation deformer keyform count",
        )?;
        let keyform_multiply_color_count = to_usize(
            counts.keyform_multiply_colors(),
            "keyform multiply color count",
        )?;
        let keyform_screen_color_count =
            to_usize(counts.keyform_screen_colors(), "keyform screen color count")?;

        let deformer_types = read_i32_section(
            bytes,
            &offsets,
            DEFORMER_TYPES_SLOT,
            deformer_count,
            endianness,
        )?;
        let deformer_kinds = deformer_types
            .iter()
            .copied()
            .map(|value| {
                Moc3DeformerKind::from_raw(value)
                    .ok_or_else(|| invalid_moc3(format!("unsupported deformer type {value}")))
            })
            .collect::<Result<Vec<_>>>()?;
        let parent_deformer_indices = read_i32_section(
            bytes,
            &offsets,
            DEFORMER_PARENT_DEFORMER_INDICES_SLOT,
            deformer_count,
            endianness,
        )?;
        let composition_order = composition_order(&parent_deformer_indices);

        Ok(Self {
            parent_deformer_indices,
            composition_order,
            deformer_kinds,
            specific_indices: read_i32_section(
                bytes,
                &offsets,
                DEFORMER_SPECIFIC_INDICES_SLOT,
                deformer_count,
                endianness,
            )?,
            warp_keyform_binding_band_indices: read_i32_section(
                bytes,
                &offsets,
                WARP_KEYFORM_BINDING_BAND_INDICES_SLOT,
                warp_count,
                endianness,
            )?,
            warp_keyform_begin_indices: read_i32_section(
                bytes,
                &offsets,
                WARP_KEYFORM_BEGIN_INDICES_SLOT,
                warp_count,
                endianness,
            )?,
            warp_keyform_counts: read_i32_section(
                bytes,
                &offsets,
                WARP_KEYFORM_COUNTS_SLOT,
                warp_count,
                endianness,
            )?,
            warp_vertex_counts: read_i32_section(
                bytes,
                &offsets,
                WARP_VERTEX_COUNTS_SLOT,
                warp_count,
                endianness,
            )?,
            warp_rows: read_i32_section(bytes, &offsets, WARP_ROWS_SLOT, warp_count, endianness)?,
            warp_cols: read_i32_section(bytes, &offsets, WARP_COLS_SLOT, warp_count, endianness)?,
            warp_keyform_opacities: read_f32_section_or_default(
                bytes,
                &offsets,
                WARP_KEYFORM_OPACITIES_SLOT,
                warp_keyform_count,
                endianness,
                1.0,
            )?,
            rotation_keyform_binding_band_indices: read_i32_section(
                bytes,
                &offsets,
                ROTATION_KEYFORM_BINDING_BAND_INDICES_SLOT,
                rotation_count,
                endianness,
            )?,
            rotation_keyform_begin_indices: read_i32_section(
                bytes,
                &offsets,
                ROTATION_KEYFORM_BEGIN_INDICES_SLOT,
                rotation_count,
                endianness,
            )?,
            rotation_keyform_counts: read_i32_section(
                bytes,
                &offsets,
                ROTATION_KEYFORM_COUNTS_SLOT,
                rotation_count,
                endianness,
            )?,
            rotation_base_angles: read_f32_section(
                bytes,
                &offsets,
                ROTATION_BASE_ANGLES_SLOT,
                rotation_count,
                endianness,
            )?,
            warp_keyform_position_begin_indices: read_i32_section(
                bytes,
                &offsets,
                WARP_KEYFORM_POSITION_BEGIN_INDICES_SLOT,
                warp_keyform_count,
                endianness,
            )?,
            rotation_keyform_angles: read_f32_section(
                bytes,
                &offsets,
                ROTATION_KEYFORM_ANGLES_SLOT,
                rotation_keyform_count,
                endianness,
            )?,
            rotation_keyform_origin_xs: read_f32_section(
                bytes,
                &offsets,
                ROTATION_KEYFORM_ORIGIN_XS_SLOT,
                rotation_keyform_count,
                endianness,
            )?,
            rotation_keyform_origin_ys: read_f32_section(
                bytes,
                &offsets,
                ROTATION_KEYFORM_ORIGIN_YS_SLOT,
                rotation_keyform_count,
                endianness,
            )?,
            rotation_keyform_scales: read_f32_section(
                bytes,
                &offsets,
                ROTATION_KEYFORM_SCALES_SLOT,
                rotation_keyform_count,
                endianness,
            )?,
            rotation_keyform_reflect_xs: read_bool_section(
                bytes,
                &offsets,
                ROTATION_KEYFORM_REFLECT_XS_SLOT,
                rotation_keyform_count,
                endianness,
            )?,
            rotation_keyform_reflect_ys: read_bool_section(
                bytes,
                &offsets,
                ROTATION_KEYFORM_REFLECT_YS_SLOT,
                rotation_keyform_count,
                endianness,
            )?,
            rotation_keyform_opacities: read_f32_section_or_default(
                bytes,
                &offsets,
                ROTATION_KEYFORM_OPACITIES_SLOT,
                rotation_keyform_count,
                endianness,
                1.0,
            )?,
            keyform_position_xys: read_f32_section(
                bytes,
                &offsets,
                KEYFORM_POSITION_XYS_SLOT,
                to_usize(counts.keyform_positions(), "keyform position count")?,
                endianness,
            )?,
            warp_keyform_color_begin_indices: read_i32_section_or_default(
                bytes,
                &offsets,
                WARP_KEYFORM_COLOR_BEGIN_INDICES_SLOT,
                warp_count,
                endianness,
                -1,
            )?,
            rotation_keyform_color_begin_indices: read_i32_section_or_default(
                bytes,
                &offsets,
                ROTATION_KEYFORM_COLOR_BEGIN_INDICES_SLOT,
                rotation_count,
                endianness,
                -1,
            )?,
            keyform_multiply_colors: read_color_channels(
                bytes,
                &offsets,
                KEYFORM_MULTIPLY_COLOR_SLOTS,
                keyform_multiply_color_count,
                endianness,
                1.0,
            )?,
            keyform_screen_colors: read_color_channels(
                bytes,
                &offsets,
                KEYFORM_SCREEN_COLOR_SLOTS,
                keyform_screen_color_count,
                endianness,
                0.0,
            )?,
        })
    }

    pub(super) fn compose(
        &self,
        bindings: &Moc3KeyformBindings,
        parameter_values: &[f32],
    ) -> Option<ComposedDeformers> {
        let mut keyform_scratch = Moc3KeyformScratch::default();
        let mut composed = ComposedDeformers::default();
        self.compose_into(
            bindings,
            parameter_values,
            &mut keyform_scratch,
            &mut composed,
        )?;
        Some(composed)
    }

    pub(super) fn compose_into(
        &self,
        bindings: &Moc3KeyformBindings,
        parameter_values: &[f32],
        keyform_scratch: &mut Moc3KeyformScratch,
        composed: &mut ComposedDeformers,
    ) -> Option<()> {
        let count = self.deformer_kinds.len();
        composed.reset_with(count, |index| {
            Some(match self.deformer_kinds.get(index)? {
                Moc3DeformerKind::Warp => ComposedDeformer::Warp(ComposedWarp {
                    grid: Vec::new(),
                    cols: 0,
                    rows: 0,
                    scale_accum: 1.0,
                    opacity_accum: 1.0,
                    multiply_color: [1.0; 3],
                    screen_color: [0.0; 3],
                }),
                Moc3DeformerKind::Rotation => ComposedDeformer::Rotation(ComposedRotation {
                    origin: Vector2::default(),
                    angle_degrees: 0.0,
                    scale: 1.0,
                    flip_x: false,
                    flip_y: false,
                    scale_accum: 1.0,
                    opacity_accum: 1.0,
                    multiply_color: [1.0; 3],
                    screen_color: [0.0; 3],
                }),
            })
        })?;
        let mut total_warp_vertices = 0_usize;
        for &idx in &self.composition_order {
            let parent = *self.parent_deformer_indices.get(idx)?;
            let specific = usize::try_from(*self.specific_indices.get(idx)?).ok()?;
            match *self.deformer_kinds.get(idx)? {
                Moc3DeformerKind::Warp => {
                    let slots = self.warp_keyform_slots_into(
                        specific,
                        bindings,
                        parameter_values,
                        keyform_scratch,
                    )?;
                    let cols = usize::try_from(*self.warp_cols.get(specific)?).ok()?;
                    let rows = usize::try_from(*self.warp_rows.get(specific)?).ok()?;
                    let grid_len = {
                        let ComposedDeformer::Warp(warp) = composed.slot_mut(idx)? else {
                            return None;
                        };
                        self.interpolated_warp_grid_into(specific, slots, &mut warp.grid)?;
                        warp.grid.len()
                    };
                    total_warp_vertices = total_warp_vertices.checked_add(grid_len)?;
                    if total_warp_vertices > MAX_TOTAL_WARP_GRID_VERTICES {
                        return None;
                    }
                    // 控制点数量通常远多于美术网格顶点，父变形器只解析一次并批量应用。
                    // 网格为空时跳过，保持与逐点实现一致的失败条件。
                    composed.apply_parent_to_warp_grid(idx, parent)?;
                    let scale_accum = parent_scale_accum(composed, parent);
                    let opacity = self.interpolated_warp_opacity(specific, slots)?;
                    let opacity_accum = opacity * parent_opacity_accum(composed, parent);
                    let (multiply_color, screen_color) = compose_colors(
                        self.interpolated_warp_colors(specific, slots)?,
                        parent_colors(composed, parent),
                    );
                    let ComposedDeformer::Warp(warp) = composed.slot_mut(idx)? else {
                        return None;
                    };
                    warp.cols = cols;
                    warp.rows = rows;
                    warp.scale_accum = scale_accum;
                    warp.opacity_accum = opacity_accum;
                    warp.multiply_color = multiply_color;
                    warp.screen_color = screen_color;
                }
                Moc3DeformerKind::Rotation => {
                    let slots = self.rotation_keyform_slots_into(
                        specific,
                        bindings,
                        parameter_values,
                        keyform_scratch,
                    )?;
                    let rotation = self.interpolated_rotation(specific, slots)?;
                    let origin =
                        composed_parent_apply(composed, parent)?.apply(rotation.translation)?;
                    let parent_angle =
                        parent_rotation_angle(composed, parent, origin, rotation.translation)?;
                    let scale_accum = parent_scale_accum(composed, parent);
                    let opacity = self.interpolated_rotation_opacity(specific, slots)?;
                    let opacity_accum = opacity * parent_opacity_accum(composed, parent);
                    let (multiply_color, screen_color) = compose_colors(
                        self.interpolated_rotation_colors(specific, slots)?,
                        parent_colors(composed, parent),
                    );
                    *composed.slot_mut(idx)? = ComposedDeformer::Rotation(ComposedRotation {
                        origin,
                        angle_degrees: rotation.angle_degrees + parent_angle.to_degrees(),
                        scale: rotation.scale * scale_accum,
                        flip_x: rotation.flip_x,
                        flip_y: rotation.flip_y,
                        scale_accum: rotation.scale * scale_accum,
                        opacity_accum,
                        multiply_color,
                        screen_color,
                    });
                }
            }
            composed.mark_valid(idx)?;
        }

        composed.is_complete().then_some(())
    }

    fn warp_keyform_slots_into<'a>(
        &self,
        warp_index: usize,
        bindings: &Moc3KeyformBindings,
        parameter_values: &[f32],
        scratch: &'a mut Moc3KeyformScratch,
    ) -> Option<&'a [Moc3KeyformSlot]> {
        let keyform_count = usize::try_from(*self.warp_keyform_counts.get(warp_index)?).ok()?;
        bindings.keyform_slots_into(
            *self.warp_keyform_binding_band_indices.get(warp_index)?,
            keyform_count,
            parameter_values,
            scratch,
        )
    }

    fn rotation_keyform_slots_into<'a>(
        &self,
        rotation_index: usize,
        bindings: &Moc3KeyformBindings,
        parameter_values: &[f32],
        scratch: &'a mut Moc3KeyformScratch,
    ) -> Option<&'a [Moc3KeyformSlot]> {
        let keyform_count =
            usize::try_from(*self.rotation_keyform_counts.get(rotation_index)?).ok()?;
        bindings.keyform_slots_into(
            *self
                .rotation_keyform_binding_band_indices
                .get(rotation_index)?,
            keyform_count,
            parameter_values,
            scratch,
        )
    }

    fn interpolated_warp_grid_into(
        &self,
        warp_index: usize,
        slots: &[Moc3KeyformSlot],
        grid: &mut Vec<Vector2>,
    ) -> Option<()> {
        let begin = usize::try_from(*self.warp_keyform_begin_indices.get(warp_index)?).ok()?;
        let vertex_count = usize::try_from(*self.warp_vertex_counts.get(warp_index)?).ok()?;
        if vertex_count > MAX_WARP_GRID_VERTICES {
            return None;
        }
        if vertex_count.checked_mul(slots.len())? > MAX_WARP_INTERPOLATION_VALUES {
            return None;
        }
        grid.clear();
        grid.try_reserve(vertex_count).ok()?;

        let first_slot = slots.first()?;
        let keyform_index = begin.checked_add(first_slot.local_index)?;
        let source = self.warp_grid_values(warp_index, keyform_index)?;
        if source.len() != vertex_count.checked_mul(2)? {
            return None;
        }
        for source in source.as_chunks::<2>().0 {
            grid.push(Vector2::new(
                0.0 + source[0] * first_slot.weight,
                0.0 + source[1] * first_slot.weight,
            ));
        }

        for slot in &slots[1..] {
            let keyform_index = begin.checked_add(slot.local_index)?;
            let source = self.warp_grid_values(warp_index, keyform_index)?;
            if source.len() != grid.len().checked_mul(2)? {
                return None;
            }
            for (target, source) in grid.iter_mut().zip(source.as_chunks::<2>().0) {
                *target = Vector2::new(
                    target.x() + source[0] * slot.weight,
                    target.y() + source[1] * slot.weight,
                );
            }
        }

        Some(())
    }

    fn interpolated_rotation(
        &self,
        rotation_index: usize,
        slots: &[Moc3KeyformSlot],
    ) -> Option<InterpolatedRotation> {
        let begin =
            usize::try_from(*self.rotation_keyform_begin_indices.get(rotation_index)?).ok()?;
        let mut angle = 0.0f32;
        let mut translation = Vector2::default();
        let mut scale = 0.0f32;
        let mut flip_x = 0.0f32;
        let mut flip_y = 0.0f32;

        for slot in slots {
            let keyform_index = begin.checked_add(slot.local_index)?;
            angle += *self.rotation_keyform_angles.get(keyform_index)? * slot.weight;
            translation = Vector2::new(
                translation.x()
                    + *self.rotation_keyform_origin_xs.get(keyform_index)? * slot.weight,
                translation.y()
                    + *self.rotation_keyform_origin_ys.get(keyform_index)? * slot.weight,
            );
            scale += *self.rotation_keyform_scales.get(keyform_index)? * slot.weight;
            flip_x += u8::from(*self.rotation_keyform_reflect_xs.get(keyform_index)?) as f32
                * slot.weight;
            flip_y += u8::from(*self.rotation_keyform_reflect_ys.get(keyform_index)?) as f32
                * slot.weight;
        }

        Some(InterpolatedRotation {
            angle_degrees: *self.rotation_base_angles.get(rotation_index)? + angle,
            translation,
            scale,
            flip_x: interpolate_bool(flip_x),
            flip_y: interpolate_bool(flip_y),
        })
    }

    fn interpolated_warp_opacity(
        &self,
        warp_index: usize,
        slots: &[Moc3KeyformSlot],
    ) -> Option<f32> {
        let begin = usize::try_from(*self.warp_keyform_begin_indices.get(warp_index)?).ok()?;
        let mut opacity = 0.0f32;
        for slot in slots {
            let keyform_index = begin.checked_add(slot.local_index)?;
            opacity += *self.warp_keyform_opacities.get(keyform_index)? * slot.weight;
        }
        Some(opacity)
    }

    fn interpolated_warp_colors(
        &self,
        warp_index: usize,
        slots: &[Moc3KeyformSlot],
    ) -> Option<([f32; 3], [f32; 3])> {
        let begin = *self.warp_keyform_color_begin_indices.get(warp_index)?;
        interpolate_colors(
            begin,
            slots,
            &self.keyform_multiply_colors,
            &self.keyform_screen_colors,
        )
    }

    fn interpolated_rotation_opacity(
        &self,
        rotation_index: usize,
        slots: &[Moc3KeyformSlot],
    ) -> Option<f32> {
        let begin =
            usize::try_from(*self.rotation_keyform_begin_indices.get(rotation_index)?).ok()?;
        let mut opacity = 0.0f32;
        for slot in slots {
            let keyform_index = begin.checked_add(slot.local_index)?;
            opacity += *self.rotation_keyform_opacities.get(keyform_index)? * slot.weight;
        }
        Some(opacity)
    }

    fn interpolated_rotation_colors(
        &self,
        rotation_index: usize,
        slots: &[Moc3KeyformSlot],
    ) -> Option<([f32; 3], [f32; 3])> {
        let begin = *self
            .rotation_keyform_color_begin_indices
            .get(rotation_index)?;
        interpolate_colors(
            begin,
            slots,
            &self.keyform_multiply_colors,
            &self.keyform_screen_colors,
        )
    }

    fn warp_grid_values(&self, warp_index: usize, keyform_index: usize) -> Option<&[f32]> {
        let start = usize::try_from(
            *self
                .warp_keyform_position_begin_indices
                .get(keyform_index)?,
        )
        .ok()?;
        let vertex_count = usize::try_from(*self.warp_vertex_counts.get(warp_index)?).ok()?;
        let len = vertex_count.checked_mul(2)?;
        self.keyform_position_xys
            .get(start..start.checked_add(len)?)
    }

    #[cfg(any(test, feature = "benchmark-support"))]
    fn benchmark_warp_chain(chain_length: usize, cols: usize, rows: usize) -> Option<Self> {
        const KEYFORM_COUNT: usize = 2;

        if chain_length == 0 || cols == 0 || rows == 0 {
            return None;
        }
        let vertex_count = cols.checked_add(1)?.checked_mul(rows.checked_add(1)?)?;
        if vertex_count > MAX_WARP_GRID_VERTICES
            || vertex_count.checked_mul(chain_length)? > MAX_TOTAL_WARP_GRID_VERTICES
            || vertex_count.checked_mul(KEYFORM_COUNT)? > MAX_WARP_INTERPOLATION_VALUES
        {
            return None;
        }
        let vertex_count_i32 = i32::try_from(vertex_count).ok()?;
        let cols_i32 = i32::try_from(cols).ok()?;
        let rows_i32 = i32::try_from(rows).ok()?;
        let mut keyform_position_xys = Vec::new();
        keyform_position_xys
            .try_reserve_exact(
                vertex_count
                    .checked_mul(chain_length)?
                    .checked_mul(KEYFORM_COUNT)?
                    .checked_mul(2)?,
            )
            .ok()?;
        let mut position_begins = Vec::new();
        position_begins
            .try_reserve_exact(chain_length.checked_mul(KEYFORM_COUNT)?)
            .ok()?;
        for _ in 0..chain_length {
            for keyform_index in 0..KEYFORM_COUNT {
                position_begins.push(i32::try_from(keyform_position_xys.len()).ok()?);
                let deformation = keyform_index as f32 * 0.02;
                for row in 0..=rows {
                    for col in 0..=cols {
                        let x = col as f32 / cols as f32;
                        let y = row as f32 / rows as f32;
                        keyform_position_xys.push(x + deformation * y * (1.0 - x));
                        keyform_position_xys.push(y + deformation * x * (1.0 - y));
                    }
                }
            }
        }

        let parent_deformer_indices = (0..chain_length)
            .map(|index| {
                if index == 0 {
                    Some(-1)
                } else {
                    i32::try_from(index - 1).ok()
                }
            })
            .collect::<Option<Vec<_>>>()?;
        let specific_indices = (0..chain_length)
            .map(|index| i32::try_from(index).ok())
            .collect::<Option<Vec<_>>>()?;

        Some(Self {
            composition_order: composition_order(&parent_deformer_indices),
            parent_deformer_indices,
            deformer_kinds: vec![Moc3DeformerKind::Warp; chain_length],
            specific_indices,
            warp_keyform_binding_band_indices: vec![0; chain_length],
            warp_keyform_begin_indices: (0..chain_length)
                .map(|index| i32::try_from(index.checked_mul(KEYFORM_COUNT)?).ok())
                .collect::<Option<Vec<_>>>()?,
            warp_keyform_counts: vec![i32::try_from(KEYFORM_COUNT).ok()?; chain_length],
            warp_vertex_counts: vec![vertex_count_i32; chain_length],
            warp_rows: vec![rows_i32; chain_length],
            warp_cols: vec![cols_i32; chain_length],
            warp_keyform_opacities: vec![1.0; chain_length.checked_mul(KEYFORM_COUNT)?],
            rotation_keyform_binding_band_indices: Vec::new(),
            rotation_keyform_begin_indices: Vec::new(),
            rotation_keyform_counts: Vec::new(),
            rotation_base_angles: Vec::new(),
            warp_keyform_position_begin_indices: position_begins,
            rotation_keyform_angles: Vec::new(),
            rotation_keyform_origin_xs: Vec::new(),
            rotation_keyform_origin_ys: Vec::new(),
            rotation_keyform_scales: Vec::new(),
            rotation_keyform_reflect_xs: Vec::new(),
            rotation_keyform_reflect_ys: Vec::new(),
            rotation_keyform_opacities: Vec::new(),
            keyform_position_xys,
            warp_keyform_color_begin_indices: vec![-1; chain_length],
            rotation_keyform_color_begin_indices: Vec::new(),
            keyform_multiply_colors: std::array::from_fn(|_| Vec::new()),
            keyform_screen_colors: std::array::from_fn(|_| Vec::new()),
        })
    }
}

#[cfg(feature = "benchmark-support")]
#[doc(hidden)]
pub struct DeformerCompositionBenchmark {
    deformers: Moc3Deformers,
    bindings: Moc3KeyformBindings,
    parameter_values: [f32; 1],
    keyform_scratch: Moc3KeyformScratch,
    composed: ComposedDeformers,
    points_per_update: usize,
}

#[cfg(feature = "benchmark-support")]
impl DeformerCompositionBenchmark {
    pub fn new(chain_length: usize, cols: usize, rows: usize) -> Option<Self> {
        let points_per_deformer = cols.checked_add(1)?.checked_mul(rows.checked_add(1)?)?;
        Some(Self {
            deformers: Moc3Deformers::benchmark_warp_chain(chain_length, cols, rows)?,
            bindings: Moc3KeyformBindings::two_keyforms_for_benchmark(),
            parameter_values: [0.35],
            keyform_scratch: Moc3KeyformScratch::default(),
            composed: ComposedDeformers::default(),
            points_per_update: points_per_deformer.checked_mul(chain_length)?,
        })
    }

    pub fn points_per_update(&self) -> usize {
        self.points_per_update
    }

    pub fn compose(&mut self) -> f32 {
        self.deformers
            .compose_into(
                &self.bindings,
                &self.parameter_values,
                &mut self.keyform_scratch,
                &mut self.composed,
            )
            .expect("基准 fixture 应能复用 composition scratch");
        self.composed.benchmark_probe()
    }
}

fn composition_order(parent_deformer_indices: &[i32]) -> Vec<usize> {
    let mut order: Vec<usize> = (0..parent_deformer_indices.len()).collect();
    order.sort_by_key(|&index| deformer_depth(parent_deformer_indices, index));
    order
}

fn deformer_depth(parent_deformer_indices: &[i32], index: usize) -> usize {
    let mut depth = 0usize;
    let mut current = index;
    loop {
        let parent = parent_deformer_indices.get(current).copied().unwrap_or(-1);
        if parent < 0 {
            break;
        }
        current = match usize::try_from(parent) {
            Ok(value) => value,
            Err(_) => break,
        };
        depth += 1;
        if depth > parent_deformer_indices.len() {
            break;
        }
    }
    depth
}

fn interpolate_bool(value: f32) -> bool {
    (value + 0.001).trunc() != 0.0
}

fn read_color_channels(
    bytes: &[u8],
    offsets: &Moc3SectionOffsets,
    slots: [usize; 3],
    count: usize,
    endianness: super::Endianness,
    default: f32,
) -> Result<[Vec<f32>; 3]> {
    Ok([
        read_f32_section_or_default(bytes, offsets, slots[0], count, endianness, default)?,
        read_f32_section_or_default(bytes, offsets, slots[1], count, endianness, default)?,
        read_f32_section_or_default(bytes, offsets, slots[2], count, endianness, default)?,
    ])
}

fn interpolate_colors(
    begin: i32,
    slots: &[Moc3KeyformSlot],
    multiply_colors: &[Vec<f32>; 3],
    screen_colors: &[Vec<f32>; 3],
) -> Option<([f32; 3], [f32; 3])> {
    if begin < 0 {
        return Some(([1.0, 1.0, 1.0], [0.0, 0.0, 0.0]));
    }
    let begin = usize::try_from(begin).ok()?;
    let mut multiply = [0.0; 3];
    let mut screen = [0.0; 3];
    for slot in slots {
        let color_index = begin.checked_add(slot.local_index)?;
        for channel in 0..3 {
            multiply[channel] += multiply_colors[channel].get(color_index).copied()? * slot.weight;
            screen[channel] += screen_colors[channel].get(color_index).copied()? * slot.weight;
        }
    }
    Some((multiply, screen))
}

fn compose_colors(
    local: ([f32; 3], [f32; 3]),
    parent: ([f32; 3], [f32; 3]),
) -> ([f32; 3], [f32; 3]) {
    let mut multiply = [0.0; 3];
    let mut screen = [0.0; 3];
    for channel in 0..3 {
        multiply[channel] = local.0[channel] * parent.0[channel];
        screen[channel] =
            local.1[channel] + parent.1[channel] - local.1[channel] * parent.1[channel];
    }
    (multiply, screen)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn composition_order_places_parents_before_children() {
        let parents = [2, -1, 1, 2];

        assert_eq!(composition_order(&parents), vec![1, 2, 0, 3]);
    }

    #[test]
    fn composition_order_preserves_sibling_order() {
        let parents = [-1, 0, 0, 1, 1];

        assert_eq!(composition_order(&parents), vec![0, 1, 2, 3, 4]);
    }

    #[test]
    fn reused_composition_matches_owned_and_recovers_after_failure() {
        let mut deformers =
            Moc3Deformers::benchmark_warp_chain(4, 4, 4).expect("固定测试参数应生成有效 warp 链");
        let bindings = Moc3KeyformBindings::two_keyforms_for_benchmark();
        let parameter_values = [0.35];
        let expected = deformers
            .compose(&bindings, &parameter_values)
            .expect("全新 composition 应成功");
        let mut keyform_scratch = Moc3KeyformScratch::default();
        let mut composed = ComposedDeformers::default();

        deformers
            .compose_into(
                &bindings,
                &parameter_values,
                &mut keyform_scratch,
                &mut composed,
            )
            .expect("复用 composition 应成功");
        assert_eq!(composed, expected);

        deformers.parent_deformer_indices[1] = i32::MAX;
        assert!(
            deformers
                .compose_into(
                    &bindings,
                    &parameter_values,
                    &mut keyform_scratch,
                    &mut composed,
                )
                .is_none(),
            "非法父索引应使 composition 失败"
        );

        deformers.parent_deformer_indices[1] = 0;
        deformers
            .compose_into(
                &bindings,
                &parameter_values,
                &mut keyform_scratch,
                &mut composed,
            )
            .expect("失败后的 scratch 应可再次复用");
        assert_eq!(composed, expected);
    }
}
