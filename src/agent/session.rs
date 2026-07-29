//! 管理单个有界会话，并用请求标识隔离取消、替换和迟到的流式结果。

use std::{
    collections::{HashSet, VecDeque},
    error::Error,
    fmt,
};

use rust_i18n::t;
use serde::{Deserialize, Serialize};

use crate::config::{DEFAULT_CONTEXT_MESSAGES, DEFAULT_CONTEXT_TOKENS};

use super::{
    media::ImageAttachment,
    memory::{AssistantTrace, ContextMessage, ContextMessageRole, ContextUsage},
};

const SNAPSHOT_VERSION: u32 = 1;
const MAX_SESSION_IMAGE_BYTES: usize = 16 * 1024 * 1024;
pub(super) const MAX_SESSION_TEXT_BYTES: usize = 4 * 1024 * 1024;
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
pub(super) const VOICE_INTERRUPTION_MARKER: &str = "\n\n[系统标注：此回复因用户开始说话而被截断。]";

/// 对话记录中一条消息的角色。
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub(super) enum ChatRole {
    User,
    Assistant,
}

/// 消息在当前会话中的可见状态。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub(super) enum ChatMessageState {
    Complete,
    Streaming,
    Failed(String),
    Cancelled,
    Interrupted,
    InterruptedByVoice,
}

/// 单个用户轮次中的一条聊天消息。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub(super) struct ChatMessage {
    id: u64,
    turn_id: u64,
    role: ChatRole,
    content: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    image: Option<ImageAttachment>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    trace: Option<AssistantTrace>,
    state: ChatMessageState,
}

impl ChatMessage {
    /// 返回消息的稳定运行时 ID。
    pub(super) fn id(&self) -> u64 {
        self.id
    }

    /// 返回消息角色。
    pub(super) fn role(&self) -> ChatRole {
        self.role
    }

    /// 返回消息正文。
    pub(super) fn content(&self) -> &str {
        &self.content
    }

    /// 返回去除内部上下文标注后的正文，避免把协议文本直接展示给用户。
    pub(super) fn visible_content(&self) -> &str {
        if matches!(self.state, ChatMessageState::InterruptedByVoice) {
            self.content
                .strip_suffix(VOICE_INTERRUPTION_MARKER)
                .unwrap_or(&self.content)
        } else {
            &self.content
        }
    }

    /// 返回测试需要核对的图片元数据与进程内内容。
    #[cfg(test)]
    pub(super) fn image(&self) -> Option<&ImageAttachment> {
        self.image.as_ref()
    }

    /// 返回消息终态或流式状态。
    pub(super) fn state(&self) -> &ChatMessageState {
        &self.state
    }

    /// 返回与本消息共同移动和持久化的助手详情。
    #[cfg(test)]
    pub(super) fn trace(&self) -> Option<&AssistantTrace> {
        self.trace.as_ref()
    }
}

/// 发送给 Provider 的上下文消息；图片内容只在当前进程内保留。
#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct ChatContextMessage {
    pub(super) source_message_id: Option<u64>,
    pub(super) role: ChatRole,
    pub(super) content: String,
    pub(super) image: Option<ImageAttachment>,
}

/// 限制当前上下文的消息数量和估算 token 数。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct ChatLimits {
    pub(super) max_messages: usize,
    /// 当前会话在内存和快照中保留的 token 上限。
    pub(super) max_tokens: usize,
    /// 单次请求可发送给 Provider 的历史输入预算。
    pub(super) max_request_tokens: usize,
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
pub(super) struct StartedTurn {
    pub(super) response_id: ResponseId,
    pub(super) context: Vec<ChatContextMessage>,
}

/// 写入磁盘的版本化单会话快照；不包含活动网络请求或任何凭据。
#[derive(Clone, Debug, Deserialize, Serialize)]
pub(super) struct ChatSessionSnapshot {
    version: u32,
    pub(super) revision: u64,
    pub(super) messages: Vec<ChatMessage>,
}

/// 保存单个有界会话，并跟踪至多一个活动流式响应。
pub(super) struct ChatSession {
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

impl ChatSession {
    /// 使用给定限制创建空会话。
    ///
    /// # Errors
    ///
    /// 消息上限不足以容纳一轮对话时返回错误；零 token 预算允许恢复为空会话，
    /// 但发送新消息会返回 [`ChatError::MessageTooLarge`]。
    pub(super) fn new(limits: ChatLimits) -> Result<Self, ChatError> {
        if limits.max_messages < 2 {
            return Err(ChatError::InvalidLimits);
        }
        Ok(Self {
            messages: VecDeque::new(),
            limits,
            total_tokens: 0,
            total_image_bytes: 0,
            trace_usage: TraceUsage::default(),
            next_message_id: 0,
            next_turn_id: 0,
            next_response_id: 0,
            active_response: None,
            voice_interruption_pending: false,
        })
    }

    /// 返回用于界面展示的全部有界消息。
    pub(super) fn messages(&self) -> &VecDeque<ChatMessage> {
        &self.messages
    }

