use crate::Result;

use super::{
    Moc3CountInfo, Moc3Header, Moc3KeyformBindings, Moc3SectionOffsets,
    parse::{invalid_moc3, read_f32_section, read_i32_section, to_usize},
};

const PART_KEYFORM_BINDING_BAND_INDICES_SLOT: usize = 4;
const PART_KEYFORM_BEGIN_INDICES_SLOT: usize = 5;
const PART_KEYFORM_COUNTS_SLOT: usize = 6;
const PART_PARENT_PART_INDICES_SLOT: usize = 9;
const PART_KEYFORM_DRAW_ORDERS_SLOT: usize = 58;
const PART_KEYFORM_OPACITIES_SLOT: usize = 59;

#[derive(Debug, Clone, PartialEq)]
pub struct Moc3Parts {
    parent_part_indices: Vec<i32>,
    keyform_binding_band_indices: Vec<i32>,
    keyform_begin_indices: Vec<i32>,
    keyform_counts: Vec<i32>,
    keyform_draw_orders: Vec<f32>,
    keyform_opacities: Vec<f32>,
}

impl Moc3Parts {
    #[cfg(test)]
    pub(crate) fn from_parts(
        parent_part_indices: Vec<i32>,
        keyform_binding_band_indices: Vec<i32>,
        keyform_begin_indices: Vec<i32>,
        keyform_counts: Vec<i32>,
        keyform_draw_orders: Vec<f32>,
        keyform_opacities: Vec<f32>,
    ) -> Self {
        Self {
            parent_part_indices,
            keyform_binding_band_indices,
            keyform_begin_indices,
            keyform_counts,
            keyform_draw_orders,
            keyform_opacities,
        }
    }

    pub fn parse(bytes: &[u8]) -> Result<Self> {
        let header = Moc3Header::parse(bytes)?;
        let offsets = Moc3SectionOffsets::parse(bytes)?;
        let counts = Moc3CountInfo::parse(bytes)?;
        let endianness = header.endianness();
        let part_count = to_usize(counts.parts(), "part count")?;
        let part_keyform_count = to_usize(counts.part_keyforms(), "part keyform count")?;

        let parent_part_indices = read_i32_section(
            bytes,
            &offsets,
            PART_PARENT_PART_INDICES_SLOT,
            part_count,
            endianness,
        )?;
        validate_parent_part_indices(&parent_part_indices)?;

        Ok(Self {
            parent_part_indices,
            keyform_binding_band_indices: read_i32_section(
                bytes,
                &offsets,
                PART_KEYFORM_BINDING_BAND_INDICES_SLOT,
                part_count,
                endianness,
            )?,
            keyform_begin_indices: read_i32_section(
                bytes,
                &offsets,
                PART_KEYFORM_BEGIN_INDICES_SLOT,
                part_count,
                endianness,
            )?,
            keyform_counts: read_i32_section(
                bytes,
                &offsets,
                PART_KEYFORM_COUNTS_SLOT,
                part_count,
                endianness,
            )?,
            keyform_draw_orders: read_f32_section(
                bytes,
                &offsets,
                PART_KEYFORM_DRAW_ORDERS_SLOT,
                part_keyform_count,
                endianness,
            )?,
            keyform_opacities: read_f32_section(
                bytes,
                &offsets,
                PART_KEYFORM_OPACITIES_SLOT,
                part_keyform_count,
                endianness,
            )?,
        })
    }

    pub fn part_count(&self) -> usize {
        self.parent_part_indices.len()
    }

    pub fn parent_part_index(&self, part_index: usize) -> Option<i32> {
        self.parent_part_indices.get(part_index).copied()
    }

    pub fn interpolate_opacity(
        &self,
        part_index: usize,
        bindings: &Moc3KeyformBindings,
        parameter_values: &[f32],
    ) -> Option<f32> {
        let keyform_count = usize::try_from(*self.keyform_counts.get(part_index)?).ok()?;
        if keyform_count == 0 {
            return Some(1.0);
        }
        let begin = usize::try_from(*self.keyform_begin_indices.get(part_index)?).ok()?;
        let slots = bindings.keyform_slots(
            *self.keyform_binding_band_indices.get(part_index)?,
            keyform_count,
            parameter_values,
        )?;

        let mut opacity = 0.0f32;
        for slot in slots {
            let keyform_index = begin.checked_add(slot.local_index)?;
            opacity += *self.keyform_opacities.get(keyform_index)? * slot.weight;
        }
        Some(opacity)
    }

    pub fn interpolate_draw_order(
        &self,
        part_index: usize,
        bindings: &Moc3KeyformBindings,
        parameter_values: &[f32],
    ) -> Option<f32> {
        let keyform_count = usize::try_from(*self.keyform_counts.get(part_index)?).ok()?;
        if keyform_count == 0 {
            return None;
        }
        let begin = usize::try_from(*self.keyform_begin_indices.get(part_index)?).ok()?;
        let slots = bindings.keyform_slots(
            *self.keyform_binding_band_indices.get(part_index)?,
            keyform_count,
            parameter_values,
        )?;

        let mut draw_order = 0.0f32;
        for slot in slots {
            let keyform_index = begin.checked_add(slot.local_index)?;
            draw_order += *self.keyform_draw_orders.get(keyform_index)? * slot.weight;
        }
        Some(draw_order)
    }
}

fn validate_parent_part_indices(parents: &[i32]) -> Result<()> {
    let mut state = vec![0_u8; parents.len()];
    let mut path = Vec::new();
    path.try_reserve_exact(parents.len())
        .map_err(|error| invalid_moc3(format!("无法为 Part 父链分配校验空间：{error}")))?;

    for start in 0..parents.len() {
        if state[start] == 2 {
            continue;
        }
        path.clear();
        let mut current = Some(start);
        while let Some(index) = current {
            let Some(node_state) = state.get_mut(index) else {
                return Err(invalid_moc3("Part 父索引越出 Part 数组"));
            };
            match *node_state {
                0 => {
                    *node_state = 1;
                    path.push(index);
                    let parent = parents[index];
                    if parent < -1 {
                        return Err(invalid_moc3("Part 父索引小于 -1"));
                    }
                    current = usize::try_from(parent).ok();
                }
                1 => return Err(invalid_moc3("Part 父链包含循环")),
                2 => break,
                _ => return Err(invalid_moc3("Part 父链校验状态无效")),
            }
        }
        for index in path.drain(..) {
            state[index] = 2;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[ignore = "依赖仓库不分发的 Hiyori Live2D 模型"]
    fn parses_hiyori_parts_with_default_opacity() {
        let bytes = std::fs::read("assets/models/Hiyori/Hiyori.moc3").unwrap();
        let parts = Moc3Parts::parse(&bytes).unwrap();
        let bindings = Moc3KeyformBindings::parse(&bytes).unwrap();
        let parameters = bindings.parameter_default_values();

        assert_eq!(parts.part_count(), 24);
        for part_index in 0..parts.part_count() {
            let opacity = parts
                .interpolate_opacity(part_index, &bindings, parameters)
                .unwrap();
            assert!((opacity - 1.0).abs() < 1e-6);
        }
    }

    #[test]
    fn rejects_invalid_parent_part_graphs() {
        assert!(validate_parent_part_indices(&[1, 0]).is_err());
        assert!(validate_parent_part_indices(&[2, -1]).is_err());
    }
}
