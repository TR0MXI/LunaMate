use crate::{
    Result,
    core::{KeyformAxis, compute_keyform_axis_interval, expand_keyform_runtime_slots},
};

use super::{
    Moc3CountInfo, Moc3Header, Moc3SectionOffsets,
    parse::{invalid_moc3, read_f32_section, read_i32_section, to_usize},
};

const MAX_KEYFORM_AXES: usize = 16;

const PARAMETER_MAX_VALUES_SLOT: usize = 51;
const PARAMETER_MIN_VALUES_SLOT: usize = 52;
const PARAMETER_DEFAULT_VALUES_SLOT: usize = 53;
const PARAMETER_BINDING_BEGIN_INDICES_SLOT: usize = 56;
const KEYFORM_BINDING_INDICES_SLOT: usize = 72;
const KEYFORM_BINDING_BAND_BEGIN_INDICES_SLOT: usize = 73;
const KEYFORM_BINDING_BAND_COUNTS_SLOT: usize = 74;
const KEYFORM_BINDING_KEYS_BEGIN_INDICES_SLOT: usize = 75;
const KEYFORM_BINDING_KEYS_COUNTS_SLOT: usize = 76;
const KEY_VALUES_SLOT: usize = 77;

#[derive(Debug, Clone, PartialEq)]
pub struct Moc3KeyformBindings {
    parameter_min_values: Vec<f32>,
    parameter_max_values: Vec<f32>,
    parameter_default_values: Vec<f32>,
    binding_parameter_indices: Vec<usize>,
    keyform_binding_indices: Vec<i32>,
    band_begin_indices: Vec<i32>,
    band_counts: Vec<i32>,
    keys_begin_indices: Vec<i32>,
    keys_counts: Vec<i32>,
    key_values: Vec<f32>,
}

#[derive(Debug, Copy, Clone, PartialEq)]
pub(super) struct Moc3KeyformSlot {
    pub(super) local_index: usize,
    pub(super) weight: f32,
}

impl Moc3KeyformBindings {
    pub fn parse(bytes: &[u8]) -> Result<Self> {
        let header = Moc3Header::parse(bytes)?;
        let offsets = Moc3SectionOffsets::parse(bytes)?;
        let counts = Moc3CountInfo::parse(bytes)?;
        let endianness = header.endianness();
        let parameter_count = to_usize(counts.parameters(), "parameter count")?;
        let parameter_binding_count =
            to_usize(counts.parameter_bindings(), "parameter binding count")?;

        let parameter_binding_begin_indices = read_i32_section(
            bytes,
            &offsets,
            PARAMETER_BINDING_BEGIN_INDICES_SLOT,
            parameter_count,
            endianness,
        )?;
        let binding_parameter_indices = expand_binding_parameter_indices(
            &parameter_binding_begin_indices,
            parameter_binding_count,
        )
        .ok_or_else(|| invalid_moc3("invalid parameter binding begin indices"))?;

        Ok(Self {
            binding_parameter_indices,
            parameter_min_values: read_f32_section(
                bytes,
                &offsets,
                PARAMETER_MIN_VALUES_SLOT,
                parameter_count,
                endianness,
            )?,
            parameter_max_values: read_f32_section(
                bytes,
                &offsets,
                PARAMETER_MAX_VALUES_SLOT,
                parameter_count,
                endianness,
            )?,
            parameter_default_values: read_f32_section(
                bytes,
                &offsets,
                PARAMETER_DEFAULT_VALUES_SLOT,
                parameter_count,
                endianness,
            )?,
            keyform_binding_indices: read_i32_section(
                bytes,
                &offsets,
                KEYFORM_BINDING_INDICES_SLOT,
                to_usize(
                    counts.parameter_binding_indices(),
                    "keyform binding index count",
                )?,
                endianness,
            )?,
            band_begin_indices: read_i32_section(
                bytes,
                &offsets,
                KEYFORM_BINDING_BAND_BEGIN_INDICES_SLOT,
                to_usize(counts.keyform_bindings(), "keyform binding band count")?,
                endianness,
            )?,
            band_counts: read_i32_section(
                bytes,
                &offsets,
                KEYFORM_BINDING_BAND_COUNTS_SLOT,
                to_usize(counts.keyform_bindings(), "keyform binding band count")?,
                endianness,
            )?,
            keys_begin_indices: read_i32_section(
                bytes,
                &offsets,
                KEYFORM_BINDING_KEYS_BEGIN_INDICES_SLOT,
                to_usize(counts.parameter_bindings(), "keyform binding count")?,
                endianness,
            )?,
            keys_counts: read_i32_section(
                bytes,
                &offsets,
                KEYFORM_BINDING_KEYS_COUNTS_SLOT,
                to_usize(counts.parameter_bindings(), "keyform binding count")?,
                endianness,
            )?,
            key_values: read_f32_section(
                bytes,
                &offsets,
                KEY_VALUES_SLOT,
                to_usize(counts.keys(), "key count")?,
                endianness,
            )?,
        })
    }

