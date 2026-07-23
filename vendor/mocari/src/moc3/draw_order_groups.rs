use crate::Result;

use super::{
    Moc3CountInfo, Moc3Header, Moc3SectionOffsets,
    parse::{read_i32_section, to_usize},
};

const GROUP_OBJECT_BEGIN_INDICES_SLOT: usize = 81;
const GROUP_OBJECT_COUNTS_SLOT: usize = 82;
const GROUP_SUBTREE_COUNTS_SLOT: usize = 83;
const GROUP_MAX_DRAW_ORDERS_SLOT: usize = 84;
const GROUP_BASE_DRAW_ORDERS_SLOT: usize = 85;
const OBJECT_TYPES_SLOT: usize = 86;
const OBJECT_INDICES_SLOT: usize = 87;
const OBJECT_SELF_GROUP_IDX_SLOT: usize = 88;

const OBJECT_TYPE_DRAWABLE: i32 = 0;
const OBJECT_TYPE_PART: i32 = 1;
const MAX_DRAW_ORDER_GROUP_DEPTH: usize = 256;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct GroupObject {
    pub object_type: i32,
    pub object_idx: usize,
    pub self_group_idx: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Group {
    pub object_begin: usize,
    pub object_count: usize,
    pub subtree_drawable_count: usize,
    pub base_draw_order: i32,
    pub max_draw_order: i32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Moc3DrawOrderGroups {
    pub(super) groups: Vec<Group>,
    pub(super) objects: Vec<GroupObject>,
    pub(super) drawable_count: usize,
}

impl Moc3DrawOrderGroups {
    pub fn parse(bytes: &[u8]) -> Result<Option<Self>> {
        let header = Moc3Header::parse(bytes)?;
        let offsets = Moc3SectionOffsets::parse(bytes)?;
        let counts = Moc3CountInfo::parse(bytes)?;
        let endianness = header.endianness();

        let group_count = to_usize(counts.draw_order_groups(), "draw order group count")?;
        let object_count = to_usize(
            counts.draw_order_group_objects(),
            "draw order group object count",
        )?;
        let drawable_count = to_usize(counts.art_meshes(), "art mesh count")?;

        if group_count == 0 || object_count == 0 {
            return Ok(None);
        }

        let begin = read_i32_section(
            bytes,
            &offsets,
            GROUP_OBJECT_BEGIN_INDICES_SLOT,
            group_count,
            endianness,
        )?;
        let count = read_i32_section(
            bytes,
            &offsets,
            GROUP_OBJECT_COUNTS_SLOT,
            group_count,
            endianness,
        )?;
        let subtree = read_i32_section(
            bytes,
            &offsets,
            GROUP_SUBTREE_COUNTS_SLOT,
            group_count,
            endianness,
        )?;
        let max = read_i32_section(
            bytes,
            &offsets,
            GROUP_MAX_DRAW_ORDERS_SLOT,
            group_count,
            endianness,
        )?;
        let base = read_i32_section(
            bytes,
            &offsets,
            GROUP_BASE_DRAW_ORDERS_SLOT,
            group_count,
            endianness,
        )?;
        let types = read_i32_section(bytes, &offsets, OBJECT_TYPES_SLOT, object_count, endianness)?;
        let indices = read_i32_section(
            bytes,
            &offsets,
            OBJECT_INDICES_SLOT,
            object_count,
            endianness,
        )?;
        let self_group_idx = read_i32_section(
            bytes,
            &offsets,
            OBJECT_SELF_GROUP_IDX_SLOT,
            object_count,
            endianness,
        )?;

        let mut groups = Vec::with_capacity(group_count);
        for group_index in 0..group_count {
            groups.push(Group {
                object_begin: to_usize(begin[group_index].max(0) as u32, "group object begin")?,
                object_count: to_usize(count[group_index].max(0) as u32, "group object count")?,
                subtree_drawable_count: to_usize(
                    subtree[group_index].max(0) as u32,
                    "group subtree count",
                )?,
                base_draw_order: base[group_index],
                max_draw_order: max[group_index],
            });
        }

        let mut objects = Vec::with_capacity(object_count);
        for object_index in 0..object_count {
            objects.push(GroupObject {
                object_type: types[object_index],
                object_idx: indices[object_index].max(0) as usize,
                self_group_idx: self_group_idx[object_index].max(0) as usize,
            });
        }

        Ok(Some(Self {
            groups,
            objects,
            drawable_count,
        }))
    }

    pub fn drawable_count(&self) -> usize {
        self.drawable_count
    }

    pub fn group_count(&self) -> usize {
        self.groups.len()
    }

    pub fn render_orders(
        &self,
        drawable_draw_orders: &[i32],
        part_draw_orders: &[i32],
        part_enable: &[bool],
        part_offscreen_indices: &[i32],
        offscreen_count: usize,
    ) -> Option<Vec<i32>> {
        if drawable_draw_orders.len() != self.drawable_count {
            return None;
        }

        let render_order_count = self.drawable_count.checked_add(offscreen_count)?;
        let mut render_orders = vec![0; render_order_count];
        let mut visiting = vec![false; self.groups.len()];
        self.expand_group(
            0,
            0,
            drawable_draw_orders,
            part_draw_orders,
            part_enable,
            part_offscreen_indices,
            &mut render_orders,
            &mut visiting,
            0,
        )?;
        Some(render_orders)
    }

    fn effective_draw_order(
        &self,
        group_index: usize,
        object: &GroupObject,
        drawable_draw_orders: &[i32],
        part_draw_orders: &[i32],
        part_enable: &[bool],
    ) -> Option<i32> {
        let fallback = self.groups.get(group_index)?.base_draw_order;
        match object.object_type {
            OBJECT_TYPE_PART => {
                if part_enable.get(object.object_idx).copied().unwrap_or(false) {
                    part_draw_orders.get(object.object_idx).copied()
                } else {
                    Some(fallback)
                }
            }
            OBJECT_TYPE_DRAWABLE => drawable_draw_orders.get(object.object_idx).copied(),
            _ => Some(fallback),
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn expand_group(
        &self,
        group_index: usize,
        start_rank: i32,
        drawable_draw_orders: &[i32],
        part_draw_orders: &[i32],
        part_enable: &[bool],
        part_offscreen_indices: &[i32],
        render_orders: &mut [i32],
        visiting: &mut [bool],
        depth: usize,
    ) -> Option<()> {
        if depth >= MAX_DRAW_ORDER_GROUP_DEPTH || *visiting.get(group_index)? {
            return None;
        }
        *visiting.get_mut(group_index)? = true;
        let group = self.groups.get(group_index)?;
        let begin = group.object_begin;
        let end = begin.checked_add(group.object_count)?;
        let members = self.objects.get(begin..end)?;

        if members.is_empty() {
            *visiting.get_mut(group_index)? = false;
            return Some(());
        }

        let mut ordered = Vec::new();
        ordered.try_reserve_exact(members.len()).ok()?;

        for (offset, object) in members.iter().enumerate() {
            let effective = self.effective_draw_order(
                group_index,
                object,
                drawable_draw_orders,
                part_draw_orders,
                part_enable,
            )?;
            let order = if group.max_draw_order > group.base_draw_order {
                effective.clamp(group.base_draw_order, group.max_draw_order)
            } else {
                group.base_draw_order
            };
            ordered.push((order, offset));
        }
        ordered.sort_by_key(|(order, _)| *order);

        let mut rank = start_rank;
        for (_, offset) in ordered {
            let object = &members[offset];
            if object.object_type == OBJECT_TYPE_PART {
                if let Some(offscreen) = part_offscreen_indices
                    .get(object.object_idx)
                    .copied()
                    .filter(|&value| value >= 0)
                {
                    let slot = self.drawable_count.checked_add(offscreen as usize)?;
                    *render_orders.get_mut(slot)? = rank;
                    rank += 1;
                }
                let child = object.self_group_idx;
                if child < self.groups.len() && child != group_index {
                    self.expand_group(
                        child,
                        rank,
                        drawable_draw_orders,
                        part_draw_orders,
                        part_enable,
                        part_offscreen_indices,
                        render_orders,
                        visiting,
                        depth + 1,
                    )?;
                    rank += i32::try_from(self.groups.get(child)?.subtree_drawable_count).ok()?;
                }
            } else {
                *render_orders.get_mut(object.object_idx)? = rank;
                rank += 1;
            }
        }

        *visiting.get_mut(group_index)? = false;
        Some(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_group_with_inverted_sentinel_draw_order_range_does_not_panic() {
        let groups = Moc3DrawOrderGroups {
            groups: vec![
                Group {
                    object_begin: 0,
                    object_count: 1,
                    subtree_drawable_count: 0,
                    base_draw_order: 0,
                    max_draw_order: 0,
                },
                Group {
                    object_begin: 1,
                    object_count: 0,
                    subtree_drawable_count: 0,
                    base_draw_order: i32::MAX,
                    max_draw_order: -i32::MAX,
                },
            ],
            objects: vec![GroupObject {
                object_type: OBJECT_TYPE_PART,
                object_idx: 0,
                self_group_idx: 1,
            }],
            drawable_count: 0,
        };

        assert_eq!(
            groups.render_orders(&[], &[0], &[false], &[-1], 0),
            Some(Vec::new())
        );
    }

    #[test]
    fn extreme_draw_order_span_sorts_only_existing_members() {
        let groups = Moc3DrawOrderGroups {
            groups: vec![Group {
                object_begin: 0,
                object_count: 1,
                subtree_drawable_count: 1,
                base_draw_order: i32::MIN,
                max_draw_order: i32::MAX,
            }],
            objects: vec![GroupObject {
                object_type: OBJECT_TYPE_DRAWABLE,
                object_idx: 0,
                self_group_idx: 0,
            }],
            drawable_count: 1,
        };

        assert_eq!(
            groups.render_orders(&[i32::MAX], &[], &[], &[], 0),
            Some(vec![0])
        );
    }

    #[test]
    fn cyclic_group_hierarchy_is_rejected_without_recursion_overflow() {
        let groups = Moc3DrawOrderGroups {
            groups: vec![
                Group {
                    object_begin: 0,
                    object_count: 1,
                    subtree_drawable_count: 1,
                    base_draw_order: 0,
                    max_draw_order: 0,
                },
                Group {
                    object_begin: 1,
                    object_count: 1,
                    subtree_drawable_count: 1,
                    base_draw_order: 0,
                    max_draw_order: 0,
                },
            ],
            objects: vec![
                GroupObject {
                    object_type: OBJECT_TYPE_PART,
                    object_idx: 0,
                    self_group_idx: 1,
                },
                GroupObject {
                    object_type: OBJECT_TYPE_PART,
                    object_idx: 0,
                    self_group_idx: 0,
                },
            ],
            drawable_count: 0,
        };

        assert_eq!(groups.render_orders(&[], &[0], &[false], &[-1], 0), None);
    }
}
