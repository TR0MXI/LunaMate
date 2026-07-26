//! 组合对话服务、供应商与人格设置、人格记忆与桌宠视图，并向应用提供窄接口。

mod media;
mod memory;
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
    config::{
        CONFIG, DEFAULT_PERSONA_ID, PersonaConfig, PersonaContextLimits, SharedLlmSettings,
        SharedPersonaSettings,
    },
    database::{Database, DatabaseError},
};

pub(crate) use memory::{
    ContextUsage, LiveContextUsage, MemoryScope, PersonaMemory, PersonaMemoryUsage,
};
use session::{ChatLimits, ChatSession, ChatSessionSnapshot};
pub(crate) use settings::{
    AgentSettingsDraft, AgentSettingsEvent, AgentSettingsView, PersonaSettingsDraft,
    PersonaSettingsEvent, PersonaSettingsView,
};
use store::ChatSessionStore;
pub(crate) use view::AgentView;

/// 保存启动时恢复的会话与配置，直到主窗口挂载对应视图。
pub(crate) struct Agent {
    settings: SharedLlmSettings,
    persona: SharedPersonaSettings,
    active_persona: String,
    session: ChatSession,
    store: Arc<ChatSessionStore>,
    memory: AgentMemoryAccess,
    initial_status: Option<String>,
}

impl Agent {
    /// 从全局配置读取供应商与人格设置，并按当前人格从数据库恢复短期上下文。
    pub(crate) async fn load(database: Result<Arc<Database>, DatabaseError>) -> Self {
        let settings = CONFIG.llm_settings();
        let persona = CONFIG.persona_settings();
        // 人格配置在解析阶段保证非空，这里的兜底只覆盖理论上的空列表。
        let (active_persona, limits) = persona
            .active()
            .map(|persona| (persona.id.clone(), chat_limits(persona)))
            .unwrap_or_else(|| (DEFAULT_PERSONA_ID.to_owned(), ChatLimits::default()));

        let (session, store, memory, initial_status) = match database {
            Ok(database) => {
                match ChatSessionStore::load(database.clone(), &active_persona, limits).await {
                    Ok((session, store)) => {
                        (session, store, AgentMemoryAccess::new(Some(database)), None)
                    }
                    Err(error) => Self::without_persistence(error.to_string()),
                }
            }
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
            persona,
            active_persona,
            session,
            store,
            memory,
            initial_status,
        }
    }

    fn without_persistence(
        error: String,
    ) -> (
        ChatSession,
        Arc<ChatSessionStore>,
        AgentMemoryAccess,
        Option<String>,
    ) {
        (
            ChatSession::default(),
            ChatSessionStore::unavailable(),
            AgentMemoryAccess::new(None),
            Some(t!("chat.persistence_unavailable", error = error).to_string()),
        )
    }

    /// 返回供设置窗口按人格访问记忆的句柄。
    pub(crate) fn memory_access(&self) -> AgentMemoryAccess {
        self.memory.clone()
    }

    /// 将已加载的 Agent 状态挂载为桌宠窗口中的视图实体。
    pub(crate) fn mount(self, window: &mut Window, cx: &mut App) -> Entity<AgentView> {
        let Self {
            settings,
            persona,
            active_persona,
            session,
            store,
            memory,
            initial_status,
        } = self;
        let view = cx.new(|cx| {
            AgentView::new(
                settings,
                persona,
                active_persona,
                session,
                store,
                memory,
                initial_status,
                window,
                cx,
            )
        });
        view.update(cx, |view, cx| view.start_initial_reply_fade(cx));
        view
    }
}

/// 把人格的上下文限制翻译为会话限制；配置层已经保证取值落在可接受区间内。
fn chat_limits(persona: &PersonaConfig) -> ChatLimits {
    chat_limits_from_context(persona.context)
}

fn chat_limits_from_context(context: PersonaContextLimits) -> ChatLimits {
    let max_messages =
        usize::try_from(context.effective_messages()).unwrap_or(ChatLimits::default().max_messages);
    ChatLimits {
        max_messages,
        max_bytes: context.effective_bytes(),
    }
}

/// 供设置窗口按人格访问记忆的句柄，不向 UI 暴露数据库类型。
///
/// 数据库初始化失败时句柄依然可以构造，但派生出的 [`PersonaMemory`] 会明确报错，
/// 而不是把"读不到"伪装成"没有记忆"。
#[derive(Clone, Default)]
pub(crate) struct AgentMemoryAccess {
    database: Option<Arc<Database>>,
    live_context_usage: LiveContextUsage,
}

impl AgentMemoryAccess {
    fn new(database: Option<Arc<Database>>) -> Self {
        Self {
            database,
            live_context_usage: LiveContextUsage::default(),
        }
    }

    /// 返回当前人格上下文占用的最新值共享状态。
    pub(crate) fn live_context_usage(&self) -> LiveContextUsage {
        self.live_context_usage.clone()
    }

    /// 返回绑定到指定人格的记忆句柄。
    pub(crate) fn persona(&self, persona_id: &str) -> PersonaMemory {
        PersonaMemory::new(self.database.clone(), persona_id)
    }

    /// 返回嵌入式数据库是否可用，供界面区分"没有记忆"与"读不到记忆"。
    pub(crate) fn is_available(&self) -> bool {
        self.database.is_some()
    }

    /// 供 Agent 内部换入人格上下文使用；数据库句柄不会离开 `agent` 模块。
    pub(super) fn database(&self) -> Option<Arc<Database>> {
        self.database.clone()
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

    /// 在后台执行最终会话保存；数据库不可用时静默跳过，启动时已提示过一次。
    pub(crate) async fn persist(self) -> Result<(), String> {
        if !self.store.is_available() {
            return Ok(());
        }
        self.store
            .save(self.snapshot)
            .await
            .map_err(|error| error.to_string())
    }
}
