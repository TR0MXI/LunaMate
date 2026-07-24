//! 组合对话服务、Provider 设置、会话存储与桌宠视图，并向应用提供窄接口。

mod palette;
mod service;
mod session;
mod settings;
mod store;
mod view;

#[cfg(test)]
mod tests;

use std::sync::Arc;

use gpui::{App, AppContext, Entity, Window};
use rust_i18n::t;

use crate::config::{CONFIG, SharedLlmSettings};

use session::{ChatSession, ChatSessionSnapshot};
pub(crate) use settings::{AgentSettingsDraft, AgentSettingsEvent, AgentSettingsView};
use store::ChatSessionStore;
pub(crate) use view::AgentView;

/// 保存启动时恢复的会话与配置，直到主窗口挂载对应视图。
pub(crate) struct Agent {
    settings: SharedLlmSettings,
    session: ChatSession,
    store: Arc<ChatSessionStore>,
    initial_status: Option<String>,
}

impl Agent {
    /// 从全局配置读取 LLM 设置和会话路径，并在快照损坏时降级为空会话。
    pub(crate) fn load() -> Self {
        let settings = CONFIG.llm_settings();
        let session_path = CONFIG.chat_session_path();
        let (session, store, initial_status) = match ChatSessionStore::load(session_path.clone()) {
            Ok((session, store)) => (session, store, None),
            Err(error) => (
                ChatSession::default(),
                ChatSessionStore::empty(session_path),
                Some(t!("chat.restore_failed", error = error.to_string()).to_string()),
            ),
        };
        Self {
            settings,
            session,
            store,
            initial_status,
        }
    }

    /// 将已加载的 Agent 状态挂载为桌宠窗口中的视图实体。
    pub(crate) fn mount(self, window: &mut Window, cx: &mut App) -> Entity<AgentView> {
        let Self {
            settings,
            session,
            store,
            initial_status,
        } = self;
        cx.new(|cx| AgentView::new(settings, session, store, initial_status, window, cx))
    }
}

/// 封装退出边界上的最终会话写入，不向应用暴露存储或快照类型。
pub(crate) struct AgentShutdown {
    store: Arc<ChatSessionStore>,
    snapshot: ChatSessionSnapshot,
}

impl AgentShutdown {
    fn new(store: Arc<ChatSessionStore>, snapshot: ChatSessionSnapshot) -> Self {
        Self { store, snapshot }
    }

    /// 在后台执行最终会话保存。
    pub(crate) fn persist(self) -> Result<(), String> {
        self.store
            .save(self.snapshot)
            .map_err(|error| error.to_string())
    }
}
