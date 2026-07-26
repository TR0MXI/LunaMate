//! 定义 GPUI 输入与后台 Live2D 模型之间的交互边界。
//!
//! UI 只发送语义命令并检测不可变帧快照，不直接访问 Mocari runtime。

pub(in crate::model) mod hit_area;

use std::sync::mpsc::{Receiver, SyncSender, sync_channel};

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
    /// 请求预览指定动作组中的下一个可用动作。
    PreviewMotion(String),
    /// 请求预览指定名称的表情。
    PreviewExpression(String),
    /// 请求恢复模型清单声明的默认表情。
    ResetExpression,
}
