//! 校验、恢复和生成版本化会话快照。

use std::collections::{HashSet, VecDeque};

use crate::{media::ImageAttachment, memory::AssistantTrace};

use super::{
    ChatError, ChatLimits, ChatMessage, ChatMessageState, ChatRole, ChatSession,
    ChatSessionSnapshot, MAX_MESSAGE_TOOL_EXECUTIONS, MAX_MESSAGE_TRACE_BYTES,
    MAX_SESSION_IMAGE_BYTES, MAX_SESSION_TEXT_BYTES, MAX_TRACE_JSON_BYTES,
    MAX_TRACE_REASONING_BYTES, MAX_TRACE_TOOL_NAME_BYTES, SNAPSHOT_VERSION, TraceUsage,
    tokens::{image_context_tokens, message_token_count},
};

impl ChatSession {
    /// 创建可由后台线程序列化的不可变快照。
    pub fn snapshot(&self, revision: u64) -> ChatSessionSnapshot {
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
    pub fn from_snapshot(
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
            // 用户调小上限后仍需恢复会话：装不下的最早轮次按窗口滚动规则淘汰。
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
}

pub(super) fn validated_trace_usage(trace: &AssistantTrace) -> Option<TraceUsage> {
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
