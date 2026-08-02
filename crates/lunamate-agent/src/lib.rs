//! 提供不依赖窗口框架与平台实现的 Agent 核心能力。
//!
//! 宿主通过显式配置快照、持久化回调和窄能力对象组合运行时适配。

use std::{
    error::Error,
    fmt,
    sync::{Arc, atomic::AtomicU64},
    time::Instant,
};

use futures::future::AbortHandle;
pub use genai::{Client, ModelIden, chat::ChatOptions};
use parking_lot::{Mutex, RwLock};
use rust_i18n::t;
use tokio::runtime::Handle;
use tokio::sync::watch;

rust_i18n::i18n!("locales", fallback = "en", minify_key = true);

mod agent_context;
mod agent_coordination;
mod agent_request;
mod agent_runtime;
pub mod config;
mod logging;
pub mod media;
pub mod memory;
pub mod persistence;
mod provider;
mod session;
mod store;
pub mod stt;
pub mod tools;
mod transport;
pub mod tts;

pub use memory::AgentMemory;
pub use provider::{ScreenshotCapability, client_from_model, model_and_options_from_config};
pub use session::{
    ChatLimits, ChatMessage, ChatMessageState, ChatRole, MAX_SESSION_TEXT_BYTES,
    context_message_tokens,
};

use config::{
    AppLanguage, DEFAULT_MAX_OUTPUT_TOKENS, LlmSettings, MODEL_CONTEXT_RESERVE_TOKENS,
    PersonaConfig,
};
use media::ImageAttachment;
use session::{ChatSession, ResponseId, estimate_text_tokens};
use store::ChatSessionStore;
use tools::{AgentOutfitRequest, OutfitOption};

const AGENT_EFFECT_CHANNEL_CAPACITY: usize = 16;

/// 把人格上下文配置翻译为当前模型可用的会话限制。
pub fn chat_limits(persona: &PersonaConfig, settings: &LlmSettings) -> ChatLimits {
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

/// Agent 请求需要的用户输入与单次宿主能力。
pub struct AgentInput {
    pub text: String,
    pub image: Option<ImageAttachment>,
    pub screenshot_capability: Option<Arc<dyn ScreenshotCapability>>,
    pub outfits: Vec<OutfitOption>,
    pub outfit_revision: u64,
    pub request_revision: u64,
    pub language: AppLanguage,
}

/// 核心 Agent 请求宿主执行的非 UI 领域效果。
#[derive(Clone)]
pub enum AgentEffect {
    ChangeOutfit(AgentOutfitRequest),
}

/// Agent 提供给宿主渲染的一致只读快照。
#[derive(Clone)]
pub struct AgentSnapshot {
    revision: u64,
    active_persona: String,
    language: AppLanguage,
    messages: Vec<ChatMessage>,
    status: Option<String>,
    reply_message_id: Option<u64>,
    streaming: bool,
    switching_memory: bool,
    shutting_down: bool,
    pending_voice: Option<u64>,
}

impl AgentSnapshot {
    pub const fn revision(&self) -> u64 {
        self.revision
    }

    pub fn active_persona(&self) -> &str {
        &self.active_persona
    }

    pub const fn language(&self) -> AppLanguage {
        self.language
    }

    pub fn messages(&self) -> &[ChatMessage] {
        &self.messages
    }

    pub fn status(&self) -> Option<&str> {
        self.status.as_deref()
    }

    pub const fn reply_message_id(&self) -> Option<u64> {
        self.reply_message_id
    }

    pub const fn is_streaming(&self) -> bool {
        self.streaming
    }

    pub const fn is_switching_memory(&self) -> bool {
        self.switching_memory
    }

    pub const fn is_shutting_down(&self) -> bool {
        self.shutting_down
    }

    pub const fn pending_voice(&self) -> Option<u64> {
        self.pending_voice
    }
}

#[derive(Clone)]
struct AgentRuntime {
    revision: u64,
    configuration_revision: u64,
    client: Client,
    model: Option<ModelIden>,
    options: Option<ChatOptions>,
    system_prompt: Arc<str>,
    memory: AgentMemory,
    active_persona: String,
    limits: ChatLimits,
    language: AppLanguage,
}

struct AgentState {
    session: ChatSession,
    store: Arc<ChatSessionStore>,
    persist_revision: u64,
    last_persist: Instant,
    status: Option<String>,
    reply_message_id: Option<u64>,
    active_request: Option<ActiveRequest>,
    request_revision: u64,
    /// 已通过前置校验的最高配置 revision；失败后保留它以拒绝更旧配置，同时允许原值重试。
    pending_configuration_revision: u64,
    switching_memory: bool,
    suspended: bool,
    shutting_down: bool,
    pending_voice: Option<PendingVoice>,
}

struct ActiveRequest {
    response_id: ResponseId,
    runtime_revision: u64,
    abort: AbortHandle,
    started_at: Instant,
}

struct PendingVoice {
    utterance_id: u64,
    runtime_revision: u64,
    persona: String,
    language: AppLanguage,
}

/// 直接组合 `genai::Client`、模型、系统提示词和人格记忆的线程安全 Agent。
///
/// 可替换运行时保存在一个短持有的读写锁中，使单次请求取得一致快照。Client clone、Provider
/// 请求和持久化操作都在释放同步锁后执行。
pub struct Agent {
    // 同时访问两者时固定先锁 runtime、再锁 state，且任何异步等待前必须释放两把锁。
    runtime: RwLock<AgentRuntime>,
    state: Mutex<AgentState>,
    persistence_runtime: Option<Handle>,
    state_revision: AtomicU64,
    state_updates: watch::Sender<u64>,
    effects: async_channel::Sender<AgentEffect>,
    effect_events: async_channel::Receiver<AgentEffect>,
}

/// Agent 操作在进入 Provider 前或会话切换期间失败。
#[derive(Debug)]
pub enum AgentError {
    ModelUnavailable,
    ContextWindowExhausted,
    MemorySwitching,
    Suspended,
    StaleInput,
    ShuttingDown,
    Session(String),
    Persistence(String),
}

impl AgentError {
    pub fn localized_message(&self, language: AppLanguage) -> String {
        match self {
            Self::ModelUnavailable => {
                t!("chat.configure_model", locale = language.id()).to_string()
            }
            Self::ContextWindowExhausted => {
                t!("chat.context_window_exhausted", locale = language.id()).to_string()
            }
            Self::MemorySwitching => {
                t!("chat.persona_switching", locale = language.id()).to_string()
            }
            Self::Suspended => t!("chat.stopped", locale = language.id()).to_string(),
            Self::StaleInput => t!("chat.stopped", locale = language.id()).to_string(),
            Self::ShuttingDown => t!("chat.stopped", locale = language.id()).to_string(),
            Self::Session(message) | Self::Persistence(message) => message.clone(),
        }
    }
}

impl fmt::Display for AgentError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ModelUnavailable => write!(formatter, "Agent 模型尚未配置"),
            Self::ContextWindowExhausted => write!(formatter, "Agent 上下文窗口没有可用输入预算"),
            Self::MemorySwitching => write!(formatter, "Agent 正在切换人格记忆"),
            Self::Suspended => write!(formatter, "Agent 当前已挂起"),
            Self::StaleInput => write!(formatter, "Agent 输入已过期"),
            Self::ShuttingDown => write!(formatter, "Agent 正在关闭"),
            Self::Session(message) => write!(formatter, "Agent 会话操作失败：{message}"),
            Self::Persistence(message) => write!(formatter, "Agent 持久化失败：{message}"),
        }
    }
}

impl Error for AgentError {}

#[cfg(test)]
mod tests;
