//! 管理单个有界会话，并用请求标识隔离取消、替换和迟到的流式结果。

use std::{collections::VecDeque, error::Error, fmt};

use rust_i18n::t;
use serde::{Deserialize, Serialize};

use crate::{
    config::{AppLanguage, DEFAULT_CONTEXT_MESSAGES, DEFAULT_CONTEXT_TOKENS},
    media::ImageAttachment,
    memory::AssistantTrace,
};

mod conversation;
mod editing;
mod snapshot;
mod tokens;

pub use tokens::context_message_tokens;
pub(super) use tokens::estimate_text_tokens;

const SNAPSHOT_VERSION: u32 = 1;
const MAX_SESSION_IMAGE_BYTES: usize = 16 * 1024 * 1024;
pub const MAX_SESSION_TEXT_BYTES: usize = 4 * 1024 * 1024;
pub(super) const MAX_TRACE_REASONING_BYTES: usize = 64 * 1024;
pub(super) const MAX_TRACE_TOOL_NAME_BYTES: usize = 256;
pub(super) const MAX_TRACE_JSON_BYTES: usize = 64 * 1024;
pub(super) const MAX_MESSAGE_TRACE_BYTES: usize = 256 * 1024;
pub(super) const MAX_MESSAGE_TOOL_EXECUTIONS: usize = 4;
pub(super) const MAX_SESSION_TRACE_BYTES: usize = 1024 * 1024;
pub(super) const MAX_SESSION_TRACE_MESSAGES: usize = 64;
pub(super) const MAX_SESSION_TOOL_EXECUTIONS: usize = 64;
const TOKENS_PER_MESSAGE: usize = 4;
const IMAGE_CONTEXT_TOKENS: usize = 1_024;
const MISSING_IMAGE_CONTEXT_TOKENS: usize = 64;

pub(super) fn voice_interruption_marker(language: AppLanguage) -> String {
    format!(
        "\n\n{}",
        t!("chat.voice_interruption_marker", locale = language.id())
    )
}

/// LunaMate 对话记录中允许持久化的消息角色。
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum ChatRole {
    User,
    Assistant,
}

/// 消息在当前会话中的可见状态。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum ChatMessageState {
    Complete,
    Streaming,
    Failed(String),
    Cancelled,
    Interrupted,
    InterruptedByVoice,
}

/// 单个用户轮次中的一条聊天消息。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ChatMessage {
    id: u64,
    turn_id: u64,
    role: ChatRole,
    content: String,
    #[serde(deserialize_with = "Option::deserialize")]
    image: Option<ImageAttachment>,
    #[serde(deserialize_with = "Option::deserialize")]
    trace: Option<AssistantTrace>,
    state: ChatMessageState,
}

impl ChatMessage {
    /// 返回消息的稳定运行时 ID。
    pub const fn id(&self) -> u64 {
        self.id
    }

    /// 返回消息角色。
    pub const fn role(&self) -> ChatRole {
        self.role
    }

    /// 返回消息正文。
    pub fn content(&self) -> &str {
        &self.content
    }

    /// 返回用于界面展示的正文；模型上下文标注不会写入消息本体。
    pub fn visible_content(&self) -> &str {
        &self.content
    }

    /// 返回图片元数据与当前进程内仍可用的内容。
    pub fn image(&self) -> Option<&ImageAttachment> {
        self.image.as_ref()
    }

    /// 返回消息终态或流式状态。
    pub fn state(&self) -> &ChatMessageState {
        &self.state
    }

    /// 返回与本消息共同移动和持久化的助手详情。
    pub fn trace(&self) -> Option<&AssistantTrace> {
        self.trace.as_ref()
    }
}

/// 发送给 Provider 的上下文消息；图片内容只在当前进程内保留。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ChatContextMessage {
    pub source_message_id: Option<u64>,
    pub role: ChatRole,
    pub content: String,
    pub image: Option<ImageAttachment>,
}

/// 限制当前上下文的消息数量和估算 token 数。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ChatLimits {
    pub max_messages: usize,
    /// 当前会话在内存和快照中保留的 token 上限。
    pub max_tokens: usize,
    /// 单次请求可发送给 Provider 的历史输入预算。
    pub max_request_tokens: usize,
}

impl Default for ChatLimits {
    fn default() -> Self {
        Self {
            max_messages: DEFAULT_CONTEXT_MESSAGES as usize,
            max_tokens: DEFAULT_CONTEXT_TOKENS as usize,
            max_request_tokens: DEFAULT_CONTEXT_TOKENS as usize,
        }
    }
}

