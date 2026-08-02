//! 处理会话轮次、流式响应和 Provider 上下文构造。

use rust_i18n::t;

use crate::{config::AppLanguage, memory::ContextUsage};

use super::{
    ActiveResponse, ChatContextMessage, ChatError, ChatLimits, ChatMessage, ChatMessageState,
    ChatRole, ChatSession, MAX_SESSION_IMAGE_BYTES, MAX_SESSION_TEXT_BYTES, ResponseId,
    StartedTurn, TraceUsage,
    snapshot::validated_trace_usage,
    tokens::{
        image_context_tokens, message_token_count, request_image_context_tokens,
        trim_request_context,
    },
    voice_interruption_marker,
};

impl ChatSession {
    /// 使用给定限制创建空会话。
    ///
    /// # Errors
    ///
    /// 消息上限不足以容纳一轮对话时返回错误；零 token 预算允许恢复为空会话，
    /// 但发送新消息会返回 [`ChatError::MessageTooLarge`]。
    pub fn new(limits: ChatLimits) -> Result<Self, ChatError> {
        if limits.max_messages < 2 {
            return Err(ChatError::InvalidLimits);
        }
        Ok(Self {
            messages: std::collections::VecDeque::new(),
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
    pub fn messages(&self) -> &std::collections::VecDeque<ChatMessage> {
        &self.messages
    }

    /// 返回当前短期上下文占用与生效上限，供人格设置界面展示。
    pub fn usage(&self) -> ContextUsage {
        ContextUsage {
            messages: self.messages.len(),
            max_messages: self.limits.max_messages,
            tokens: self.total_tokens,
            max_tokens: self.limits.max_tokens,
        }
    }

    /// 返回当前活动响应 ID。
    pub fn active_response_id(&self) -> Option<ResponseId> {
        self.active_response.map(|active| active.id)
    }

    /// 原子写入用户消息、创建助手占位并生成不含占位消息的请求上下文。
    ///
    /// # Errors
    ///
    /// 当前已有请求、消息为空或单条消息超过本地限制时返回错误。
    #[cfg(test)]
    pub(crate) fn start_turn(
        &mut self,
        content: impl Into<String>,
    ) -> Result<StartedTurn, ChatError> {
        self.start_turn_with_image(content, None, AppLanguage::default())
    }

    /// 创建可选附图的用户轮次；图片像素只驻留于有界内存，不进入数据库快照。
    pub fn start_turn_with_image(
        &mut self,
        content: impl Into<String>,
        image: Option<crate::media::ImageAttachment>,
        language: AppLanguage,
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
            t!("chat.image_only_prompt", locale = language.id()).to_string()
        } else {
            content.to_owned()
        };
        if content.len() > MAX_SESSION_TEXT_BYTES {
            return Err(ChatError::MessageTooLarge);
        }
        let image_bytes = image
            .as_ref()
            .map_or(0, crate::media::ImageAttachment::byte_len);
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
        let interruption_marker = self
            .voice_interruption_pending
            .then(|| voice_interruption_marker(language));
        let request_content = if let Some(marker) = &interruption_marker {
            format!("{marker}\n\n{content}")
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

        let mut context = self.context_messages(language);
        trim_request_context(&mut context, self.limits.max_request_tokens);
        if let Some(marker) = interruption_marker {
            let marker_retained = interrupted_message_id.is_some_and(|message_id| {
                context.iter().any(|message| {
                    message.source_message_id == Some(message_id)
                        && message.role == ChatRole::Assistant
                        && message.content.ends_with(&marker)
                })
            });
            if !marker_retained
                && let Some(user) = context
                    .iter_mut()
                    .rev()
                    .find(|message| message.role == ChatRole::User)
            {
                user.content = format!("{marker}\n\n{}", user.content);
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
    pub fn append_response(
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
    pub fn attach_response_trace(
        &mut self,
        response_id: ResponseId,
        trace: crate::memory::AssistantTrace,
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
    pub fn finish_response(&mut self, response_id: ResponseId) -> bool {
        self.set_response_state(response_id, ChatMessageState::Complete)
    }

    /// 将匹配响应标记为失败并保留已经收到的部分文本。
    pub fn fail_response(&mut self, response_id: ResponseId, message: String) -> bool {
        self.set_response_state(response_id, ChatMessageState::Failed(message))
    }

    /// 仅取消匹配的响应，旧任务不能取消后续新请求。
    pub fn cancel_response(&mut self, response_id: ResponseId) -> bool {
        self.set_response_state(response_id, ChatMessageState::Cancelled)
    }

    /// 丢弃匹配响应所在的整轮消息；调用方必须已经中止网络请求。
    pub fn discard_response_turn(&mut self, response_id: ResponseId) -> bool {
        let Some(active) = self
            .active_response
            .filter(|active| active.id == response_id)
        else {
            return false;
        };
        self.active_response = None;
        let message_ids = self
            .messages
            .iter()
            .filter(|message| message.turn_id == active.turn_id)
            .map(ChatMessage::id)
            .collect::<Vec<_>>();
        self.delete_messages(&message_ids)
            .is_ok_and(|removed| removed != 0)
    }

    /// 标记匹配的回复，并保留该终态供下一次模型请求按请求语言注入打断语义。
    pub fn interrupt_response_by_voice(&mut self, response_id: ResponseId) -> bool {
        let interrupted =
            self.set_response_state(response_id, ChatMessageState::InterruptedByVoice);
        self.voice_interruption_pending = interrupted;
        interrupted
    }

    /// 将退出时仍活动的响应标记为中断，且不允许迟到事件继续写入。
    pub fn interrupt_active_response(&mut self) {
        if let Some(active) = self.active_response {
            self.set_response_state(active.id, ChatMessageState::Interrupted);
        }
    }

    fn context_messages(&self, language: AppLanguage) -> Vec<ChatContextMessage> {
        let active_turn = self.active_response.map(|active| active.turn_id);
        let interruption_marker = voice_interruption_marker(language);
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
                    include.then(|| {
                        let mut content = message.content.clone();
                        if message.state == ChatMessageState::InterruptedByVoice {
                            content.push_str(&interruption_marker);
                        }
                        ChatContextMessage {
                            source_message_id: Some(message.id),
                            role: message.role,
                            content,
                            image: message.image.clone(),
                        }
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
}

fn allocate(counter: &mut u64) -> u64 {
    *counter = counter.wrapping_add(1).max(1);
    *counter
}
