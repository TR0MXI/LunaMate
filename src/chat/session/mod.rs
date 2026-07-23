//! 管理单个有界会话，并用请求标识隔离取消、替换和迟到的流式结果。

use std::{collections::VecDeque, error::Error, fmt};

use rust_i18n::t;
use serde::{Deserialize, Serialize};

const SNAPSHOT_VERSION: u32 = 1;

/// 对话记录中一条消息的角色。
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub(crate) enum ChatRole {
    User,
    Assistant,
}

/// 消息在当前会话中的可见状态。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub(crate) enum ChatMessageState {
    Complete,
    Streaming,
    Failed(String),
    Cancelled,
    Interrupted,
}

/// 单个用户轮次中的一条聊天消息。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub(crate) struct ChatMessage {
    id: u64,
    turn_id: u64,
    role: ChatRole,
    content: String,
    state: ChatMessageState,
}

impl ChatMessage {
    /// 返回消息的稳定运行时 ID。
    pub(crate) fn id(&self) -> u64 {
        self.id
    }

    /// 返回消息角色。
    pub(crate) fn role(&self) -> ChatRole {
        self.role
    }

    /// 返回消息正文。
    pub(crate) fn content(&self) -> &str {
        &self.content
    }

    /// 返回消息终态或流式状态。
    pub(crate) fn state(&self) -> &ChatMessageState {
        &self.state
    }
}

/// 发送给 Provider 的纯文本上下文消息。
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ChatContextMessage {
    pub(crate) role: ChatRole,
    pub(crate) content: String,
}

/// 限制当前上下文的消息数量和 UTF-8 字节数。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct ChatLimits {
    pub(crate) max_messages: usize,
    pub(crate) max_bytes: usize,
}

impl Default for ChatLimits {
    fn default() -> Self {
        Self {
            max_messages: 64,
            max_bytes: 64 * 1024,
        }
    }
}

/// 标识一次流式响应，用于拒绝已取消或被新请求替换的结果。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct ResponseId(u64);

#[derive(Clone, Copy, Debug)]
struct ActiveResponse {
    id: ResponseId,
    turn_id: u64,
    message_id: u64,
}

/// 原子创建一个用户轮次后交给网络层的结果。
pub(crate) struct StartedTurn {
    pub(crate) response_id: ResponseId,
    pub(crate) context: Vec<ChatContextMessage>,
}

/// 写入磁盘的版本化单会话快照；不包含活动网络请求或任何凭据。
#[derive(Clone, Debug, Deserialize, Serialize)]
pub(crate) struct ChatSessionSnapshot {
    version: u32,
    pub(crate) revision: u64,
    messages: Vec<ChatMessage>,
}

/// 保存单个有界会话，并跟踪至多一个活动流式响应。
pub(crate) struct ChatSession {
    messages: VecDeque<ChatMessage>,
    limits: ChatLimits,
    total_bytes: usize,
    next_message_id: u64,
    next_turn_id: u64,
    next_response_id: u64,
    active_response: Option<ActiveResponse>,
}

impl ChatSession {
    /// 使用给定限制创建空会话。
    ///
    /// # Errors
    ///
    /// 消息上限不足以容纳一轮对话，或字节上限为零时返回错误。
    pub(crate) fn new(limits: ChatLimits) -> Result<Self, ChatError> {
        if limits.max_messages < 2 || limits.max_bytes == 0 {
            return Err(ChatError::InvalidLimits);
        }
        Ok(Self {
            messages: VecDeque::new(),
            limits,
            total_bytes: 0,
            next_message_id: 0,
            next_turn_id: 0,
            next_response_id: 0,
            active_response: None,
        })
    }

    /// 返回用于界面展示的全部有界消息。
    pub(crate) fn messages(&self) -> &VecDeque<ChatMessage> {
        &self.messages
    }

    /// 返回当前活动响应 ID。
    pub(crate) fn active_response_id(&self) -> Option<ResponseId> {
        self.active_response.map(|active| active.id)
    }