    /// 返回当前短期上下文占用与生效上限，供人格设置界面展示。
    pub(super) fn usage(&self) -> ContextUsage {
        ContextUsage {
            messages: self.messages.len(),
            max_messages: self.limits.max_messages,
            tokens: self.total_tokens,
            max_tokens: self.limits.max_tokens,
        }
    }

    /// 更新会话与请求预算，并按新上限淘汰最早的非活动轮次。
    pub(super) fn update_limits(&mut self, limits: ChatLimits) -> Result<(), ChatError> {
        if limits.max_messages < 2 {
            return Err(ChatError::InvalidLimits);
        }
        self.limits = limits;
        self.trim_completed_turns_for(0, 0, 0);
        Ok(())
    }

    /// 创建设置页可以安全展示和编辑的消息快照。
    pub(super) fn editable_messages(&self) -> Vec<ContextMessage> {
        self.messages
            .iter()
            .map(|message| {
                let image_tokens = image_context_tokens(message.image.as_ref());
                ContextMessage {
                    id: message.id,
                    role: match message.role {
                        ChatRole::User => ContextMessageRole::User,
                        ChatRole::Assistant => ContextMessageRole::Assistant,
                    },
                    content: message.visible_content().to_owned(),
                    tokens: message_token_count(&message.content, image_tokens),
                    fixed_tokens: TOKENS_PER_MESSAGE.saturating_add(image_tokens),
                    trace: message.trace.clone(),
                }
            })
            .collect()
    }

    /// 清空短期上下文；调用方必须已经取消并中断活动请求。
    pub(super) fn clear(&mut self) {
        self.interrupt_active_response();
        self.messages.clear();
        self.total_tokens = 0;
        self.total_image_bytes = 0;
        self.trace_usage = TraceUsage::default();
        self.active_response = None;
        self.voice_interruption_pending = false;
    }

    /// 返回当前活动响应 ID。
    pub(super) fn active_response_id(&self) -> Option<ResponseId> {
        self.active_response.map(|active| active.id)
    }

    /// 原子写入用户消息、创建助手占位并生成不含占位消息的请求上下文。
    ///
    /// # Errors
    ///
    /// 当前已有请求、消息为空或单条消息超过本地限制时返回错误。
    #[cfg(test)]
    pub(super) fn start_turn(
        &mut self,
        content: impl Into<String>,
    ) -> Result<StartedTurn, ChatError> {
        self.start_turn_with_image(content, None)
    }

    /// 创建可选附图的用户轮次；图片像素只驻留于有界内存，不进入数据库快照。
    pub(super) fn start_turn_with_image(
        &mut self,
        content: impl Into<String>,
        image: Option<ImageAttachment>,
    ) -> Result<StartedTurn, ChatError> {
        if self.active_response.is_some() {
            return Err(ChatError::Busy);
        }
        let content = content.into();
        let content = content.trim();
        let content = if content.is_empty() {
            if image.is_none() {
                return Err(ChatError::EmptyMessage);
            }
            t!("chat.image_only_prompt").to_string()
        } else {
            content.to_owned()
        };
        if content.len() > MAX_SESSION_TEXT_BYTES {
            return Err(ChatError::MessageTooLarge);
        }
        let image_bytes = image.as_ref().map_or(0, ImageAttachment::byte_len);
        if image_bytes > MAX_SESSION_IMAGE_BYTES {
            return Err(ChatError::MessageTooLarge);
        }
        let interrupted_message_id = if self.voice_interruption_pending {
            self.messages
                .iter()
                .rev()
                .find(|message| message.state == ChatMessageState::InterruptedByVoice)
                .map(|message| message.id)
        } else {
            None
        };

        let image_tokens = image_context_tokens(image.as_ref());
        let request_image_tokens = request_image_context_tokens(image.as_ref());
        let request_content = if self.voice_interruption_pending {
            format!("{VOICE_INTERRUPTION_MARKER}\n\n{content}")
        } else {
            content.clone()
        };
        if message_token_count(&request_content, request_image_tokens)
            > self.limits.max_request_tokens
        {
            return Err(ChatError::MessageTooLarge);
        }
        let user_tokens = message_token_count(&content, image_tokens);
        let assistant_tokens = message_token_count("", 0);
        let turn_tokens = user_tokens.saturating_add(assistant_tokens);
        if turn_tokens > self.limits.max_tokens {
            return Err(ChatError::MessageTooLarge);
        }
        self.trim_completed_turns_for(2, turn_tokens, image_bytes);
        if self.messages.len().saturating_add(2) > self.limits.max_messages
            || self.total_tokens.saturating_add(turn_tokens) > self.limits.max_tokens
            || self.total_image_bytes.saturating_add(image_bytes) > MAX_SESSION_IMAGE_BYTES
        {
            return Err(ChatError::MessageTooLarge);
        }

        let turn_id = allocate(&mut self.next_turn_id);
        let response_id = ResponseId(allocate(&mut self.next_response_id));
        let user_id = allocate(&mut self.next_message_id);
        let assistant_id = allocate(&mut self.next_message_id);
        self.total_tokens = self.total_tokens.saturating_add(turn_tokens);
        self.total_image_bytes += image_bytes;
        self.messages.push_back(ChatMessage {
            id: user_id,
            turn_id,
            role: ChatRole::User,
            content,
            image,
            trace: None,
            state: ChatMessageState::Complete,
        });
        self.messages.push_back(ChatMessage {
            id: assistant_id,
            turn_id,
            role: ChatRole::Assistant,
            content: String::new(),
            image: None,
            trace: None,
            state: ChatMessageState::Streaming,
        });
        self.active_response = Some(ActiveResponse {
            id: response_id,
            turn_id,
            message_id: assistant_id,
        });

        let mut context = self.context_messages();
        trim_request_context(&mut context, self.limits.max_request_tokens);
        if self.voice_interruption_pending {
            let marker_retained = interrupted_message_id.is_some_and(|message_id| {
                context.iter().any(|message| {
                    message.source_message_id == Some(message_id)
                        && message.role == ChatRole::Assistant
                        && message.content.ends_with(VOICE_INTERRUPTION_MARKER)
                })
            });
            if !marker_retained
                && let Some(user) = context
                    .iter_mut()
                    .rev()
                    .find(|message| message.role == ChatRole::User)
            {
                user.content = format!("{VOICE_INTERRUPTION_MARKER}\n\n{}", user.content);
            }
            self.voice_interruption_pending = false;
        }
        trim_request_context(&mut context, self.limits.max_request_tokens);

        Ok(StartedTurn {
            response_id,
            context,
        })
    }

