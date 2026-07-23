//! 管理聊天会话、`genai` 流式服务与桌宠窗口中的对话界面。

mod service;
mod session;
mod store;
mod view;

pub(crate) use session::{
    ChatContextMessage, ChatMessage, ChatMessageState, ChatRole, ChatSession, ResponseId,
};
pub(crate) use store::ChatSessionStore;
pub(crate) use view::ChatView;
