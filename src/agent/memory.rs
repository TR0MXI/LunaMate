//! 定义人格记忆的分层模型，并提供设置界面需要的用量统计与清除入口。
//!
//! 记忆分三层，生命周期各不相同：
//!
//! - 短期：当前对话上下文。运行时驻留内存，由 [`super::store::ChatSessionStore`]
//!   在关闭程序时持久化到本人格独占的会话文档。
//! - 中期：由后台模型从对话中提取的条目化记忆，写入 `agent_memory` 的 `medium` 层。
//! - 长期：供 RAG 检索的记忆，写入 `agent_memory` 的 `long` 层。
//!
//! 中期与长期记忆的提取、合并、过期与召回尚未实现，本模块只固定数据结构与
//! 删除路径；[`MediumTermMemory`] 与 [`LongTermMemory`] 的字段与数据库模式一一对应，
//! 后续实现写入时不需要再改动人格与设置界面的接口。

use std::{
    error::Error,
    fmt,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
};

use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::database::{Database, DatabaseError, MemoryTier, MemoryUsage};

/// 记忆条目的语义分类，与 `agent_memory.kind` 的取值一一对应。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[expect(dead_code, reason = "中期与长期记忆的写入路径尚未实现，先固定分类取值")]
pub(super) enum MemoryKind {
    /// 具体发生过的事件。
    Episodic,
    /// 关于用户或世界的稳定事实。
    Semantic,
    /// 交互方式与偏好的做法性知识。
    Procedural,
    /// 用户画像类的长期属性。
    Profile,
}

/// 后台模型从对话中提取的一条中期记忆。
///
/// 字段对应 `agent_memory` 中 `tier = "medium"` 的记录；`importance` 与 `confidence`
/// 用于后续的合并与淘汰策略，`expires_at` 用于到期清理。
#[derive(Clone, Debug)]
#[expect(dead_code, reason = "预留结构：中期记忆提取尚未实现")]
pub(super) struct MediumTermMemory {
    pub(super) agent_id: String,
    pub(super) kind: MemoryKind,
    pub(super) content: String,
    pub(super) summary: Option<String>,
    pub(super) tags: Vec<String>,
    pub(super) importance: f32,
    pub(super) confidence: f32,
    pub(super) source: Option<String>,
    pub(super) expires_at: Option<i64>,
}

/// 供 RAG 检索的一条长期记忆。
///
/// `embedding` 的维度取决于后续选定的 embedding 模型，因此数据库首版只约束字段类型，
/// 不创建固定维度的向量索引。
#[derive(Clone, Debug)]
#[expect(dead_code, reason = "预留结构：长期记忆检索尚未实现")]
pub(super) struct LongTermMemory {
    pub(super) agent_id: String,
    pub(super) kind: MemoryKind,
    pub(super) content: String,
    pub(super) summary: Option<String>,
    pub(super) embedding: Option<Vec<f32>>,
    pub(super) tags: Vec<String>,
    pub(super) importance: f32,
}

/// 短期上下文的占用量与生效上限。
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) struct ContextUsage {
    pub(crate) messages: usize,
    pub(crate) max_messages: usize,
    pub(crate) tokens: usize,
    pub(crate) max_tokens: usize,
}

/// 设置页编辑当前上下文时使用的稳定角色。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ContextMessageRole {
    User,
    Assistant,
}

/// 一次本地工具执行中可安全展示的 Provider 无关记录。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub(crate) struct ToolExecutionTrace {
    name: String,
    arguments: Value,
    result: Value,
}

impl ToolExecutionTrace {
    pub(super) fn new(name: String, arguments: Value, result: Value) -> Self {
        Self {
            name,
            arguments,
            result,
        }
    }

    /// 返回稳定工具名，不包含 Provider call ID。
    pub(crate) fn name(&self) -> &str {
        &self.name
    }

    /// 返回模型提交给本地工具的 JSON 参数。
    pub(crate) fn arguments(&self) -> &Value {
        &self.arguments
    }

    /// 返回权限复核后生成的脱敏 JSON 结果。
    pub(crate) fn result(&self) -> &Value {
        &self.result
    }
}

/// 助手消息附带的可展示推理与本地工具执行详情。
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub(crate) struct AssistantTrace {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    reasoning: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    tool_executions: Vec<ToolExecutionTrace>,
}

impl AssistantTrace {
    pub(super) fn new(reasoning: Option<String>, tool_executions: Vec<ToolExecutionTrace>) -> Self {
        Self {
            reasoning,
            tool_executions,
        }
    }

    /// 返回 Provider 提供的可读推理文本；协议签名不会进入此字段。
    pub(crate) fn reasoning(&self) -> Option<&str> {
        self.reasoning.as_deref()
    }

    /// 按实际执行顺序返回本地工具记录。
    pub(crate) fn tool_executions(&self) -> &[ToolExecutionTrace] {
        &self.tool_executions
    }

    pub(super) fn is_empty(&self) -> bool {
        self.reasoning
            .as_ref()
            .is_none_or(|reasoning| reasoning.trim().is_empty())
            && self.tool_executions.is_empty()
    }
}

/// 不含内部状态、图片内容和 Provider 类型的可编辑上下文消息。
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ContextMessage {
    pub(crate) id: u64,
    pub(crate) role: ContextMessageRole,
    pub(crate) content: String,
    pub(crate) tokens: usize,
    pub(crate) fixed_tokens: usize,
    /// 仅供展示；编辑正文不会把该数据回放给 Provider。
    pub(crate) trace: Option<AssistantTrace>,
}

