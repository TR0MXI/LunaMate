//! 检查模型声明的可交互能力，并把 Mocari 元数据转换为应用内部描述。
//!
//! 本模块只描述模型具备的能力，不负责播放动作、处理输入或修改运行时状态。

mod diagnostics;
mod resources;

use std::{collections::BTreeMap, sync::Arc};

use mocari::assets::RuntimeModel;

pub(crate) use diagnostics::{ModelDiagnosticCategory, ModelLoadDiagnostic, ModelLoadDiagnostics};
pub(crate) use resources::{
    AuxiliaryResourceBudget, ExternalExpressionReference, MAX_AUXILIARY_RESOURCE_BYTES,
    ModelResourceResolver,
};

const MAX_HIT_AREA_COUNT: usize = 256;

/// 标识诊断对应的模型资源类型。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ModelDiagnosticResource {
    /// 模型清单声明的动作资源。
    Motion,
    /// 模型清单声明的表情资源。
    Expression,
    /// 模型清单声明的命中区域。
    HitArea,
}

/// 已解析到具体 Drawable 的模型命中区域。
#[derive(Clone, Debug)]
pub(crate) struct HitAreaCapability {
    name: Arc<str>,
    drawable_index: usize,
    bounds_slot: usize,
}

impl HitAreaCapability {
    /// 构造不依赖 `.moc3` 运行时的命中区域，用于验证逐帧包围盒计算。
    #[cfg(test)]
    pub(in crate::model) fn new_for_test(
        name: &str,
        drawable_index: usize,
        bounds_slot: usize,
    ) -> Self {
        Self {
            name: Arc::from(name),
            drawable_index,
            bounds_slot,
        }
    }

    /// 返回模型清单中面向交互语义的区域名称。
    pub(crate) fn name(&self) -> &Arc<str> {
        &self.name
    }

    /// 返回当前 HitArea 对应的模型 Drawable 索引。
    pub(crate) fn drawable_index(&self) -> usize {
        self.drawable_index
    }

    /// 返回当前 generation 中复用 Drawable 包围盒的紧凑槽位。
    pub(crate) fn bounds_slot(&self) -> usize {
        self.bounds_slot
    }
}

/// 保存模型加载后可供 LunaMate 使用的能力目录。
pub(crate) struct ModelCapabilities {
    hit_areas: Vec<HitAreaCapability>,
    hit_area_bounds_count: usize,
}

impl ModelCapabilities {
    /// 检查模型元数据，并解析当前已支持的能力。
    pub(crate) fn inspect(model: &RuntimeModel) -> (Self, ModelLoadDiagnostics) {
        let runtime = model.runtime();
        let mut hit_areas = Vec::new();
        let mut bounds_slots = BTreeMap::new();
        let mut diagnostics = ModelLoadDiagnostics::default();

        let declared_hit_areas = runtime.model().hit_areas();
        for (index, hit_area) in declared_hit_areas
            .iter()
            .take(MAX_HIT_AREA_COUNT)
            .enumerate()
        {
            let Some(drawable_index) = runtime.drawable_index(hit_area.id()) else {
                diagnostics.push(ModelLoadDiagnostic::hit_area(
                    hit_area.name(),
                    index,
                    hit_area.id(),
                    ModelDiagnosticCategory::InvalidReference,
                    "未引用有效的 Drawable",
                ));
                continue;
            };

            let bounds_slot = match bounds_slots.get(&drawable_index) {
                Some(slot) => *slot,
                None => {
                    let slot = bounds_slots.len();
                    bounds_slots.insert(drawable_index, slot);
                    slot
                }
            };
            hit_areas.push(HitAreaCapability {
                name: Arc::from(hit_area.name()),
                drawable_index,
                bounds_slot,
            });
        }
        if let Some(hit_area) = declared_hit_areas.get(MAX_HIT_AREA_COUNT) {
            diagnostics.push(
                ModelLoadDiagnostic::hit_area(
                    hit_area.name(),
                    MAX_HIT_AREA_COUNT,
                    hit_area.id(),
                    ModelDiagnosticCategory::LimitExceeded,
                    format!(
                        "HitArea 声明总数为 {}，仅处理前 {MAX_HIT_AREA_COUNT} 项",
                        declared_hit_areas.len()
                    ),
                )
                .with_affected_count(declared_hit_areas.len() - MAX_HIT_AREA_COUNT),
            );
        }

        (
            Self {
                hit_areas,
                hit_area_bounds_count: bounds_slots.len(),
            },
            diagnostics,
        )
    }

    /// 返回按模型清单声明顺序排列的有效 HitArea。
    pub(crate) fn hit_areas(&self) -> &[HitAreaCapability] {
        &self.hit_areas
    }

    /// 返回一帧需要计算的唯一 HitArea Drawable 数量。
    pub(crate) fn hit_area_bounds_count(&self) -> usize {
        self.hit_area_bounds_count
    }
}