    /// 原子写入用户消息、创建助手占位并生成不含占位消息的请求上下文。
    ///
    /// # Errors
    ///
    /// 当前已有请求、消息为空或单条消息超过字节限制时返回错误。
    pub(crate) fn start_turn(
        &mut self,
        content: impl Into<String>,
    ) -> Result<StartedTurn, ChatError> {
        if self.active_response.is_some() {
            return Err(ChatError::Busy);
        }
        let content = content.into();
        let content = content.trim();
        if content.is_empty() {
            return Err(ChatError::EmptyMessage);
        }
        if content.len() >= self.limits.max_bytes {
            return Err(ChatError::MessageTooLarge);
        }

        self.trim_completed_turns_for(2, content.len());
        if self.messages.len().saturating_add(2) > self.limits.max_messages
            || self.total_bytes.saturating_add(content.len()) > self.limits.max_bytes
        {
            return Err(ChatError::MessageTooLarge);
        }

        let turn_id = allocate(&mut self.next_turn_id);
        let response_id = ResponseId(allocate(&mut self.next_response_id));
        let user_id = allocate(&mut self.next_message_id);
        let assistant_id = allocate(&mut self.next_message_id);
        let content = content.to_owned();
        self.total_bytes += content.len();
        self.messages.push_back(ChatMessage {
            id: user_id,
            turn_id,
            role: ChatRole::User,
            content,
            state: ChatMessageState::Complete,
        });
        self.messages.push_back(ChatMessage {
            id: assistant_id,
            turn_id,
            role: ChatRole::Assistant,
            content: String::new(),
            state: ChatMessageState::Streaming,
        });
        self.active_response = Some(ActiveResponse {
            id: response_id,
            turn_id,
            message_id: assistant_id,
        });

        Ok(StartedTurn {
            response_id,
            context: self.context_messages(),
        })
    }

