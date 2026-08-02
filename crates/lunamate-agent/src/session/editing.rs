//! 处理上下文限制更新、消息编辑删除和完整轮次淘汰。

use std::collections::HashSet;

use crate::{media::ImageAttachment, memory::ContextMessage};

use super::{
    ChatError, ChatLimits, ChatMessageState, ChatSession, MAX_SESSION_IMAGE_BYTES,
    MAX_SESSION_TEXT_BYTES, TOKENS_PER_MESSAGE, TraceUsage,
    snapshot::validated_trace_usage,
    tokens::{image_context_tokens, message_token_count},
};

impl ChatSession {
    /// 更新会话与请求预算，并按新上限淘汰最早的非活动轮次。
    pub fn update_limits(&mut self, limits: ChatLimits) -> Result<(), ChatError> {
        if limits.max_messages < 2 {
            return Err(ChatError::InvalidLimits);
        }
        self.limits = limits;
        self.trim_completed_turns_for(0, 0, 0);
        Ok(())
    }

    /// 创建设置页可以安全展示和编辑的消息快照。
    pub fn editable_messages(&self) -> Vec<ContextMessage> {
        self.messages
            .iter()
            .map(|message| {
                let image_tokens = image_context_tokens(message.image.as_ref());
                ContextMessage {
                    id: message.id,
                    role: message.role,
                    content: message.visible_content().to_owned(),
                    tokens: message_token_count(&message.content, image_tokens),
                    fixed_tokens: TOKENS_PER_MESSAGE.saturating_add(image_tokens),
                    trace: message.trace.clone(),
                }
            })
            .collect()
    }

    /// 清空短期上下文；调用方必须已经取消并中断活动请求。
    pub fn clear(&mut self) {
        self.interrupt_active_response();
        self.messages.clear();
        self.total_tokens = 0;
        self.total_image_bytes = 0;
        self.trace_usage = TraceUsage::default();
        self.active_response = None;
        self.voice_interruption_pending = false;
    }

    /// 修改一条非活动消息；编辑后的失败或中断消息恢复为可发送的完整消息。
    pub fn edit_message(&mut self, message_id: u64, content: &str) -> Result<(), ChatError> {
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
    pub fn delete_messages(&mut self, message_ids: &[u64]) -> Result<usize, ChatError> {
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

    /// 保留单条删除测试入口，生产路径统一使用原子批量删除。
    #[cfg(test)]
    pub(crate) fn delete_message(&mut self, message_id: u64) -> Result<bool, ChatError> {
        self.delete_messages(&[message_id])
            .map(|removed| removed != 0)
    }

    pub(super) fn trim_completed_turns_for(
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

    fn refresh_voice_interruption_pending(&mut self) {
        self.voice_interruption_pending = self
            .messages
            .back()
            .is_some_and(|message| message.state == ChatMessageState::InterruptedByVoice);
    }
}
