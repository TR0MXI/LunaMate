//! 提供人格上下文快照、持久化记忆用量和清理入口。

use std::{
    error::Error,
    fmt,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
};

use parking_lot::Mutex;
use rapidhash::RapidHashMap;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::persistence::{
    AgentPersistenceCallbacks, PersistenceError, PersistentMemoryTier, PersistentMemoryUsage,
};
use crate::session::ChatRole;
use crate::store::{
    SessionDocumentCoordinator, SessionDocumentLock, delete_persona_session_reserved,
};

/// 短期上下文的占用量与生效上限。
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct ContextUsage {
    pub messages: usize,
    pub max_messages: usize,
    pub tokens: usize,
    pub max_tokens: usize,
}

/// 一次本地工具执行中可安全展示的 Provider 无关记录。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ToolExecutionTrace {
    name: String,
    arguments: Value,
    result: Value,
}

impl ToolExecutionTrace {
    pub fn new(name: String, arguments: Value, result: Value) -> Self {
        Self {
            name,
            arguments,
            result,
        }
    }

    /// 返回稳定工具名，不包含 Provider call ID。
    pub fn name(&self) -> &str {
        &self.name
    }

    /// 返回模型提交给本地工具的 JSON 参数。
    pub fn arguments(&self) -> &Value {
        &self.arguments
    }

    /// 返回权限复核后生成的脱敏 JSON 结果。
    pub fn result(&self) -> &Value {
        &self.result
    }
}

/// 助手消息附带的可展示推理与本地工具执行详情。
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct AssistantTrace {
    #[serde(deserialize_with = "Option::deserialize")]
    reasoning: Option<String>,
    tool_executions: Vec<ToolExecutionTrace>,
}

impl AssistantTrace {
    pub fn new(reasoning: Option<String>, tool_executions: Vec<ToolExecutionTrace>) -> Self {
        Self {
            reasoning,
            tool_executions,
        }
    }

    /// 返回 Provider 提供的可读推理文本；协议签名不会进入此字段。
    pub fn reasoning(&self) -> Option<&str> {
        self.reasoning.as_deref()
    }

    /// 按实际执行顺序返回本地工具记录。
    pub fn tool_executions(&self) -> &[ToolExecutionTrace] {
        &self.tool_executions
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.reasoning
            .as_ref()
            .is_none_or(|reasoning| reasoning.trim().is_empty())
            && self.tool_executions.is_empty()
    }
}

/// 不含内部状态、图片内容和 Provider 类型的可编辑上下文消息。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ContextMessage {
    pub id: u64,
    pub role: ChatRole,
    pub content: String,
    pub tokens: usize,
    pub fixed_tokens: usize,
    /// 仅供展示；编辑正文不会把该数据回放给 Provider。
    pub trace: Option<AssistantTrace>,
}

/// 人格设置界面展示的三层记忆用量。
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct PersonaMemoryUsage {
    pub context: ContextUsage,
    pub medium: u64,
    pub long: u64,
}

/// 当前人格的最新上下文占用。
///
/// 短期上下文运行时只存在于持有会话的视图里，设置界面无法从数据库读到未落盘的
/// 增量，因此这里使用只保留最新值的共享状态：视图在每次提交快照时发布，界面按需读取。
#[derive(Clone, Default)]
pub struct LiveContextUsage {
    latest: Arc<Mutex<Option<LiveContextSnapshot>>>,
    revision: Arc<AtomicU64>,
}

#[derive(Clone)]
struct LiveContextSnapshot {
    persona_id: String,
    usage: ContextUsage,
    messages: Vec<ContextMessage>,
}

impl LiveContextUsage {
    pub fn publish(&self, persona_id: &str, usage: ContextUsage, messages: Vec<ContextMessage>) {
        *self.latest.lock() = Some(LiveContextSnapshot {
            persona_id: persona_id.to_owned(),
            usage,
            messages,
        });
        self.revision.fetch_add(1, Ordering::Release);
    }

    /// 返回指定人格的实时占用；该人格当前未被加载时返回 `None`。
    pub fn get(&self, persona_id: &str) -> Option<ContextUsage> {
        self.latest
            .lock()
            .as_ref()
            .filter(|snapshot| snapshot.persona_id == persona_id)
            .map(|snapshot| snapshot.usage)
    }

    /// 返回指定人格实时上下文最近一次发布的 revision；非活动人格返回 `None`。
    pub fn revision_for(&self, persona_id: &str) -> Option<u64> {
        self.latest
            .lock()
            .as_ref()
            .is_some_and(|snapshot| snapshot.persona_id == persona_id)
            .then(|| self.revision.load(Ordering::Acquire))
    }