    /// 将增量文本追加到当前助手消息。
    ///
    /// # Errors
    ///
    /// 响应已过期、目标消息缺失或响应超过会话字节上限时返回错误。
    pub(crate) fn append_response(
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
        if new_len > self.limits.max_bytes {
            return Err(ChatError::MessageTooLarge);
        }

        let active_turn_bytes = user
            .map(|message| message.content.len())
            .and_then(|user_len| user_len.checked_add(new_len))
            .ok_or(ChatError::MessageTooLarge)?;
        if active_turn_bytes > self.limits.max_bytes {
            return Err(ChatError::MessageTooLarge);
        }

        self.trim_completed_turns_for(0, chunk.len());
        if self.total_bytes.saturating_add(chunk.len()) > self.limits.max_bytes {
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
        self.total_bytes += chunk.len();
        Ok(())
    }

    /// 完成匹配的流式响应；迟到完成不会影响新请求。
    pub(crate) fn finish_response(&mut self, response_id: ResponseId) -> bool {
        self.set_response_state(response_id, ChatMessageState::Complete)
    }

    /// 将匹配响应标记为失败并保留已经收到的部分文本。
    pub(crate) fn fail_response(&mut self, response_id: ResponseId, message: String) -> bool {
        self.set_response_state(response_id, ChatMessageState::Failed(message))
    }

    /// 仅取消匹配的响应，旧任务不能取消后续新请求。
    pub(crate) fn cancel_response(&mut self, response_id: ResponseId) -> bool {
        self.set_response_state(response_id, ChatMessageState::Cancelled)
    }

    /// 将退出时仍活动的响应标记为中断，且不允许迟到事件继续写入。
    pub(crate) fn interrupt_active_response(&mut self) {
        if let Some(active) = self.active_response {
            self.set_response_state(active.id, ChatMessageState::Interrupted);
        }
    }

    /// 清空当前单会话；调用方应先终止对应网络任务。
    pub(crate) fn clear(&mut self) {
        self.messages.clear();
        self.total_bytes = 0;
        self.active_response = None;
    }

    /// 创建可由后台线程序列化的不可变快照。
    pub(crate) fn snapshot(&self, revision: u64) -> ChatSessionSnapshot {
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
    /// 快照版本未知，或任一消息超过当前字节限制时返回错误。
    pub(crate) fn from_snapshot(
        snapshot: ChatSessionSnapshot,
        limits: ChatLimits,
    ) -> Result<Self, ChatError> {
        if snapshot.version != SNAPSHOT_VERSION {
            return Err(ChatError::UnsupportedSnapshot);
        }
        let mut session = Self::new(limits)?;
        if !snapshot.messages.len().is_multiple_of(2) {
            return Err(ChatError::InvalidSnapshot);
        }
        for pair in snapshot.messages.chunks_exact(2) {
            let user = &pair[0];
            let assistant = &pair[1];
            if user.role != ChatRole::User
                || assistant.role != ChatRole::Assistant
                || user.turn_id != assistant.turn_id
                || user.state != ChatMessageState::Complete
            {
                return Err(ChatError::InvalidSnapshot);
            }
            if user.content.len() > limits.max_bytes || assistant.content.len() > limits.max_bytes {
                return Err(ChatError::MessageTooLarge);
            }
            if user
                .content
                .len()
                .checked_add(assistant.content.len())
                .is_none_or(|turn_bytes| turn_bytes > limits.max_bytes)
            {
                return Err(ChatError::MessageTooLarge);
            }

            let turn_id = allocate(&mut session.next_turn_id);
            let user_id = allocate(&mut session.next_message_id);
            let assistant_id = allocate(&mut session.next_message_id);
            let assistant_state = match &assistant.state {
                ChatMessageState::Streaming => ChatMessageState::Interrupted,
                state => state.clone(),
            };
            session.total_bytes = session
                .total_bytes
                .saturating_add(user.content.len())
                .saturating_add(assistant.content.len());
            session.messages.push_back(ChatMessage {
                id: user_id,
                turn_id,
                role: ChatRole::User,
                content: user.content.clone(),
                state: ChatMessageState::Complete,
            });
            session.messages.push_back(ChatMessage {
                id: assistant_id,
                turn_id,
                role: ChatRole::Assistant,
                content: assistant.content.clone(),
                state: assistant_state,
            });
            session.trim_completed_turns_for(0, 0);
        }
        Ok(session)
    }

    fn context_messages(&self) -> Vec<ChatContextMessage> {
        let active_turn = self.active_response.map(|active| active.turn_id);
        let mut context = Vec::new();
        let mut index = 0;
        while index < self.messages.len() {
            let Some(user) = self.messages.get(index) else {
                break;
            };
            let assistant = self.messages.get(index + 1).filter(|assistant| {
                assistant.turn_id == user.turn_id && assistant.role == ChatRole::Assistant
            });
            if user.role != ChatRole::User {
                index += 1;
                continue;
            }

            if active_turn == Some(user.turn_id) {
                context.push(ChatContextMessage {
                    role: ChatRole::User,
                    content: user.content.clone(),
                });
            } else if let Some(assistant) = assistant
                && assistant.state == ChatMessageState::Complete
            {
                context.push(ChatContextMessage {
                    role: ChatRole::User,
                    content: user.content.clone(),
                });
                context.push(ChatContextMessage {
                    role: ChatRole::Assistant,
                    content: assistant.content.clone(),
                });
            }
            index += usize::from(assistant.is_some()) + 1;
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

    fn trim_completed_turns_for(&mut self, additional_messages: usize, additional_bytes: usize) {
        while self.messages.len().saturating_add(additional_messages) > self.limits.max_messages
            || self.total_bytes.saturating_add(additional_bytes) > self.limits.max_bytes
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
                self.total_bytes = self.total_bytes.saturating_sub(removed.content.len());
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

/// 描述会话状态更新被拒绝的原因。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ChatError {
    InvalidLimits,
    EmptyMessage,
    Busy,
    MessageTooLarge,
    StaleResponse,
    MissingResponse,
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
            Self::UnsupportedSnapshot => t!("chat.error.unsupported_snapshot"),
            Self::InvalidSnapshot => t!("chat.error.invalid_snapshot"),
        };
        formatter.write_str(&message)
    }
}

impl Error for ChatError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn context_excludes_streaming_placeholder() {
        let mut session = ChatSession::default();
        let started = session.start_turn("hello").expect("用户消息应当可发送");

        assert_eq!(
            started.context,
            vec![ChatContextMessage {
                role: ChatRole::User,
                content: "hello".to_owned(),
            }]
        );
        assert_eq!(session.messages().len(), 2);
    }

    #[test]
    fn stale_cancel_cannot_cancel_replacement_request() {
        let mut session = ChatSession::default();
        let old = session.start_turn("old").expect("第一轮应当可开始");
        assert!(session.cancel_response(old.response_id));
        let current = session.start_turn("new").expect("取消后应当可开始新一轮");

        assert!(!session.cancel_response(old.response_id));
        session
            .append_response(current.response_id, "answer")
            .expect("当前请求应当保持有效");
    }

    #[test]
    fn history_trims_complete_turns_without_evicting_active_response() {
        let mut session = ChatSession::new(ChatLimits {
            max_messages: 4,
            max_bytes: 14,
        })
        .expect("测试限制必须有效");
        let first = session.start_turn("first").expect("第一轮应当可开始");
        session
            .append_response(first.response_id, "one")
            .expect("第一轮回复应当可写入");
        assert!(session.finish_response(first.response_id));

        let second = session.start_turn("second").expect("第二轮应当可开始");
        session
            .append_response(second.response_id, "two")
            .expect("活动回复应当通过淘汰旧轮次获得空间");

        assert_eq!(session.messages().len(), 2);
        assert_eq!(session.messages()[0].content(), "second");
        assert_eq!(session.messages()[1].content(), "two");
    }

    #[test]
    fn failed_turn_is_not_replayed_in_next_context() {
        let mut session = ChatSession::default();
        let failed = session.start_turn("failed").expect("失败轮次应当可开始");
        assert!(session.fail_response(failed.response_id, "offline".to_owned()));
        let next = session.start_turn("next").expect("失败后应当可继续");

        assert_eq!(next.context.len(), 1);
        assert_eq!(next.context[0].content, "next");
    }

    #[test]
    fn restoring_streaming_response_marks_it_interrupted() {
        let mut session = ChatSession::default();
        let started = session.start_turn("hello").expect("测试轮次应当可开始");
        session
            .append_response(started.response_id, "partial")
            .expect("测试增量应当可写入");
        let snapshot = session.snapshot(7);

        let restored = ChatSession::from_snapshot(snapshot, ChatLimits::default())
            .expect("当前版本快照应当可恢复");
        assert_eq!(restored.messages().len(), 2);
        assert_eq!(
            restored.messages()[1].state(),
            &ChatMessageState::Interrupted
        );
        assert_eq!(restored.active_response_id(), None);
    }

    #[test]
    fn oversized_active_turn_does_not_evict_previous_history() {
        let mut session = ChatSession::new(ChatLimits {
            max_messages: 6,
            max_bytes: 12,
        })
        .expect("测试限制应当有效");
        let first = session.start_turn("a").expect("首轮应当可开始");
        session
            .append_response(first.response_id, "b")
            .expect("首轮回复应当可写入");
        session.finish_response(first.response_id);
        let second = session.start_turn("12345").expect("第二轮应当可开始");

        let error = session
            .append_response(second.response_id, "12345678")
            .expect_err("活动轮次自身超限时必须拒绝");
        assert_eq!(error, ChatError::MessageTooLarge);
        assert_eq!(session.messages().len(), 4);
        assert_eq!(session.messages()[0].content(), "a");
    }

    #[test]
    fn user_message_must_leave_room_for_response() {
        let mut session = ChatSession::new(ChatLimits {
            max_messages: 2,
            max_bytes: 4,
        })
        .expect("测试限制应当有效");

        assert!(matches!(
            session.start_turn("1234"),
            Err(ChatError::MessageTooLarge)
        ));
    }

    #[test]
    fn malformed_snapshot_is_rejected_instead_of_silently_dropping_messages() {
        let mut session = ChatSession::default();
        let started = session.start_turn("hello").expect("测试轮次应当可开始");
        session.finish_response(started.response_id);
        let mut snapshot = session.snapshot(1);
        snapshot.messages.pop();

        assert!(matches!(
            ChatSession::from_snapshot(snapshot, ChatLimits::default()),
            Err(ChatError::InvalidSnapshot)
        ));
    }
}