    /// 将增量文本追加到当前助手消息。
    ///
    /// # Errors
    ///
    /// 响应已过期、目标消息缺失或响应超过会话 token 上限时返回错误。
    pub(super) fn append_response(
        &mut self,
        response_id: ResponseId,
        chunk: &str,
    ) -> Result<(), ChatError> {
        let active = self.current_response(response_id)?;
        let message_count = self.messages.len();
        let assistant = self.messages.back().filter(|message| {
            message.id == active.message_id
                && message.turn_id == active.turn_id
                && message.role == ChatRole::Assistant
                && message.state == ChatMessageState::Streaming
        });
        let user = message_count
            .checked_sub(2)
            .and_then(|index| self.messages.get(index))
            .filter(|message| message.turn_id == active.turn_id && message.role == ChatRole::User);
        let current_len = assistant
            .map(|message| message.content.len())
            .ok_or(ChatError::MissingResponse)?;
        let new_len = current_len
            .checked_add(chunk.len())
            .ok_or(ChatError::MessageTooLarge)?;
        if new_len > MAX_SESSION_TEXT_BYTES {
            return Err(ChatError::MessageTooLarge);
        }
        let current_tokens = message_token_count(
            assistant
                .map(|message| message.content.as_str())
                .unwrap_or_default(),
            0,
        );
        let new_tokens = message_token_count(
            &assistant
                .map(|message| format!("{}{}", message.content, chunk))
                .unwrap_or_default(),
            0,
        );
        let additional_tokens = new_tokens.saturating_sub(current_tokens);
        let active_turn_tokens = user
            .map(|message| {
                message_token_count(
                    &message.content,
                    image_context_tokens(message.image.as_ref()),
                )
            })
            .unwrap_or_default()
            .saturating_add(new_tokens);
        if active_turn_tokens > self.limits.max_tokens {
            return Err(ChatError::MessageTooLarge);
        }
        self.trim_completed_turns_for(0, additional_tokens, 0);
        if self.total_tokens.saturating_add(additional_tokens) > self.limits.max_tokens {
            return Err(ChatError::MessageTooLarge);
        }
        let message = self
            .messages
            .back_mut()
            .filter(|message| {
                message.id == active.message_id
                    && message.turn_id == active.turn_id
                    && message.role == ChatRole::Assistant
                    && message.state == ChatMessageState::Streaming
            })
            .ok_or(ChatError::MissingResponse)?;
        message.content.push_str(chunk);
        self.total_tokens = self.total_tokens.saturating_add(additional_tokens);
        Ok(())
    }

    /// 把可选详情附加到当前响应对应的助手占位，不允许迟到事件改写替代请求。
    ///
    /// # Errors
    ///
    /// 响应已过期、目标消息不再流式，或详情超过固定安全上限时返回错误。
    pub(super) fn attach_response_trace(
        &mut self,
        response_id: ResponseId,
        trace: AssistantTrace,
    ) -> Result<bool, ChatError> {
        let active = self.current_response(response_id)?;
        if trace.is_empty() {
            return Ok(false);
        }
        let trace_usage = validated_trace_usage(&trace).ok_or(ChatError::MessageTooLarge)?;
        let next_usage = self
            .trace_usage
            .checked_add(trace_usage)
            .ok_or(ChatError::MessageTooLarge)?;
        let message = self
            .messages
            .iter_mut()
            .find(|message| {
                message.id == active.message_id
                    && message.turn_id == active.turn_id
                    && message.role == ChatRole::Assistant
                    && message.state == ChatMessageState::Streaming
                    && message.trace.is_none()
            })
            .ok_or(ChatError::MissingResponse)?;
        message.trace = Some(trace);
        self.trace_usage = next_usage;
        Ok(true)
    }