    fn messages(&self, persona_id: &str) -> Option<Vec<ContextMessage>> {
        self.latest
            .lock()
            .as_ref()
            .filter(|snapshot| snapshot.persona_id == persona_id)
            .map(|snapshot| snapshot.messages.clone())
    }
}

/// 提供持久化、实时上下文和人格清理协调的共享记忆句柄。
#[derive(Clone)]
pub struct AgentMemory {
    persistence: Option<AgentPersistenceCallbacks>,
    live_context_usage: LiveContextUsage,
    session_document_lock: SessionDocumentLock,
    deleted_persona_cleanup: Arc<Mutex<RapidHashMap<String, DeletedPersonaCleanupState>>>,
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum DeletedPersonaCleanupState {
    InFlight,
    Completed,
}

impl AgentMemory {
    pub fn new(persistence: Option<AgentPersistenceCallbacks>) -> Self {
        Self {
            persistence,
            live_context_usage: LiveContextUsage::default(),
            session_document_lock: Arc::new(SessionDocumentCoordinator::new()),
            deleted_persona_cleanup: Arc::default(),
        }
    }

    pub fn unavailable() -> Self {
        Self::new(None)
    }

    pub fn live_context_usage(&self) -> LiveContextUsage {
        self.live_context_usage.clone()
    }

    pub fn persona(&self, persona_id: &str) -> PersonaMemory {
        PersonaMemory::new(
            self.persistence.clone(),
            self.session_document_lock.clone(),
            persona_id,
        )
    }

    pub fn is_available(&self) -> bool {
        self.persistence.is_some()
    }

    pub(crate) fn persistence(&self) -> Option<AgentPersistenceCallbacks> {
        self.persistence.clone()
    }

    pub(crate) fn session_document_lock(&self) -> SessionDocumentLock {
        self.session_document_lock.clone()
    }

    pub async fn cleanup_deleted_persona(&self, persona_id: &str) -> Result<(), String> {
        let persistence = self
            .persistence
            .as_ref()
            .ok_or_else(|| "Agent 持久化当前不可用".to_owned())?;
        let operation = self.session_document_lock.reserve();
        let context = delete_persona_session_reserved(persistence, persona_id, operation)
            .await
            .map_err(|error| error.to_string());
        let memories = self
            .persona(persona_id)
            .clear(PersistentMemoryScope::All)
            .await
            .map_err(|error| error.to_string());
        match (context, memories) {
            (Ok(()), Ok(())) => Ok(()),
            (Err(context), Ok(())) => Err(context),
            (Ok(()), Err(memories)) => Err(memories),
            (Err(context), Err(memories)) => Err(format!("{context}; {memories}")),
        }
    }

    pub fn claim_deleted_persona_cleanup(&self, persona_id: &str) -> bool {
        let mut cleanup = self.deleted_persona_cleanup.lock();
        if cleanup.contains_key(persona_id) {
            return false;
        }
        cleanup.insert(persona_id.to_owned(), DeletedPersonaCleanupState::InFlight);
        true
    }

    pub fn complete_deleted_persona_cleanup(&self, persona_id: &str) {
        self.deleted_persona_cleanup
            .lock()
            .insert(persona_id.to_owned(), DeletedPersonaCleanupState::Completed);
    }

    pub fn fail_deleted_persona_cleanup(&self, persona_id: &str) {
        let mut cleanup = self.deleted_persona_cleanup.lock();
        if cleanup.get(persona_id) == Some(&DeletedPersonaCleanupState::InFlight) {
            cleanup.remove(persona_id);
        }
    }

    pub fn deleted_persona_cleanup_is_completed(&self, persona_id: &str) -> bool {
        self.deleted_persona_cleanup.lock().get(persona_id)
            == Some(&DeletedPersonaCleanupState::Completed)
    }

    pub fn completed_deleted_persona_cleanups(&self) -> Vec<String> {
        self.deleted_persona_cleanup
            .lock()
            .iter()
            .filter(|(_, state)| **state == DeletedPersonaCleanupState::Completed)
            .map(|(persona, _)| persona.clone())
            .collect()
    }