/// 人格设置界面展示的三层记忆用量。
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) struct PersonaMemoryUsage {
    pub(crate) context: ContextUsage,
    pub(crate) medium: u64,
    pub(crate) long: u64,
}

/// 当前人格的最新上下文占用。
///
/// 短期上下文运行时只存在于持有会话的视图里，设置界面无法从数据库读到未落盘的
/// 增量，因此这里使用只保留最新值的共享状态：视图在每次提交快照时发布，界面按需读取。
#[derive(Clone, Default)]
pub(crate) struct LiveContextUsage {
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
    pub(super) fn publish(
        &self,
        persona_id: &str,
        usage: ContextUsage,
        messages: Vec<ContextMessage>,
    ) {
        *self.latest.lock() = Some(LiveContextSnapshot {
            persona_id: persona_id.to_owned(),
            usage,
            messages,
        });
        self.revision.fetch_add(1, Ordering::Release);
    }

    /// 返回指定人格的实时占用；该人格当前未被加载时返回 `None`。
    pub(crate) fn get(&self, persona_id: &str) -> Option<ContextUsage> {
        self.latest
            .lock()
            .as_ref()
            .filter(|snapshot| snapshot.persona_id == persona_id)
            .map(|snapshot| snapshot.usage)
    }

    /// 返回指定人格实时上下文最近一次发布的 revision；非活动人格返回 `None`。
    pub(crate) fn revision_for(&self, persona_id: &str) -> Option<u64> {
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

/// 需要清除的记忆范围。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[allow(dead_code)] // 中期与长期页面暂时留空，但存储层仍需区分清理范围。
pub(crate) enum MemoryScope {
    /// 当前对话上下文。
    Context,
    /// 中期记忆。
    Medium,
    /// 长期记忆。
    Long,
    /// 该人格的全部记忆。
    All,
}

impl MemoryScope {
    /// 返回日志和持久化操作使用的稳定范围标识。
    pub(crate) const fn id(self) -> &'static str {
        match self {
            Self::Context => "context",
            Self::Medium => "medium",
            Self::Long => "long",
            Self::All => "all",
        }
    }
}

/// 绑定到单个人格的记忆存储句柄。
///
/// 数据库初始化失败时句柄仍然可用，但所有操作都会返回 [`MemoryError::Unavailable`]，
/// 而不是伪装成"没有记忆"。
#[derive(Clone)]
pub(crate) struct PersonaMemory {
    database: Option<Arc<Database>>,
    session_document_lock: super::store::SessionDocumentLock,
    persona_id: String,
}

impl PersonaMemory {
    pub(super) fn new(
        database: Option<Arc<Database>>,
        session_document_lock: super::store::SessionDocumentLock,
        persona_id: impl Into<String>,
    ) -> Self {
        Self {
            database,
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
    /// 数据库不可用或查询失败时返回错误。
    pub(crate) async fn usage(
        &self,
        live: LiveContextUsage,
        limits: ContextUsage,
    ) -> Result<PersonaMemoryUsage, MemoryError> {
        let database = self.database.as_ref().ok_or(MemoryError::Unavailable)?;
        let memory: MemoryUsage = database
            .agent_memory_usage(&self.persona_id)
            .await
            .map_err(MemoryError::Database)?;
        let context = match live.get(&self.persona_id) {
            Some(usage) => usage,
            None => super::store::persona_context_usage(
                database,
                &self.session_document_lock,
                &self.persona_id,
                limits,
            )
            .await
            .map_err(|error| MemoryError::Stored(error.to_string()))?,
        };
        Ok(PersonaMemoryUsage {
            context,
            medium: memory.medium,
            long: memory.long,
        })
    }

    /// 返回设置页可编辑的当前上下文；活动人格优先使用内存快照。
    pub(crate) async fn context_messages(
        &self,
        live: LiveContextUsage,
        limits: ContextUsage,
    ) -> Result<Vec<ContextMessage>, MemoryError> {
        if let Some(messages) = live.messages(&self.persona_id) {
            return Ok(messages);
        }
        let database = self.database.as_ref().ok_or(MemoryError::Unavailable)?;
        super::store::persona_context_messages(
            database,
            &self.session_document_lock,
            &self.persona_id,
            limits,
        )
        .await
        .map_err(|error| MemoryError::Stored(error.to_string()))
    }

    /// 删除该人格在数据库中的记忆；短期上下文由会话存储单独清除。
    ///
    /// # Errors
    ///
    /// 数据库不可用或删除失败时返回错误。
    pub(crate) async fn clear(&self, scope: MemoryScope) -> Result<(), MemoryError> {
        let tier = match scope {
            MemoryScope::Context => return Ok(()),
            MemoryScope::Medium => Some(MemoryTier::Medium),
            MemoryScope::Long => Some(MemoryTier::Long),
            MemoryScope::All => None,
        };
        let database = self.database.as_ref().ok_or(MemoryError::Unavailable)?;
        database
            .delete_agent_memories(&self.persona_id, tier)
            .await
            .map_err(MemoryError::Database)
    }
}

/// 描述人格记忆访问失败。
#[derive(Debug)]
pub(crate) enum MemoryError {
    Database(DatabaseError),
    Stored(String),
    Unavailable,
}

impl fmt::Display for MemoryError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Database(source) => write!(formatter, "人格记忆操作失败：{source}"),
            Self::Stored(reason) => write!(formatter, "人格上下文无法读取：{reason}"),
            Self::Unavailable => write!(formatter, "嵌入式数据库当前不可用"),
        }
    }
}

impl Error for MemoryError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Database(source) => Some(source),
            Self::Stored(_) | Self::Unavailable => None,
        }
    }
}