    pub fn parameter_default_values(&self) -> &[f32] {
        &self.parameter_default_values
    }

    pub fn parameter_min_values(&self) -> &[f32] {
        &self.parameter_min_values
    }

    pub fn parameter_max_values(&self) -> &[f32] {
        &self.parameter_max_values
    }

    pub fn default_keyform_index(&self, band_index: i32, keyform_count: usize) -> Option<usize> {
        self.keyform_slots(band_index, keyform_count, &self.parameter_default_values)?
            .into_iter()
            .max_by(|left, right| left.weight.total_cmp(&right.weight))
            .map(|slot| slot.local_index)
    }

    pub(super) fn keyform_slots(
        &self,
        band_index: i32,
        keyform_count: usize,
        parameter_values: &[f32],
    ) -> Option<Vec<Moc3KeyformSlot>> {
        if keyform_count == 0 {
            return None;
        }

        if keyform_count == 1 {
            return Some(vec![Moc3KeyformSlot {
                local_index: 0,
                weight: 1.0,
            }]);
        }

        if band_index < 0 {
            return Some(vec![Moc3KeyformSlot {
                local_index: 0,
                weight: 1.0,
            }]);
        }

        let bindings = self.band_keyform_bindings(band_index)?;
        if bindings.is_empty() {
            return Some(vec![Moc3KeyformSlot {
                local_index: 0,
                weight: 1.0,
            }]);
        }
        if bindings.len() > MAX_KEYFORM_AXES {
            return None;
        }

        let mut axes = Vec::new();
        axes.try_reserve_exact(bindings.len()).ok()?;
        let mut stride = 1usize;
        for &binding_index in bindings {
            let binding_index = usize::try_from(binding_index).ok()?;
            let keys = self.binding_keys(binding_index)?;
            let parameter_index = *self.binding_parameter_indices.get(binding_index)?;
            let parameter_value = parameter_values
                .get(parameter_index)
                .copied()
                .unwrap_or(0.0);
            let interval = compute_keyform_axis_interval(keys, parameter_value)?;
            let active_index = interval.left_index() + usize::from(interval.t() != 0.0);
            if active_index >= keys.len() {
                return None;
            }
            axes.push(KeyformAxis::new(
                interval.left_index(),
                interval.t(),
                stride,
            ));
            stride = stride.checked_mul(keys.len())?;
        }

        let expanded = expand_keyform_runtime_slots(&axes);
        if expanded.is_empty() {
            return None;
        }
        let slots = expanded
            .into_iter()
            .map(|slot| {
                (slot.flat_index() < keyform_count).then_some(Moc3KeyformSlot {
                    local_index: slot.flat_index(),
                    weight: slot.weight(),
                })
            })
            .collect::<Option<Vec<_>>>()?;
        Some(slots)
    }

    fn band_keyform_bindings(&self, band_index: i32) -> Option<&[i32]> {
        let band_index = usize::try_from(band_index).ok()?;
        let begin = usize::try_from(*self.band_begin_indices.get(band_index)?).ok()?;
        let len = usize::try_from(*self.band_counts.get(band_index)?).ok()?;
        self.keyform_binding_indices
            .get(begin..begin.checked_add(len)?)
    }

    fn binding_keys(&self, binding_index: usize) -> Option<&[f32]> {
        let begin = usize::try_from(*self.keys_begin_indices.get(binding_index)?).ok()?;
        let len = usize::try_from(*self.keys_counts.get(binding_index)?).ok()?;
        self.key_values.get(begin..begin.checked_add(len)?)
    }
}

