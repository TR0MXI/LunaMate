use crate::{Error, Result};

use super::{Moc3Header, Moc3SectionOffsets, parse::read_u32};

const U32_SIZE: usize = 4;
const MAX_STRUCTURAL_COUNT: u32 = 65_536;
const MAX_SECTION_VALUE_COUNT: u32 = 4_000_000;
const MAX_TOTAL_DECLARED_VALUES: u64 = 16_000_000;

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub struct Moc3CountInfo {
    parts: u32,
    deformers: u32,
    warp_deformers: u32,
    rotation_deformers: u32,
    art_meshes: u32,
    parameters: u32,
    part_keyforms: u32,
    warp_deformer_keyforms: u32,
    rotation_deformer_keyforms: u32,
    art_mesh_keyforms: u32,
    keyform_positions: u32,
    parameter_binding_indices: u32,
    keyform_bindings: u32,
    parameter_bindings: u32,
    keys: u32,
    uvs: u32,
    position_indices: u32,
    drawable_masks: u32,
    draw_order_groups: u32,
    draw_order_group_objects: u32,
    glue: u32,
    glue_info: u32,
    glue_keyforms: u32,
    keyform_multiply_colors: u32,
    keyform_screen_colors: u32,
}

impl Moc3CountInfo {
    pub fn parse(bytes: &[u8]) -> Result<Self> {
        let header = Moc3Header::parse(bytes)?;
        let offsets = Moc3SectionOffsets::parse(bytes)?;
        let offset = usize::try_from(offsets.count_info_offset())
            .map_err(|_| invalid_counts("count info offset does not fit in platform usize"))?;
        let word_count = header.version().count_info_word_count();
        let required_len = word_count * U32_SIZE;

        if bytes.len().saturating_sub(offset) < required_len {
            return Err(invalid_counts("count info table is incomplete"));
        }

        let read = |index| read_u32(bytes, offset + index * U32_SIZE, header.endianness());

        let counts = Self {
            parts: read(0),
            deformers: read(1),
            warp_deformers: read(2),
            rotation_deformers: read(3),
            art_meshes: read(4),
            parameters: read(5),
            part_keyforms: read(6),
            warp_deformer_keyforms: read(7),
            rotation_deformer_keyforms: read(8),
            art_mesh_keyforms: read(9),
            keyform_positions: read(10),
            parameter_binding_indices: read(11),
            keyform_bindings: read(12),
            parameter_bindings: read(13),
            keys: read(14),
            uvs: read(15),
            position_indices: read(16),
            drawable_masks: read(17),
            draw_order_groups: read(18),
            draw_order_group_objects: read(19),
            glue: read(20),
            glue_info: read(21),
            glue_keyforms: read(22),
            keyform_multiply_colors: if word_count > 23 { read(23) } else { 0 },
            keyform_screen_colors: if word_count > 24 { read(24) } else { 0 },
        };
        counts.validate_allocation_budget()?;
        Ok(counts)
    }

    fn validate_allocation_budget(&self) -> Result<()> {
        let structural = [
            ("part count", self.parts),
            ("deformer count", self.deformers),
            ("warp deformer count", self.warp_deformers),
            ("rotation deformer count", self.rotation_deformers),
            ("art mesh count", self.art_meshes),
            ("parameter count", self.parameters),
            ("draw order group count", self.draw_order_groups),
            (
                "draw order group object count",
                self.draw_order_group_objects,
            ),
            ("glue count", self.glue),
        ];
        for (name, count) in structural {
            if count > MAX_STRUCTURAL_COUNT {
                return Err(invalid_counts(format!(
                    "{name} {count} exceeds structural limit {MAX_STRUCTURAL_COUNT}"
                )));
            }
        }

        let declared = [
            ("part count", self.parts),
            ("deformer count", self.deformers),
            ("warp deformer count", self.warp_deformers),
            ("rotation deformer count", self.rotation_deformers),
            ("art mesh count", self.art_meshes),
            ("parameter count", self.parameters),
            ("part keyform count", self.part_keyforms),
            ("warp deformer keyform count", self.warp_deformer_keyforms),
            (
                "rotation deformer keyform count",
                self.rotation_deformer_keyforms,
            ),
            ("art mesh keyform count", self.art_mesh_keyforms),
            ("keyform position count", self.keyform_positions),
            (
                "parameter binding index count",
                self.parameter_binding_indices,
            ),
            ("keyform binding count", self.keyform_bindings),
            ("parameter binding count", self.parameter_bindings),
            ("key count", self.keys),
            ("uv count", self.uvs),
            ("position index count", self.position_indices),
            ("drawable mask count", self.drawable_masks),
            ("draw order group count", self.draw_order_groups),
            (
                "draw order group object count",
                self.draw_order_group_objects,
            ),
            ("glue count", self.glue),
            ("glue info count", self.glue_info),
            ("glue keyform count", self.glue_keyforms),
            ("keyform multiply color count", self.keyform_multiply_colors),
            ("keyform screen color count", self.keyform_screen_colors),
        ];
        let mut total = 0_u64;
        for (name, count) in declared {
            if count > MAX_SECTION_VALUE_COUNT {
                return Err(invalid_counts(format!(
                    "{name} {count} exceeds section limit {MAX_SECTION_VALUE_COUNT}"
                )));
            }
            total = total
                .checked_add(u64::from(count))
                .ok_or_else(|| invalid_counts("declared value total overflows"))?;
        }
        if total > MAX_TOTAL_DECLARED_VALUES {
            return Err(invalid_counts(format!(
                "declared value total {total} exceeds limit {MAX_TOTAL_DECLARED_VALUES}"
            )));
        }
        Ok(())
    }

