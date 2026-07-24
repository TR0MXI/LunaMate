//! 定义 GPUI 输入与后台 Live2D 模型之间的交互边界。
//!
//! UI 只发送语义命令并检测不可变帧快照，不直接访问 Mocari runtime。

pub(in crate::model) mod hit_area;

use std::sync::{
    Arc,
    mpsc::{Receiver, SyncSender, sync_channel},
};

pub(crate) use hit_area::{RenderedHitArea, RenderedModelFrame, render_hit_areas};

pub(in crate::model) const COMMAND_CHANNEL_CAPACITY: usize = 16;

/// 每个渲染帧最多处理的离散模型命令数。
pub(crate) const MAX_COMMANDS_PER_FRAME: usize = 8;

/// UI 侧持有的非阻塞模型命令发送端。
pub(crate) type ModelCommandSender = SyncSender<ModelCommand>;

/// 创建当前模型 generation 专用的有界命令通道。
pub(crate) fn command_channel() -> (ModelCommandSender, Receiver<ModelCommand>) {
    sync_channel(COMMAND_CHANNEL_CAPACITY)
}

/// GPUI 可以发送给后台 Live2D 模型的离散命令。
#[derive(Debug)]
pub(crate) enum ModelCommand {
    /// 请求模型响应一次已确认的 HitArea 点击。
    ActivateHitArea(HitAreaActivation),
    /// 请求预览指定动作组中的下一个可用动作。
    PreviewMotion(String),
    /// 请求预览指定名称的表情。
    PreviewExpression(String),
    /// 请求恢复模型清单声明的默认表情。
    ResetExpression,
}

/// 与具体渲染帧命中结果对应的 HitArea 语义数据。
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct HitAreaActivation {
    id: Arc<str>,
    name: Arc<str>,
}

impl HitAreaActivation {
    /// 创建一个不依赖 Mocari 生命周期的 HitArea 激活事件。
    pub(crate) fn new(id: Arc<str>, name: Arc<str>) -> Self {
        Self { id, name }
    }

    /// 返回模型清单中的 HitArea ID。
    pub(crate) fn id(&self) -> &str {
        &self.id
    }

    /// 返回用于动作匹配的 HitArea 名称。
    pub(crate) fn name(&self) -> &str {
        &self.name
    }
}

/// 按从具体部位到通用动作的顺序尝试点击动作组，并在首个成功项后停止。
pub(in crate::model) fn try_tap_motion_groups(
    area: &HitAreaActivation,
    mut try_group: impl FnMut(&str) -> bool,
) -> bool {
    let mut candidate = String::with_capacity(4 + area.name().len().max(area.id().len()));
    for (index, label) in [area.name(), area.id()].into_iter().enumerate() {
        if label.is_empty() || (index == 1 && label == area.name()) {
            continue;
        }
        candidate.clear();
        candidate.push_str("Tap@");
        candidate.push_str(label);
        if try_group(&candidate) {
            return true;
        }
        candidate.remove(3);
        if try_group(&candidate) {
            return true;
        }
    }
    try_group("Tap")
}