    /// 完成匹配的流式响应；迟到完成不会影响新请求。
    pub(super) fn finish_response(&mut self, response_id: ResponseId) -> bool {
        self.set_response_state(response_id, ChatMessageState::Complete)
    }

    /// 将匹配响应标记为失败并保留已经收到的部分文本。
    pub(super) fn fail_response(&mut self, response_id: ResponseId, message: String) -> bool {
        self.set_response_state(response_id, ChatMessageState::Failed(message))
    }

    /// 仅取消匹配的响应，旧任务不能取消后续新请求。
    pub(super) fn cancel_response(&mut self, response_id: ResponseId) -> bool {
        self.set_response_state(response_id, ChatMessageState::Cancelled)
    }

    /// 截断匹配的回复、追加稳定标注并保留该轮供下一次模型请求理解打断语义。
    pub(super) fn interrupt_response_by_voice(&mut self, response_id: ResponseId) -> bool {
        let Ok(active) = self.current_response(response_id) else {
            return false;
        };
        let Some(index) = self.messages.iter().position(|message| {
            message.id == active.message_id
                && message.turn_id == active.turn_id
                && message.role == ChatRole::Assistant
                && message.state == ChatMessageState::Streaming
        }) else {
            self.active_response = None;
            return false;
        };
        let current_tokens = message_token_count(&self.messages[index].content, 0);
        let tokens_without_response = self.total_tokens.saturating_sub(current_tokens);
        let available = self
            .limits
            .max_tokens
            .saturating_sub(tokens_without_response);
        if message_token_count(VOICE_INTERRUPTION_MARKER, 0) > available {
            // 即使 token 预算容不下标注，也持久化独立终态；恢复后可据此向下一轮注入语义。
            let interrupted =
                self.set_response_state(response_id, ChatMessageState::InterruptedByVoice);
            self.voice_interruption_pending = interrupted;
            return interrupted;
        }
        let content = truncated_with_suffix(
            &self.messages[index].content,
            VOICE_INTERRUPTION_MARKER,
            available,
        );
        let message = &mut self.messages[index];
        message.content = content;
        message.state = ChatMessageState::InterruptedByVoice;
        self.total_tokens =
            tokens_without_response.saturating_add(message_token_count(&message.content, 0));
        self.active_response = None;
        self.voice_interruption_pending = true;
        true
    }

    /// 将退出时仍活动的响应标记为中断，且不允许迟到事件继续写入。
    pub(super) fn interrupt_active_response(&mut self) {
        if let Some(active) = self.active_response {
            self.set_response_state(active.id, ChatMessageState::Interrupted);
        }
    }

    /// 修改一条非活动消息；编辑后的失败或中断消息恢复为可发送的完整消息。
    pub(super) fn edit_message(&mut self, message_id: u64, content: &str) -> Result<(), ChatError> {
        let content = content.trim();
        if content.is_empty() {
            return Err(ChatError::EmptyMessage);
        }
        if content.len() > MAX_SESSION_TEXT_BYTES {
            return Err(ChatError::MessageTooLarge);
        }
        let index = self
            .messages
            .iter()
            .position(|message| message.id == message_id)
            .ok_or(ChatError::MissingMessage)?;
        if self
            .active_response
            .is_some_and(|active| active.turn_id == self.messages[index].turn_id)
        {
            return Err(ChatError::Busy);
        }
        let old_tokens = message_token_count(
            &self.messages[index].content,
            image_context_tokens(self.messages[index].image.as_ref()),
        );
        let new_tokens = message_token_count(
            content,
            image_context_tokens(self.messages[index].image.as_ref()),
        );
        let next_total = self
            .total_tokens
            .saturating_sub(old_tokens)
            .saturating_add(new_tokens);
        if next_total > self.limits.max_tokens {
            return Err(ChatError::MessageTooLarge);
        }
        let removed_trace = self.messages[index]
            .trace
            .as_ref()
            .and_then(validated_trace_usage);
        let message = &mut self.messages[index];
        message.content = content.to_owned();
        message.trace = None;
        message.state = ChatMessageState::Complete;
        self.total_tokens = next_total;
        if let Some(removed_trace) = removed_trace {
            self.trace_usage.subtract(removed_trace);
        }
        self.refresh_voice_interruption_pending();
        Ok(())
    }