/// 标识一次流式响应，用于拒绝已取消或被新请求替换的结果。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct ResponseId(u64);

impl ResponseId {
    /// 返回仅在当前会话进程内有效的关联编号。
    pub(super) const fn get(self) -> u64 {
        self.0
    }
}

#[derive(Clone, Copy, Debug)]
struct ActiveResponse {
    id: ResponseId,
    turn_id: u64,
    message_id: u64,
}

#[derive(Clone, Copy, Debug, Default)]
struct TraceUsage {
    bytes: usize,
    messages: usize,
    tool_executions: usize,
}

impl TraceUsage {
    fn checked_add(self, additional: Self) -> Option<Self> {
        let usage = Self {
            bytes: self.bytes.checked_add(additional.bytes)?,
            messages: self.messages.checked_add(additional.messages)?,
            tool_executions: self
                .tool_executions
                .checked_add(additional.tool_executions)?,
        };
        (usage.bytes <= MAX_SESSION_TRACE_BYTES
            && usage.messages <= MAX_SESSION_TRACE_MESSAGES
            && usage.tool_executions <= MAX_SESSION_TOOL_EXECUTIONS)
            .then_some(usage)
    }

    fn subtract(&mut self, removed: Self) {
        self.bytes = self.bytes.saturating_sub(removed.bytes);
        self.messages = self.messages.saturating_sub(removed.messages);
        self.tool_executions = self.tool_executions.saturating_sub(removed.tool_executions);
    }
}

/// 原子创建一个用户轮次后交给网络层的结果。
pub struct StartedTurn {
    pub response_id: ResponseId,
    pub context: Vec<ChatContextMessage>,
}

/// 写入磁盘的版本化单会话快照；不包含活动网络请求或任何凭据。
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct ChatSessionSnapshot {
    version: u32,
    pub(super) revision: u64,
    pub(super) messages: Vec<ChatMessage>,
}

/// 保存单个有界会话，并跟踪至多一个活动流式响应。
pub struct ChatSession {
    messages: VecDeque<ChatMessage>,
    limits: ChatLimits,
    total_tokens: usize,
    total_image_bytes: usize,
    trace_usage: TraceUsage,
    next_message_id: u64,
    next_turn_id: u64,
    next_response_id: u64,
    active_response: Option<ActiveResponse>,
    voice_interruption_pending: bool,
}

impl Default for ChatSession {
    fn default() -> Self {
        Self::new(ChatLimits::default()).expect("默认聊天限制必须容纳一轮非空对话")
    }
}

/// 描述会话状态更新被拒绝的原因。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ChatError {
    InvalidLimits,
    EmptyMessage,
    Busy,
    MessageTooLarge,
    StaleResponse,
    MissingResponse,
    MissingMessage,
    UnsupportedSnapshot,
    InvalidSnapshot,
}

impl ChatError {
    /// 返回绑定到单次宿主操作语言的用户可见说明。
    pub fn localized_message(self, language: AppLanguage) -> String {
        match self {
            Self::InvalidLimits => t!("chat.error.invalid_limits", locale = language.id()),
            Self::EmptyMessage => t!("chat.error.empty_message", locale = language.id()),
            Self::Busy => t!("chat.error.busy", locale = language.id()),
            Self::MessageTooLarge => {
                t!("chat.error.message_too_large", locale = language.id())
            }
            Self::StaleResponse => t!("chat.error.stale_response", locale = language.id()),
            Self::MissingResponse => t!("chat.error.missing_response", locale = language.id()),
            Self::MissingMessage => t!("chat.error.missing_message", locale = language.id()),
            Self::UnsupportedSnapshot => {
                t!("chat.error.unsupported_snapshot", locale = language.id())
            }
            Self::InvalidSnapshot => t!("chat.error.invalid_snapshot", locale = language.id()),
        }
        .to_string()
    }
}

impl fmt::Display for ChatError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let message = match self {
            Self::InvalidLimits => "invalid chat limits",
            Self::EmptyMessage => "empty chat message",
            Self::Busy => "chat response is active",
            Self::MessageTooLarge => "chat message exceeds limits",
            Self::StaleResponse => "stale chat response",
            Self::MissingResponse => "missing assistant response",
            Self::MissingMessage => "missing chat message",
            Self::UnsupportedSnapshot => "unsupported chat snapshot version",
            Self::InvalidSnapshot => "invalid chat snapshot",
        };
        formatter.write_str(message)
    }
}

impl Error for ChatError {}
