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

use std::{collections::HashMap, sync::Arc, time::Instant};

use async_channel::{Receiver, Sender};
use gpui::{App, AppContext, Entity, Window};
use parking_lot::Mutex;
use rust_i18n::t;

use crate::{
    config::{
        CONFIG, DEFAULT_MAX_OUTPUT_TOKENS, DEFAULT_PERSONA_ID, LlmSettings,
        MODEL_CONTEXT_RESERVE_TOKENS, PersonaConfig, SharedLlmSettings, SharedPersonaSettings,
    },
    database::{Database, DatabaseError},
};

pub(crate) use memory::{
    AssistantTrace, ContextMessage, ContextMessageRole, ContextUsage, LiveContextUsage,
    MemoryScope, PersonaMemory, PersonaMemoryUsage, ToolExecutionTrace,
};
use session::{ChatLimits, ChatSession, ChatSessionSnapshot, estimate_text_tokens};
pub(crate) use settings::{
    AgentSettingsDraft, AgentSettingsEvent, AgentSettingsView, ContextMutationCompletion,
    PersonaSettingsDraft, PersonaSettingsEvent, PersonaSettingsView,
};
use store::{ChatSessionStore, SessionOperationReservation};
pub(crate) use view::AgentView;

/// Agent 工具请求桌宠切换到当前模型的一套服装。
#[derive(Clone)]
pub(crate) struct AgentOutfitRequest {
    outfit: String,
    revision: u64,
    result: Sender<AgentOutfitResult>,
}

#[derive(Clone, Copy)]
enum AgentOutfitResult {
    Applied,
    Failed,
}

impl AgentOutfitRequest {
    /// 创建一次有界换装请求及其单消费者结果端。
    fn channel(outfit: String, revision: u64) -> (Self, Receiver<AgentOutfitResult>) {
        let (result, receiver) = async_channel::bounded(1);
        (
            Self {
                outfit,
                revision,
                result,
            },
            receiver,
        )
    }

    /// 返回模型选择的本地化服装名称。
    pub(crate) fn outfit(&self) -> &str {
        &self.outfit
    }

    /// 返回创建请求时的服装清单 revision，用于拒绝模型切换后的迟到调用。
    pub(crate) fn revision(&self) -> u64 {
        self.revision
    }

    /// 返回请求结果接收端是否已随会话取消或窗口关闭而消失。
    fn is_cancelled(&self) -> bool {
        self.result.is_closed()
    }

    /// 将 GPUI 线程上的换装受理结果交还给后台工具循环。
    pub(crate) fn complete(&self, applied: bool) {
        let result = if applied {
            AgentOutfitResult::Applied
        } else {
            AgentOutfitResult::Failed
        };
        let _ = self.result.try_send(result);
    }
}