    pub fn parts(&self) -> u32 {
        self.parts
    }

    pub fn deformers(&self) -> u32 {
        self.deformers
    }

    pub fn warp_deformers(&self) -> u32 {
        self.warp_deformers
    }

    pub fn rotation_deformers(&self) -> u32 {
        self.rotation_deformers
    }

    pub fn art_meshes(&self) -> u32 {
        self.art_meshes
    }

    pub fn parameters(&self) -> u32 {
        self.parameters
    }

    pub fn part_keyforms(&self) -> u32 {
        self.part_keyforms
    }

    pub fn warp_deformer_keyforms(&self) -> u32 {
        self.warp_deformer_keyforms
    }

    pub fn rotation_deformer_keyforms(&self) -> u32 {
        self.rotation_deformer_keyforms
    }

    pub fn art_mesh_keyforms(&self) -> u32 {
        self.art_mesh_keyforms
    }

    pub fn keyform_positions(&self) -> u32 {
        self.keyform_positions
    }

    pub fn parameter_binding_indices(&self) -> u32 {
        self.parameter_binding_indices
    }

    pub fn keyform_bindings(&self) -> u32 {
        self.keyform_bindings
    }

    pub fn parameter_bindings(&self) -> u32 {
        self.parameter_bindings
    }

    pub fn keys(&self) -> u32 {
        self.keys
    }

    pub fn uvs(&self) -> u32 {
        self.uvs
    }

    pub fn position_indices(&self) -> u32 {
        self.position_indices
    }

    pub fn drawable_masks(&self) -> u32 {
        self.drawable_masks
    }

    pub fn draw_order_groups(&self) -> u32 {
        self.draw_order_groups
    }

    pub fn draw_order_group_objects(&self) -> u32 {
        self.draw_order_group_objects
    }

    pub fn glue(&self) -> u32 {
        self.glue
    }

    pub fn glue_info(&self) -> u32 {
        self.glue_info
    }

    pub fn glue_keyforms(&self) -> u32 {
        self.glue_keyforms
    }

    pub fn keyform_multiply_colors(&self) -> u32 {
        self.keyform_multiply_colors
    }

    pub fn keyform_screen_colors(&self) -> u32 {
        self.keyform_screen_colors
    }
}

fn invalid_counts(message: impl Into<String>) -> Error {
    Error::InvalidMoc3 {
        message: message.into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const OFFSET_TABLE_START: usize = 0x40;
    const OFFSET_COUNT: usize = 160;
    const COUNT_INFO_OFFSET: usize = OFFSET_TABLE_START + OFFSET_COUNT * U32_SIZE;

    fn count_info_with(index: usize, value: u32) -> Vec<u8> {
        let mut bytes = vec![0_u8; COUNT_INFO_OFFSET + 35 * U32_SIZE];
        bytes[0..4].copy_from_slice(b"MOC3");
        bytes[4] = 6;
        bytes[5] = 0;
        bytes[OFFSET_TABLE_START..OFFSET_TABLE_START + U32_SIZE]
            .copy_from_slice(&(COUNT_INFO_OFFSET as u32).to_le_bytes());
        bytes[OFFSET_TABLE_START + U32_SIZE..OFFSET_TABLE_START + 2 * U32_SIZE]
            .copy_from_slice(&(COUNT_INFO_OFFSET as u32).to_le_bytes());
        let start = COUNT_INFO_OFFSET + index * U32_SIZE;
        bytes[start..start + U32_SIZE].copy_from_slice(&value.to_le_bytes());
        bytes
    }

    #[test]
    fn rejects_structural_count_before_section_allocation() {
        let bytes = count_info_with(4, MAX_STRUCTURAL_COUNT + 1);

        let error = Moc3CountInfo::parse(&bytes).expect_err("oversized art mesh count must fail");

        assert!(error.to_string().contains("structural limit"));
    }

    #[test]
    fn rejects_oversized_scalar_section_count() {
        let bytes = count_info_with(10, MAX_SECTION_VALUE_COUNT + 1);

        let error = Moc3CountInfo::parse(&bytes).expect_err("oversized keyform section must fail");

        assert!(error.to_string().contains("section limit"));
    }
}