    /// 原子删除多条非活动消息，保留未选中的同轮消息供用户继续编辑。
    pub(super) fn delete_messages(&mut self, message_ids: &[u64]) -> Result<usize, ChatError> {
        let selected = message_ids.iter().copied().collect::<HashSet<_>>();
        if selected.is_empty() {
            return Ok(0);
        }
        if self.active_response.is_some_and(|active| {
            self.messages
                .iter()
                .any(|message| selected.contains(&message.id) && message.turn_id == active.turn_id)
        }) {
            return Err(ChatError::Busy);
        }

        let mut removed = 0_usize;
        let mut removed_tokens = 0_usize;
        let mut removed_image_bytes = 0_usize;
        let mut removed_trace_usage = TraceUsage::default();
        self.messages.retain(|message| {
            if !selected.contains(&message.id) {
                return true;
            }
            removed = removed.saturating_add(1);
            removed_tokens = removed_tokens.saturating_add(message_token_count(
                &message.content,
                image_context_tokens(message.image.as_ref()),
            ));
            removed_image_bytes = removed_image_bytes
                .saturating_add(message.image.as_ref().map_or(0, ImageAttachment::byte_len));
            if let Some(trace_usage) = message.trace.as_ref().and_then(validated_trace_usage) {
                removed_trace_usage.bytes =
                    removed_trace_usage.bytes.saturating_add(trace_usage.bytes);
                removed_trace_usage.messages = removed_trace_usage
                    .messages
                    .saturating_add(trace_usage.messages);
                removed_trace_usage.tool_executions = removed_trace_usage
                    .tool_executions
                    .saturating_add(trace_usage.tool_executions);
            }
            false
        });
        self.total_tokens = self.total_tokens.saturating_sub(removed_tokens);
        self.total_image_bytes = self.total_image_bytes.saturating_sub(removed_image_bytes);
        self.trace_usage.subtract(removed_trace_usage);
        self.refresh_voice_interruption_pending();
        Ok(removed)
    }

    /// 按稳定消息 ID 重排完整上下文，并重建 Provider 与快照共用的合法轮次分组。
    #[allow(dead_code)] // 排序语义由会话层保留，当前人格页面不再提供拖拽入口。
    pub(super) fn reorder_messages(&mut self, message_ids: &[u64]) -> Result<bool, ChatError> {
        let current_ids = self
            .messages
            .iter()
            .map(|message| message.id)
            .collect::<Vec<_>>();
        let requested_ids = message_ids.iter().copied().collect::<HashSet<_>>();
        if message_ids.len() != current_ids.len()
            || requested_ids.len() != message_ids.len()
            || current_ids.iter().any(|id| !requested_ids.contains(id))
        {
            return Err(ChatError::InvalidMessageOrder);
        }
        if current_ids == message_ids {
            return Ok(false);
        }
        if self.active_response.is_some() {
            return Err(ChatError::Busy);
        }

        let mut reordered = VecDeque::with_capacity(self.messages.len());
        for message_id in message_ids {
            let Some(message) = self
                .messages
                .iter()
                .find(|message| message.id == *message_id)
                .cloned()
            else {
                return Err(ChatError::InvalidMessageOrder);
            };
            reordered.push_back(message);
        }
        self.messages = reordered;
        self.rebuild_turn_ids();
        self.refresh_voice_interruption_pending();
        Ok(true)
    }

    /// 保留单条删除测试入口，生产路径统一使用原子批量删除。
    #[cfg(test)]
    pub(super) fn delete_message(&mut self, message_id: u64) -> Result<bool, ChatError> {
        self.delete_messages(&[message_id])
            .map(|removed| removed != 0)
    }

    /// 创建可由后台线程序列化的不可变快照。
    pub(super) fn snapshot(&self, revision: u64) -> ChatSessionSnapshot {
        ChatSessionSnapshot {
            version: SNAPSHOT_VERSION,
            revision,
            messages: self.messages.iter().cloned().collect(),
        }
    }

