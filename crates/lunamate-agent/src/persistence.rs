//! 定义 Agent 会话与人格记忆所需的最小异步持久化回调。

use std::{error::Error, fmt, future::Future, sync::Arc};

use futures::future::BoxFuture;

type LoadSessionCallback = dyn Fn(String) -> BoxFuture<'static, Result<Option<SessionDocument>, PersistenceError>>
    + Send
    + Sync;
type SaveSessionCallback = dyn Fn(String, SessionDocument) -> BoxFuture<'static, Result<(), PersistenceError>>
    + Send
    + Sync;
type DeleteSessionCallback =
    dyn Fn(String) -> BoxFuture<'static, Result<(), PersistenceError>> + Send + Sync;
type MemoryUsageCallback = dyn Fn(String) -> BoxFuture<'static, Result<PersistentMemoryUsage, PersistenceError>>
    + Send
    + Sync;
type ClearMemoriesCallback = dyn Fn(String, Option<PersistentMemoryTier>) -> BoxFuture<'static, Result<(), PersistenceError>>
    + Send
    + Sync;

/// 一份版本化会话文档；Agent 自己负责内容格式与大小校验。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SessionDocument {
    format_version: u32,
    contents: Vec<u8>,
}

impl SessionDocument {
    pub fn new(format_version: u32, contents: Vec<u8>) -> Self {
        Self {
            format_version,
            contents,
        }
    }

    pub fn format_version(&self) -> u32 {
        self.format_version
    }

    pub fn contents(&self) -> &[u8] {
        &self.contents
    }

    pub fn into_parts(self) -> (u32, Vec<u8>) {
        (self.format_version, self.contents)
    }
}

/// Agent 需要区分的持久化记忆层级。
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum PersistentMemoryTier {
    Medium,
    Long,
}

/// 单个人格当前持久化的中期与长期记忆条数。
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct PersistentMemoryUsage {
    medium: u64,
    long: u64,
}

impl PersistentMemoryUsage {
    pub const fn new(medium: u64, long: u64) -> Self {
        Self { medium, long }
    }

    pub const fn medium(self) -> u64 {
        self.medium
    }

    pub const fn long(self) -> u64 {
        self.long
    }
}

/// 宿主持久化回调返回的脱敏错误。
#[derive(Debug)]
pub struct PersistenceError {
    diagnostic_kind: &'static str,
    message: String,
    invalid_document: bool,
}

impl PersistenceError {
    pub fn new(diagnostic_kind: &'static str, message: impl fmt::Display) -> Self {
        Self {
            diagnostic_kind,
            message: message.to_string(),
            invalid_document: false,
        }
    }

    /// 标记宿主已经读到记录，但记录结构不足以构造一份会话文档。
    pub fn invalid_document(diagnostic_kind: &'static str, message: impl fmt::Display) -> Self {
        Self {
            diagnostic_kind,
            message: message.to_string(),
            invalid_document: true,
        }
    }

    pub const fn diagnostic_kind(&self) -> &'static str {
        self.diagnostic_kind
    }

    pub const fn is_invalid_document(&self) -> bool {
        self.invalid_document
    }
}

impl fmt::Display for PersistenceError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl Error for PersistenceError {}

/// 宿主为 Agent 注册的完整持久化能力。
///
/// 五个回调作为一个不可分割的对象注册，避免会话与记忆落到不同生命周期的后端。
#[derive(Clone)]
pub struct AgentPersistenceCallbacks {
    load_session: Arc<LoadSessionCallback>,
    save_session: Arc<SaveSessionCallback>,
    delete_session: Arc<DeleteSessionCallback>,
    memory_usage: Arc<MemoryUsageCallback>,
    clear_memories: Arc<ClearMemoriesCallback>,
}

impl AgentPersistenceCallbacks {
    pub fn new<L, LF, S, SF, D, DF, U, UF, C, CF>(
        load_session: L,
        save_session: S,
        delete_session: D,
        memory_usage: U,
        clear_memories: C,
    ) -> Self
    where
        L: Fn(String) -> LF + Send + Sync + 'static,
        LF: Future<Output = Result<Option<SessionDocument>, PersistenceError>> + Send + 'static,
        S: Fn(String, SessionDocument) -> SF + Send + Sync + 'static,
        SF: Future<Output = Result<(), PersistenceError>> + Send + 'static,
        D: Fn(String) -> DF + Send + Sync + 'static,
        DF: Future<Output = Result<(), PersistenceError>> + Send + 'static,
        U: Fn(String) -> UF + Send + Sync + 'static,
        UF: Future<Output = Result<PersistentMemoryUsage, PersistenceError>> + Send + 'static,
        C: Fn(String, Option<PersistentMemoryTier>) -> CF + Send + Sync + 'static,
        CF: Future<Output = Result<(), PersistenceError>> + Send + 'static,
    {
        Self {
            load_session: Arc::new(move |persona| Box::pin(load_session(persona))),
            save_session: Arc::new(move |persona, document| {
                Box::pin(save_session(persona, document))
            }),
            delete_session: Arc::new(move |persona| Box::pin(delete_session(persona))),
            memory_usage: Arc::new(move |persona| Box::pin(memory_usage(persona))),
            clear_memories: Arc::new(move |persona, tier| Box::pin(clear_memories(persona, tier))),
        }
    }

    pub(crate) async fn load_session(
        &self,
        persona: &str,
    ) -> Result<Option<SessionDocument>, PersistenceError> {
        (self.load_session)(persona.to_owned()).await
    }

    pub(crate) async fn save_session(
        &self,
        persona: &str,
        document: SessionDocument,
    ) -> Result<(), PersistenceError> {
        (self.save_session)(persona.to_owned(), document).await
    }

    pub(crate) async fn delete_session(&self, persona: &str) -> Result<(), PersistenceError> {
        (self.delete_session)(persona.to_owned()).await
    }

    pub(crate) async fn memory_usage(
        &self,
        persona: &str,
    ) -> Result<PersistentMemoryUsage, PersistenceError> {
        (self.memory_usage)(persona.to_owned()).await
    }

    pub(crate) async fn clear_memories(
        &self,
        persona: &str,
        tier: Option<PersistentMemoryTier>,
    ) -> Result<(), PersistenceError> {
        (self.clear_memories)(persona.to_owned(), tier).await
    }
}