    pub fn release_deleted_persona_cleanup(&self, persona_id: &str) {
        let mut cleanup = self.deleted_persona_cleanup.lock();
        if cleanup.get(persona_id) == Some(&DeletedPersonaCleanupState::Completed) {
            cleanup.remove(persona_id);
        }
    }
}

impl Default for AgentMemory {
    fn default() -> Self {
        Self::unavailable()
    }
}

/// 需要清除的持久化记忆范围。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PersistentMemoryScope {
    /// 中期记忆。
    Medium,
    /// 长期记忆。
    Long,
    /// 该人格的全部持久化记忆。
    All,
}

impl PersistentMemoryScope {
    /// 返回日志和持久化操作使用的稳定范围标识。
    pub const fn id(self) -> &'static str {
        match self {
            Self::Medium => "medium",
            Self::Long => "long",
            Self::All => "all",
        }
    }
}

/// 绑定到单个人格的记忆存储句柄。
///
/// 宿主未注册持久化时句柄仍然可用，但所有操作都会返回 [`MemoryError::Unavailable`]，
/// 而不是伪装成"没有记忆"。
#[derive(Clone)]
pub struct PersonaMemory {
    persistence: Option<AgentPersistenceCallbacks>,
    session_document_lock: super::store::SessionDocumentLock,
    persona_id: String,
}

impl PersonaMemory {
    pub(super) fn new(
        persistence: Option<AgentPersistenceCallbacks>,
        session_document_lock: super::store::SessionDocumentLock,
        persona_id: impl Into<String>,
    ) -> Self {
        Self {
            persistence,
            session_document_lock,
            persona_id: persona_id.into(),
        }
    }

    /// 返回该人格三层记忆的当前用量。
    ///
    /// 短期部分优先使用实时占用；该人格未被加载时回退到已落盘的会话文档。
    ///
    /// # Errors
    ///
    /// 持久化不可用或查询失败时返回错误。
    pub async fn usage(
        &self,
        live: LiveContextUsage,
        limits: ContextUsage,
    ) -> Result<PersonaMemoryUsage, MemoryError> {
        let persistence = self.persistence.as_ref().ok_or(MemoryError::Unavailable)?;
        let memory: PersistentMemoryUsage = persistence
            .memory_usage(&self.persona_id)
            .await
            .map_err(MemoryError::Persistence)?;
        let context = match live.get(&self.persona_id) {
            Some(usage) => usage,
            None => super::store::persona_context_usage(
                persistence,
                &self.session_document_lock,
                &self.persona_id,
                limits,
            )
            .await
            .map_err(|error| MemoryError::Stored(error.to_string()))?,
        };
        Ok(PersonaMemoryUsage {
            context,
            medium: memory.medium(),
            long: memory.long(),
        })
    }

    /// 返回设置页可编辑的当前上下文；活动人格优先使用内存快照。
    pub async fn context_messages(
        &self,
        live: LiveContextUsage,
        limits: ContextUsage,
    ) -> Result<Vec<ContextMessage>, MemoryError> {
        if let Some(messages) = live.messages(&self.persona_id) {
            return Ok(messages);
        }
        let persistence = self.persistence.as_ref().ok_or(MemoryError::Unavailable)?;
        super::store::persona_context_messages(
            persistence,
            &self.session_document_lock,
            &self.persona_id,
            limits,
        )
        .await
        .map_err(|error| MemoryError::Stored(error.to_string()))
    }

    /// 删除该人格在数据库中的中期或长期记忆；短期上下文由会话存储单独清除。
    ///
    /// # Errors
    ///
    /// 持久化不可用或删除失败时返回错误。
    pub async fn clear(&self, scope: PersistentMemoryScope) -> Result<(), MemoryError> {
        let tier = match scope {
            PersistentMemoryScope::Medium => Some(PersistentMemoryTier::Medium),
            PersistentMemoryScope::Long => Some(PersistentMemoryTier::Long),
            PersistentMemoryScope::All => None,
        };
        let persistence = self.persistence.as_ref().ok_or(MemoryError::Unavailable)?;
        persistence
            .clear_memories(&self.persona_id, tier)
            .await
            .map_err(MemoryError::Persistence)
    }
}

/// 描述人格记忆访问失败。
#[derive(Debug)]
pub enum MemoryError {
    Persistence(PersistenceError),
    Stored(String),
    Unavailable,
}

impl fmt::Display for MemoryError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Persistence(source) => write!(formatter, "人格记忆操作失败：{source}"),
            Self::Stored(reason) => write!(formatter, "人格上下文无法读取：{reason}"),
            Self::Unavailable => write!(formatter, "Agent 持久化当前不可用"),
        }
    }
}

impl Error for MemoryError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Persistence(source) => Some(source),
            Self::Stored(_) | Self::Unavailable => None,
        }
    }
}