    /// 从不可信的本地快照恢复完整轮次，未完成响应会转为中断状态。
    ///
    /// # Errors
    ///
    /// 快照版本未知、结构非法或任一消息超过固定安全上限时返回错误。
    pub(super) fn from_snapshot(
        snapshot: ChatSessionSnapshot,
        limits: ChatLimits,
    ) -> Result<Self, ChatError> {
        if snapshot.version != SNAPSHOT_VERSION {
            return Err(ChatError::UnsupportedSnapshot);
        }
        let mut session = Self::new(limits)?;
        let mut source = VecDeque::from(snapshot.messages);
        let mut message_ids = HashSet::with_capacity(source.len());
        let mut turn_ids = HashSet::new();
        let mut snapshot_trace_usage = TraceUsage::default();
        while let Some(first) = source.pop_front() {
            let source_turn = first.turn_id;
            if source_turn == 0 || source_turn == u64::MAX || !turn_ids.insert(source_turn) {
                return Err(ChatError::InvalidSnapshot);
            }
            let mut turn = vec![first];
            while source
                .front()
                .is_some_and(|message| message.turn_id == source_turn)
            {
                let Some(message) = source.pop_front() else {
                    break;
                };
                turn.push(message);
            }
            if turn.iter().any(|message| {
                message.id == 0
                    || message.id == u64::MAX
                    || !message_ids.insert(message.id)
                    || message.content.len() > MAX_SESSION_TEXT_BYTES
                    || (message.role == ChatRole::User
                        && message.state != ChatMessageState::Complete)
                    || (message.role == ChatRole::Assistant && message.image.is_some())
                    || (message.role == ChatRole::User && message.trace.is_some())
                    || message
                        .image
                        .as_ref()
                        .is_some_and(|image| !image.has_safe_metadata())
            }) {
                return Err(ChatError::InvalidSnapshot);
            }
            let valid_shape = match turn.as_slice() {
                [message] => matches!(message.role, ChatRole::User | ChatRole::Assistant),
                [user, assistant] => {
                    user.role == ChatRole::User && assistant.role == ChatRole::Assistant
                }
                _ => false,
            };
            if !valid_shape {
                return Err(ChatError::InvalidSnapshot);
            }
            let mut turn_trace_usage = TraceUsage::default();
            for message in &turn {
                let Some(trace) = message.trace.as_ref() else {
                    continue;
                };
                let Some(trace_usage) = validated_trace_usage(trace) else {
                    return Err(ChatError::InvalidSnapshot);
                };
                let Some(next_snapshot_usage) = snapshot_trace_usage.checked_add(trace_usage)
                else {
                    return Err(ChatError::InvalidSnapshot);
                };
                let Some(next_turn_usage) = turn_trace_usage.checked_add(trace_usage) else {
                    return Err(ChatError::InvalidSnapshot);
                };
                snapshot_trace_usage = next_snapshot_usage;
                turn_trace_usage = next_turn_usage;
            }
            session.next_turn_id = session.next_turn_id.max(source_turn);
            session.next_message_id = session.next_message_id.max(
                turn.iter()
                    .map(|message| message.id)
                    .max()
                    .unwrap_or_default(),
            );
            let turn_tokens = turn.iter().fold(0_usize, |total, message| {
                total.saturating_add(message_token_count(
                    &message.content,
                    image_context_tokens(message.image.as_ref()),
                ))
            });
            let turn_image_bytes = turn.iter().fold(0_usize, |total, message| {
                total.saturating_add(message.image.as_ref().map_or(0, ImageAttachment::byte_len))
            });
            if turn.len() > limits.max_messages
                || turn_tokens > limits.max_tokens
                || turn_image_bytes > MAX_SESSION_IMAGE_BYTES
            {
                continue;
            }
            // 用户调小上限后旧快照仍必须可用：装不下的最旧轮次按窗口滚动规则淘汰。
            session.trim_completed_turns_for(turn.len(), turn_tokens, turn_image_bytes);
            if session.messages.len().saturating_add(turn.len()) > limits.max_messages
                || session.total_tokens.saturating_add(turn_tokens) > limits.max_tokens
                || session.total_image_bytes.saturating_add(turn_image_bytes)
                    > MAX_SESSION_IMAGE_BYTES
            {
                continue;
            }
            session.total_tokens = session.total_tokens.saturating_add(turn_tokens);
            session.total_image_bytes = session.total_image_bytes.saturating_add(turn_image_bytes);
            session.trace_usage = session
                .trace_usage
                .checked_add(turn_trace_usage)
                .ok_or(ChatError::InvalidSnapshot)?;
            for message in turn {
                session.messages.push_back(ChatMessage {
                    id: message.id,
                    turn_id: source_turn,
                    role: message.role,
                    content: message.content,
                    image: message.image,
                    trace: message.trace,
                    state: match message.state {
                        ChatMessageState::Streaming => ChatMessageState::Interrupted,
                        state => state,
                    },
                });
            }
        }
        session.voice_interruption_pending = session
            .messages
            .back()
            .is_some_and(|message| message.state == ChatMessageState::InterruptedByVoice);
        Ok(session)
    }

    fn context_messages(&self) -> Vec<ChatContextMessage> {
        let active_turn = self.active_response.map(|active| active.turn_id);
        let mut context = Vec::new();
        let mut index = 0;
        while index < self.messages.len() {
            let Some(first) = self.messages.get(index) else {
                break;
            };
            let turn_id = first.turn_id;
            let end = self
                .messages
                .iter()
                .skip(index)
                .position(|message| message.turn_id != turn_id)
                .map_or(self.messages.len(), |offset| index + offset);
            let turn = self.messages.range(index..end);
            let has_user = turn.clone().any(|message| {
                message.role == ChatRole::User && message.state == ChatMessageState::Complete
            });
            let has_assistant = turn.clone().any(|message| {
                message.role == ChatRole::Assistant
                    && matches!(
                        message.state,
                        ChatMessageState::Complete | ChatMessageState::InterruptedByVoice
                    )
            });
            let failed = active_turn != Some(turn_id)
                && turn.clone().any(|message| {
                    message.role == ChatRole::Assistant
                        && matches!(
                            message.state,
                            ChatMessageState::Streaming
                                | ChatMessageState::Failed(_)
                                | ChatMessageState::Cancelled
                                | ChatMessageState::Interrupted
                        )
                });
            if !failed {
                context.extend(turn.filter_map(|message| {
                    let include = match message.role {
                        ChatRole::User => {
                            message.state == ChatMessageState::Complete
                                && (active_turn == Some(turn_id) || has_assistant)
                        }
                        ChatRole::Assistant => {
                            has_user
                                && matches!(
                                    message.state,
                                    ChatMessageState::Complete
                                        | ChatMessageState::InterruptedByVoice
                                )
                        }
                    };
                    include.then(|| ChatContextMessage {
                        source_message_id: Some(message.id),
                        role: message.role,
                        content: message.content.clone(),
                        image: message.image.clone(),
                    })
                }));
            }
            index = end;
        }
        context
    }