/// Agent 视图向桌宠根视图发布的本地能力请求。
pub(crate) enum AgentViewEvent {
    ChangeOutfit(AgentOutfitRequest),
}

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
        let started = Instant::now();
        let settings = CONFIG.llm_settings();
        let persona = CONFIG.persona_settings();
        // 人格配置在解析阶段保证非空，这里的兜底只覆盖理论上的空列表。
        let (active_persona, limits) = persona
            .active()
            .map(|persona| (persona.id.clone(), chat_limits(persona, &settings)))
            .unwrap_or_else(|| (DEFAULT_PERSONA_ID.to_owned(), ChatLimits::default()));

        let (session, store, memory, initial_status) = match database {
            Ok(database) => {
                let memory = AgentMemoryAccess::new(Some(database.clone()));
                match ChatSessionStore::load_with_lock(
                    database,
                    &active_persona,
                    limits,
                    memory.session_document_lock(),
                )
                .await
                {
                    Ok((session, store)) => (session, store, memory, None),
                    Err(error) => {
                        log::error!(
                            "恢复聊天会话失败，持久化已禁用：error_kind={}",
                            error.diagnostic_kind()
                        );
                        Self::without_persistence(error.to_string(), limits)
                    }
                }
            }
            Err(error) => {
                log::error!(
                    "{}",
                    t!("log.database_init_failed", error = error.diagnostic_kind())
                );
                Self::without_persistence(error.to_string(), limits)
            }
        };
        let usage = session.usage();
        log::info!(
            "Agent 已就绪：providers={}, personas={}, persistence_available={}, restored_messages={}, restored_tokens={}, elapsed_ms={}",
            settings.models.len(),
            persona.personas.len(),
            store.is_available(),
            usage.messages,
            usage.tokens,
            started.elapsed().as_millis()
        );
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
        limits: ChatLimits,
    ) -> (
        ChatSession,
        Arc<ChatSessionStore>,
        AgentMemoryAccess,
        Option<String>,
    ) {
        (
            ChatSession::new(limits).unwrap_or_default(),
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
fn chat_limits(persona: &PersonaConfig, settings: &LlmSettings) -> ChatLimits {
    let max_messages = usize::try_from(persona.context.effective_messages())
        .unwrap_or(ChatLimits::default().max_messages);
    let persona_tokens = usize::try_from(persona.context.effective_tokens())
        .unwrap_or(ChatLimits::default().max_tokens);
    let model = persona
        .model
        .as_deref()
        .and_then(|id| settings.model(id))
        .or_else(|| settings.selected());
    let model_budgets = model
        .and_then(|model| model.advanced.context_window_tokens)
        .and_then(|window| usize::try_from(window).ok())
        .map(|window| {
            let output = model
                .and_then(|model| model.advanced.max_output_tokens)
                .and_then(|tokens| usize::try_from(tokens).ok())
                .unwrap_or(DEFAULT_MAX_OUTPUT_TOKENS as usize);
            let prompt = estimate_text_tokens(&persona.system_prompt);
            // 为消息角色、请求包装和常规工具 schema 保留固定空间；图片仍按消息单独估算。
            let stored =
                window.saturating_sub(prompt.saturating_add(MODEL_CONTEXT_RESERVE_TOKENS as usize));
            let request = stored.saturating_sub(output);
            (stored, request)
        });
    let (max_tokens, max_request_tokens) = model_budgets
        .map_or((persona_tokens, persona_tokens), |(stored, request)| {
            (persona_tokens.min(stored), persona_tokens.min(request))
        });
    ChatLimits {
        max_messages,
        max_tokens,
        max_request_tokens,
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
    session_document_lock: store::SessionDocumentLock,
    deleted_persona_cleanup: Arc<Mutex<HashMap<String, DeletedPersonaCleanupState>>>,
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum DeletedPersonaCleanupState {
    InFlight,
    Completed,
}

impl AgentMemoryAccess {
    fn new(database: Option<Arc<Database>>) -> Self {
        Self {
            database,
            live_context_usage: LiveContextUsage::default(),
            session_document_lock: Arc::new(store::SessionDocumentCoordinator::new()),
            deleted_persona_cleanup: Arc::default(),
        }
    }

    /// 返回当前人格上下文占用的最新值共享状态。
    pub(crate) fn live_context_usage(&self) -> LiveContextUsage {
        self.live_context_usage.clone()
    }

    /// 返回绑定到指定人格的记忆句柄。
    pub(crate) fn persona(&self, persona_id: &str) -> PersonaMemory {
        PersonaMemory::new(
            self.database.clone(),
            self.session_document_lock.clone(),
            persona_id,
        )
    }

    /// 返回嵌入式数据库是否可用，供界面区分"没有记忆"与"读不到记忆"。
    pub(crate) fn is_available(&self) -> bool {
        self.database.is_some()
    }

    /// 幂等清理由配置 tombstone 标记的已删除人格；调用方必须保证 ID 不在当前人格列表中。
    pub(crate) async fn cleanup_deleted_persona(&self, persona_id: &str) -> Result<(), String> {
        let database = self
            .database
            .as_ref()
            .ok_or_else(|| "嵌入式数据库当前不可用".to_owned())?;
        let operation = self.session_document_lock.reserve();
        let context = store::delete_persona_session_reserved(database, persona_id, operation)
            .await
            .map_err(|error| error.to_string());
        let memories = self
            .persona(persona_id)
            .clear(MemoryScope::All)
            .await
            .map_err(|error| error.to_string());
        match (context, memories) {
            (Ok(()), Ok(())) => Ok(()),
            (Err(context), Ok(())) => Err(context),
            (Ok(()), Err(memories)) => Err(memories),
            (Err(context), Err(memories)) => Err(format!("{context}; {memories}")),
        }
    }

    /// 抢占一个已删除人格的全局清理权；同一 ID 在完成发布前最多只有一个消费者。
    pub(crate) fn claim_deleted_persona_cleanup(&self, persona_id: &str) -> bool {
        let mut cleanup = self.deleted_persona_cleanup.lock();
        if cleanup.contains_key(persona_id) {
            return false;
        }
        cleanup.insert(persona_id.to_owned(), DeletedPersonaCleanupState::InFlight);
        true
    }

    pub(crate) fn complete_deleted_persona_cleanup(&self, persona_id: &str) {
        self.deleted_persona_cleanup
            .lock()
            .insert(persona_id.to_owned(), DeletedPersonaCleanupState::Completed);
    }

    pub(crate) fn fail_deleted_persona_cleanup(&self, persona_id: &str) {
        let mut cleanup = self.deleted_persona_cleanup.lock();
        if cleanup.get(persona_id) == Some(&DeletedPersonaCleanupState::InFlight) {
            cleanup.remove(persona_id);
        }
    }

    pub(crate) fn deleted_persona_cleanup_is_completed(&self, persona_id: &str) -> bool {
        self.deleted_persona_cleanup.lock().get(persona_id)
            == Some(&DeletedPersonaCleanupState::Completed)
    }

    pub(crate) fn completed_deleted_persona_cleanups(&self) -> Vec<String> {
        self.deleted_persona_cleanup
            .lock()
            .iter()
            .filter(|(_, state)| **state == DeletedPersonaCleanupState::Completed)
            .map(|(persona, _)| persona.clone())
            .collect()
    }

    /// tombstone 已发布移除后释放 ID；此前新任务不能按同一字符串再次删除数据。
    pub(crate) fn release_deleted_persona_cleanup(&self, persona_id: &str) {
        let mut cleanup = self.deleted_persona_cleanup.lock();
        if cleanup.get(persona_id) == Some(&DeletedPersonaCleanupState::Completed) {
            cleanup.remove(persona_id);
        }
    }

    /// 供 Agent 内部换入人格上下文使用；数据库句柄不会离开 `agent` 模块。
    pub(super) fn database(&self) -> Option<Arc<Database>> {
        self.database.clone()
    }

    /// 返回 Agent 内全部会话读改写共享的串行化屏障。
    fn session_document_lock(&self) -> store::SessionDocumentLock {
        self.session_document_lock.clone()
    }
}

/// 封装退出边界上的最终会话写入，不向应用暴露存储或快照类型。
pub(crate) struct AgentShutdown {
    store: Arc<ChatSessionStore>,
    snapshot: ChatSessionSnapshot,
    operation: SessionOperationReservation,
}

impl AgentShutdown {
    fn new(
        store: Arc<ChatSessionStore>,
        snapshot: ChatSessionSnapshot,
        operation: SessionOperationReservation,
    ) -> Self {
        Self {
            store,
            snapshot,
            operation,
        }
    }

    /// 在后台执行最终会话保存；数据库不可用时静默跳过，启动时已提示过一次。
    pub(crate) async fn persist(self) -> Result<(), String> {
        if !self.store.is_available() {
            return Ok(());
        }
        self.store
            .save_reserved(self.snapshot, self.operation)
            .await
            .map_err(|error| error.to_string())
    }
}
