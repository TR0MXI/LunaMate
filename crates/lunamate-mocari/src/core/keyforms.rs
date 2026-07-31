#[derive(Debug, Copy, Clone, PartialEq)]
pub struct KeyformAxisInterval {
    left_index: usize,
    t: f32,
}

impl KeyformAxisInterval {
    pub fn left_index(&self) -> usize {
        self.left_index
    }

    pub fn t(&self) -> f32 {
        self.t
    }
}

#[derive(Debug, Copy, Clone, PartialEq)]
pub struct KeyformAxis {
    left_index: usize,
    t: f32,
    stride: usize,
}

impl KeyformAxis {
    pub fn new(left_index: usize, t: f32, stride: usize) -> Self {
        Self {
            left_index,
            t,
            stride,
        }
    }
}

#[derive(Debug, Copy, Clone, PartialEq)]
pub struct KeyformRuntimeSlot {
    pub(crate) local_index: usize,
    pub(crate) weight: f32,
}

impl KeyformRuntimeSlot {
    pub fn flat_index(&self) -> usize {
        self.local_index
    }

    pub fn weight(&self) -> f32 {
        self.weight
    }
}

pub fn compute_keyform_axis_interval(keys: &[f32], value: f32) -> Option<KeyformAxisInterval> {
    let first = *keys.first()?;
    if value <= first {
        return Some(KeyformAxisInterval {
            left_index: 0,
            t: 0.0,
        });
    }

    let last_index = keys.len() - 1;
    if value >= keys[last_index] {
        return Some(KeyformAxisInterval {
            left_index: last_index,
            t: 0.0,
        });
    }

    for index in 0..last_index {
        let left = keys[index];
        let right = keys[index + 1];
        if left <= value && value <= right {
            return Some(KeyformAxisInterval {
                left_index: index,
                t: (value - left) / (right - left),
            });
        }
    }

    Some(KeyformAxisInterval {
        left_index: last_index,
        t: 0.0,
    })
}

pub fn expand_keyform_runtime_slots(axes: &[KeyformAxis]) -> Vec<KeyformRuntimeSlot> {
    let mut slots = Vec::new();
    let _ = expand_keyform_runtime_slots_into(axes, &mut slots);
    slots
}

pub(crate) fn expand_keyform_runtime_slots_into(
    axes: &[KeyformAxis],
    slots: &mut Vec<KeyformRuntimeSlot>,
) -> Option<()> {
    slots.clear();
    let result = (|| {
        let active_count = axes.iter().filter(|axis| axis.t != 0.0).count();
        let slot_count = u32::try_from(active_count)
            .ok()
            .and_then(|count| 1usize.checked_shl(count))
            .filter(|count| *count <= MAX_RUNTIME_KEYFORM_SLOTS)?;
        slots.try_reserve(slot_count).ok()?;

        for mask in 0..slot_count {
            let mut flat_index = 0usize;
            let mut weight = 1.0f32;
            let mut bit = 0usize;

            for axis in axes {
                if axis.t == 0.0 {
                    let next = axis
                        .left_index
                        .checked_mul(axis.stride)
                        .and_then(|offset| flat_index.checked_add(offset))?;
                    flat_index = next;
                    continue;
                }

                let use_right = ((mask >> bit) & 1) != 0;
                bit += 1;

                let axis_index = axis.left_index.checked_add(usize::from(use_right))?;
                let next = axis_index
                    .checked_mul(axis.stride)
                    .and_then(|offset| flat_index.checked_add(offset))?;
                flat_index = next;
                if use_right {
                    weight *= axis.t;
                } else {
                    weight *= 1.0 - axis.t;
                }
            }

            slots.push(KeyformRuntimeSlot {
                local_index: flat_index,
                weight,
            });
        }

        Some(())
    })();
    if result.is_none() {
        slots.clear();
    }
    result
}

const MAX_RUNTIME_KEYFORM_SLOTS: usize = 65_536;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_exponential_slot_expansion_above_the_runtime_limit() {
        let axes = vec![KeyformAxis::new(0, 0.5, 1); 17];

        assert!(expand_keyform_runtime_slots(&axes).is_empty());
    }

    #[test]
    fn expands_a_small_axis_set() {
        let axes = [KeyformAxis::new(0, 0.5, 1), KeyformAxis::new(0, 0.5, 2)];

        assert_eq!(expand_keyform_runtime_slots(&axes).len(), 4);
    }

    #[test]
    fn reused_slot_storage_matches_owned_expansion() {
        let axes = [KeyformAxis::new(0, 0.25, 1), KeyformAxis::new(1, 0.75, 3)];
        let expected = expand_keyform_runtime_slots(&axes);
        let mut slots = Vec::new();

        expand_keyform_runtime_slots_into(&axes, &mut slots).expect("有效轴应能写入复用缓冲");
        let pointer = slots.as_ptr();
        assert_eq!(slots, expected);

        expand_keyform_runtime_slots_into(&axes, &mut slots).expect("第二次写入复用缓冲应成功");
        assert_eq!(slots.as_ptr(), pointer);
        assert_eq!(slots, expected);
    }
}