    fn current_response(&self, response_id: ResponseId) -> Result<ActiveResponse, ChatError> {
        self.active_response
            .filter(|active| active.id == response_id)
            .ok_or(ChatError::StaleResponse)
    }

    fn set_response_state(&mut self, response_id: ResponseId, state: ChatMessageState) -> bool {
        let Ok(active) = self.current_response(response_id) else {
            return false;
        };
        let Some(message) = self
            .messages
            .iter_mut()
            .find(|message| message.id == active.message_id && message.turn_id == active.turn_id)
        else {
            self.active_response = None;
            return false;
        };
        message.state = state;
        self.active_response = None;
        true
    }

    fn refresh_voice_interruption_pending(&mut self) {
        self.voice_interruption_pending = self
            .messages
            .back()
            .is_some_and(|message| message.state == ChatMessageState::InterruptedByVoice);
    }

    #[allow(dead_code)]
    fn rebuild_turn_ids(&mut self) {
        let mut index = 0_usize;
        while index < self.messages.len() {
            let pair = self
                .messages
                .get(index)
                .is_some_and(|message| message.role == ChatRole::User)
                && self
                    .messages
                    .get(index.saturating_add(1))
                    .is_some_and(|message| message.role == ChatRole::Assistant);
            let turn_id = allocate(&mut self.next_turn_id);
            if let Some(message) = self.messages.get_mut(index) {
                message.turn_id = turn_id;
            }
            if pair {
                index = index.saturating_add(1);
                if let Some(message) = self.messages.get_mut(index) {
                    message.turn_id = turn_id;
                }
            }
            index = index.saturating_add(1);
        }
    }

