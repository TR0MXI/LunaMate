//! 组合对话服务、Provider 设置、会话存储与桌宠视图，并向应用提供窄接口。

mod media;
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

use crate::{
    config::{CONFIG, SharedLlmSettings},
    database::{Database, DatabaseError},
};

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
    /// 从全局配置读取 LLM 设置，并从数据库恢复会话。
    pub(crate) async fn load(database: Result<Arc<Database>, DatabaseError>) -> Self {
        let settings = CONFIG.llm_settings();
        let (session, store, initial_status) = match database {
            Ok(database) => match ChatSessionStore::load(database).await {
                Ok((session, store)) => (session, store, None),
                Err(error) => Self::without_persistence(error.to_string()),
            },
            Err(error) => {
                log::error!(
                    "{}",
                    t!("log.database_init_failed", error = error.to_string())
                );
                Self::without_persistence(error.to_string())
            }
        };
        Self {
            settings,
            session,
            store,
            initial_status,
        }
    }

    fn without_persistence(error: String) -> (ChatSession, Arc<ChatSessionStore>, Option<String>) {
        (
            ChatSession::default(),
            ChatSessionStore::unavailable(),
            Some(t!("chat.persistence_unavailable", error = error).to_string()),
        )
    }

    /// 将已加载的 Agent 状态挂载为桌宠窗口中的视图实体。
    pub(crate) fn mount(self, window: &mut Window, cx: &mut App) -> Entity<AgentView> {
        let Self {
            settings,
            session,
            store,
            initial_status,
        } = self;
        let view =
            cx.new(|cx| AgentView::new(settings, session, store, initial_status, window, cx));
        view.update(cx, |view, cx| view.start_initial_reply_fade(cx));
        view
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
    pub(crate) async fn persist(self) -> Result<(), String> {
        self.store
            .save(self.snapshot)
            .await
            .map_err(|error| error.to_string())
    }
}
