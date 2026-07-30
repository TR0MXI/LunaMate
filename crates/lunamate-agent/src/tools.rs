//! 定义 Agent 与宿主本地能力之间的框架无关请求。

use async_channel::{Receiver, Sender};

/// Agent 可选择的一套服装，稳定 ID 与用户显示名相互独立。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OutfitOption {
    id: String,
    label: String,
}

impl OutfitOption {
    pub fn new(id: impl Into<String>, label: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            label: label.into(),
        }
    }

    pub fn id(&self) -> &str {
        &self.id
    }

    pub fn label(&self) -> &str {
        &self.label
    }
}

/// Agent 工具请求宿主切换到当前模型的一套服装。
#[derive(Clone)]
pub struct AgentOutfitRequest {
    outfit_id: String,
    revision: u64,
    result: Sender<AgentOutfitResult>,
}

/// 宿主完成换装请求后发送给工具循环的结果。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AgentOutfitResult {
    Applied,
    Failed,
}

impl AgentOutfitRequest {
    /// 创建一次有界换装请求及其单消费者结果端。
    pub fn channel(outfit_id: String, revision: u64) -> (Self, Receiver<AgentOutfitResult>) {
        let (result, receiver) = async_channel::bounded(1);
        (
            Self {
                outfit_id,
                revision,
                result,
            },
            receiver,
        )
    }

    /// 返回模型选择对应的稳定服装 ID。
    pub fn outfit_id(&self) -> &str {
        &self.outfit_id
    }

    /// 返回创建请求时的服装清单 revision。
    pub const fn revision(&self) -> u64 {
        self.revision
    }

    /// 返回结果接收端是否已经随请求取消或宿主关闭而消失。
    pub fn is_cancelled(&self) -> bool {
        self.result.is_closed()
    }

    /// 将宿主的换装结果交还给后台工具循环。
    pub fn complete(&self, applied: bool) {
        let result = if applied {
            AgentOutfitResult::Applied
        } else {
            AgentOutfitResult::Failed
        };
        let _ = self.result.try_send(result);
    }
}