    fn trim_completed_turns_for(
        &mut self,
        additional_messages: usize,
        additional_tokens: usize,
        additional_image_bytes: usize,
    ) {
        while self.messages.len().saturating_add(additional_messages) > self.limits.max_messages
            || self.total_tokens.saturating_add(additional_tokens) > self.limits.max_tokens
            || self
                .total_image_bytes
                .saturating_add(additional_image_bytes)
                > MAX_SESSION_IMAGE_BYTES
        {
            let Some(front) = self.messages.front() else {
                break;
            };
            if self
                .active_response
                .is_some_and(|active| active.turn_id == front.turn_id)
            {
                break;
            }
            let turn_id = front.turn_id;
            while self
                .messages
                .front()
                .is_some_and(|message| message.turn_id == turn_id)
            {
                let Some(removed) = self.messages.pop_front() else {
                    break;
                };
                self.total_tokens = self.total_tokens.saturating_sub(message_token_count(
                    &removed.content,
                    image_context_tokens(removed.image.as_ref()),
                ));
                self.total_image_bytes = self
                    .total_image_bytes
                    .saturating_sub(removed.image.as_ref().map_or(0, ImageAttachment::byte_len));
                if let Some(trace_usage) = removed.trace.as_ref().and_then(validated_trace_usage) {
                    self.trace_usage.subtract(trace_usage);
                }
            }
        }
    }
}

impl Default for ChatSession {
    fn default() -> Self {
        Self::new(ChatLimits::default()).expect("默认聊天限制必须容纳一轮非空对话")
    }
}

fn allocate(counter: &mut u64) -> u64 {
    *counter = counter.wrapping_add(1).max(1);
    *counter
}

fn validated_trace_usage(trace: &AssistantTrace) -> Option<TraceUsage> {
    if trace.is_empty()
        || trace.reasoning().is_some_and(|reasoning| {
            reasoning.trim().is_empty() || reasoning.len() > MAX_TRACE_REASONING_BYTES
        })
        || trace.tool_executions().len() > MAX_MESSAGE_TOOL_EXECUTIONS
    {
        return None;
    }
    for execution in trace.tool_executions() {
        if execution.name().trim().is_empty() || execution.name().len() > MAX_TRACE_TOOL_NAME_BYTES
        {
            return None;
        }
        let arguments_bytes = serde_json::to_vec(execution.arguments()).ok()?.len();
        let result_bytes = serde_json::to_vec(execution.result()).ok()?.len();
        if arguments_bytes > MAX_TRACE_JSON_BYTES || result_bytes > MAX_TRACE_JSON_BYTES {
            return None;
        }
    }
    let bytes = serde_json::to_vec(trace).ok()?.len();
    if bytes > MAX_MESSAGE_TRACE_BYTES {
        return None;
    }
    Some(TraceUsage {
        bytes,
        messages: 1,
        tool_executions: trace.tool_executions().len(),
    })
}

/// 估算跨 Provider 文本 token 数。模型词表不可用时按 UTF-8 密度与词法片段取较大值，
/// 避免中文、emoji 或长 ASCII 文本被明显低估。
pub(super) fn estimate_text_tokens(text: &str) -> usize {
    let bytes_estimate = text.len().div_ceil(3);
    let mut lexical_estimate = 0_usize;
    let mut ascii_word = 0_usize;
    for character in text.chars() {
        if character.is_ascii_alphanumeric() || character == '_' {
            ascii_word = ascii_word.saturating_add(1);
            continue;
        }
        lexical_estimate = lexical_estimate.saturating_add(ascii_word.div_ceil(4));
        ascii_word = 0;
        if !character.is_ascii_whitespace() {
            lexical_estimate = lexical_estimate.saturating_add(if character.is_ascii() {
                1
            } else {
                character.len_utf8().div_ceil(2)
            });
        }
    }
    lexical_estimate = lexical_estimate.saturating_add(ascii_word.div_ceil(4));
    bytes_estimate.max(lexical_estimate)
}

pub(super) fn context_message_tokens(content: &str, fixed_tokens: usize) -> usize {
    fixed_tokens.saturating_add(estimate_text_tokens(content))
}

fn message_token_count(content: &str, image_tokens: usize) -> usize {
    context_message_tokens(content, TOKENS_PER_MESSAGE.saturating_add(image_tokens))
}

fn image_context_tokens(image: Option<&ImageAttachment>) -> usize {
    match image {
        Some(image) if image.bytes().is_some() => IMAGE_CONTEXT_TOKENS,
        Some(_) | None => 0,
    }
}

fn request_image_context_tokens(image: Option<&ImageAttachment>) -> usize {
    match image {
        Some(image) if image.bytes().is_some() => IMAGE_CONTEXT_TOKENS,
        Some(_) => MISSING_IMAGE_CONTEXT_TOKENS,
        None => 0,
    }
}

fn trim_request_context(context: &mut Vec<ChatContextMessage>, maximum_tokens: usize) {
    let mut total = context.iter().fold(0_usize, |tokens, message| {
        tokens.saturating_add(message_token_count(
            &message.content,
            request_image_context_tokens(message.image.as_ref()),
        ))
    });
    while total > maximum_tokens && context.len() > 1 {
        let remove = if context.len() >= 2
            && context[0].role == ChatRole::User
            && context[1].role == ChatRole::Assistant
        {
            2
        } else {
            1
        };
        if remove >= context.len() {
            break;
        }
        for message in context.drain(..remove) {
            total = total.saturating_sub(message_token_count(
                &message.content,
                request_image_context_tokens(message.image.as_ref()),
            ));
        }
    }
}

fn truncated_with_suffix(content: &str, suffix: &str, maximum_tokens: usize) -> String {
    let full = format!("{content}{suffix}");
    if message_token_count(&full, 0) <= maximum_tokens {
        return full;
    }
    let mut boundaries = content
        .char_indices()
        .map(|(index, _)| index)
        .collect::<Vec<_>>();
    boundaries.push(content.len());
    let mut low = 0_usize;
    let mut high = boundaries.len();
    let mut keep = 0_usize;
    while low < high {
        let middle = low + (high - low) / 2;
        let boundary = boundaries[middle];
        let candidate = format!("{}{suffix}", &content[..boundary]);
        if message_token_count(&candidate, 0) <= maximum_tokens {
            keep = boundary;
            low = middle + 1;
        } else {
            high = middle;
        }
    }
    format!("{}{suffix}", &content[..keep])
}

/// 描述会话状态更新被拒绝的原因。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum ChatError {
    InvalidLimits,
    EmptyMessage,
    Busy,
    MessageTooLarge,
    StaleResponse,
    MissingResponse,
    MissingMessage,
    #[allow(dead_code)]
    InvalidMessageOrder,
    UnsupportedSnapshot,
    InvalidSnapshot,
}

impl fmt::Display for ChatError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let message = match self {
            Self::InvalidLimits => t!("chat.error.invalid_limits"),
            Self::EmptyMessage => t!("chat.error.empty_message"),
            Self::Busy => t!("chat.error.busy"),
            Self::MessageTooLarge => t!("chat.error.message_too_large"),
            Self::StaleResponse => t!("chat.error.stale_response"),
            Self::MissingResponse => t!("chat.error.missing_response"),
            Self::MissingMessage => t!("chat.error.missing_message"),
            Self::InvalidMessageOrder => t!("chat.error.invalid_message_order"),
            Self::UnsupportedSnapshot => t!("chat.error.unsupported_snapshot"),
            Self::InvalidSnapshot => t!("chat.error.invalid_snapshot"),
        };
        formatter.write_str(&message)
    }
}

impl Error for ChatError {}