/// Maps each parameter binding to the parameter that drives it.
///
/// `begin_indices` holds, per parameter, the index of its first parameter
/// binding. Parameters without a binding repeat the previous begin index (a
/// sentinel), so a parameter owns the bindings from its begin index up to the
/// next strictly greater begin index, and the first parameter to claim a
/// binding keeps it.
fn expand_binding_parameter_indices(
    begin_indices: &[i32],
    binding_count: usize,
) -> Option<Vec<usize>> {
    let mut next_greater = vec![binding_count; begin_indices.len()];
    let mut stack = Vec::<usize>::new();
    stack.try_reserve_exact(begin_indices.len()).ok()?;
    for index in (0..begin_indices.len()).rev() {
        let Ok(begin) = usize::try_from(begin_indices[index]) else {
            continue;
        };
        while let Some(&candidate) = stack.last() {
            let candidate_begin = usize::try_from(begin_indices[candidate]).ok()?;
            if candidate_begin > begin {
                break;
            }
            stack.pop();
        }
        if let Some(&candidate) = stack.last() {
            next_greater[index] = usize::try_from(begin_indices[candidate]).ok()?;
        }
        stack.push(index);
    }

    let mut sources = Vec::new();
    sources.try_reserve_exact(binding_count).ok()?;
    sources.resize(binding_count, usize::MAX);
    let successor_count = binding_count.checked_add(1)?;
    let mut successors = Vec::new();
    successors.try_reserve_exact(successor_count).ok()?;
    successors.extend(0..successor_count);

    for (parameter_index, &begin) in begin_indices.iter().enumerate() {
        let Ok(begin) = usize::try_from(begin) else {
            continue;
        };
        let end = *next_greater.get(parameter_index)?;
        if begin > end || end > binding_count {
            return None;
        }
        let mut slot = find_unassigned(&mut successors, begin)?;
        while slot < end {
            sources[slot] = parameter_index;
            let next = find_unassigned(&mut successors, slot.checked_add(1)?)?;
            successors[slot] = next;
            slot = next;
        }
    }
    (!sources.contains(&usize::MAX)).then_some(sources)
}

fn find_unassigned(successors: &mut [usize], index: usize) -> Option<usize> {
    let mut root = index;
    while *successors.get(root)? != root {
        root = successors[root];
    }
    let mut current = index;
    while successors[current] != current {
        let parent = successors[current];
        successors[current] = root;
        current = parent;
    }
    Some(root)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn expands_each_parameter_binding_range_to_its_parameter() {
        // Parameter 0 owns bindings [0,1); parameter 1 owns [1,3); parameter 2
        // owns [3,4). Bindings 1 and 2 both belong to parameter 1.
        let begin = [0, 1, 3];
        let sources = expand_binding_parameter_indices(&begin, 4).unwrap();
        assert_eq!(sources, vec![0, 1, 1, 2]);
    }

    #[test]
    fn parameters_without_a_binding_repeat_the_previous_begin() {
        // Parameters 2 and 3 have no binding: their begin repeats an earlier
        // value already claimed, so they own nothing and parameter 4 owns the
        // last binding.
        let begin = [0, 1, 0, 0, 2];
        let sources = expand_binding_parameter_indices(&begin, 3).unwrap();
        assert_eq!(sources, vec![0, 1, 4]);
    }

    #[test]
    fn repeated_begin_indices_skip_already_claimed_bindings() {
        let mut begin = vec![0; 10_000];
        begin.push(1);

        let sources = expand_binding_parameter_indices(&begin, 2)
            .expect("重复 sentinel 应当线性跳过已分配 binding");

        assert_eq!(sources, vec![0, 10_000]);
    }

    #[test]
    fn hiyori_binding_parameter_map_matches_layout() {
        let bytes = std::fs::read("assets/models/Hiyori/Hiyori.moc3").unwrap();
        let bindings = Moc3KeyformBindings::parse(&bytes).unwrap();

        // 70 parameters, 72 bindings: parameters 30 and 31 each own two bindings,
        // so the map diverges from the identity after binding 30.
        assert_eq!(bindings.binding_parameter_indices.len(), 72);
        assert_eq!(
            &bindings.binding_parameter_indices[30..36],
            &[30, 30, 31, 31, 32, 33]
        );
        // Bindings past the parameter count resolve to real parameters.
        assert_eq!(bindings.binding_parameter_indices[70], 68);
        assert_eq!(bindings.binding_parameter_indices[71], 69);
    }
}
