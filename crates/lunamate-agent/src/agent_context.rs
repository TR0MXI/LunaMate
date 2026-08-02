//! 协调短期上下文编辑、清理和最终有序持久化。

use std::sync::Arc;

use crate::{
    Agent, AgentError, ChatLimits,
    session::{self, ChatSession},
    store::{
        ChatSessionStore, SessionOperationReservation, delete_persona_session_reserved,
        mutate_persona_session_reserved,
    },
};

use super::agent_coordination::{abort_active_request, next_revision};

impl Agent {
    /// 清除指定人格的短期上下文，并等待持久化结果。
    pub async fn clear_context(&self, persona: &str) -> Result<(), AgentError> {
        let runtime = self.runtime.read().clone();
        if runtime.active_persona != persona {
            let persistence = runtime
                .memory
                .persistence()
                .ok_or_else(|| AgentError::Persistence("Agent 持久化当前不可用".to_owned()))?;
            let operation = runtime.memory.session_document_lock().reserve();
            return delete_persona_session_reserved(&persistence, persona, operation)
                .await
                .map_err(|error| AgentError::Persistence(error.to_string()));
        }
        let save = {
            let mut state = self.state.lock();
            abort_active_request(&mut state, "context_clear");
            state.session.clear();
            state.reply_message_id = None;
            state.status = None;
            state.persist_revision = next_revision(state.persist_revision);
            let operation = state.store.reserve_document_operation();
            state.store.is_available().then(|| {
                (
                    state.store.clone(),
                    state.session.snapshot(state.persist_revision),
                    operation,
                )
            })
        };
        self.publish_live_context_for(&runtime);
        self.notify_state();
        persist_reserved(save).await
    }

    /// 修改指定人格的一条短期上下文消息，并等待持久化结果。
    pub async fn edit_context_message(
        &self,
        persona: &str,
        limits: ChatLimits,
        message_id: u64,
        content: String,
    ) -> Result<(), AgentError> {
        self.mutate_context(persona, limits, move |session| {
            session.edit_message(message_id, &content).map(|()| true)
        })
        .await
    }

    /// 原子删除指定人格的一组短期上下文消息，并等待持久化结果。
    pub async fn delete_context_messages(
        &self,
        persona: &str,
        limits: ChatLimits,
        message_ids: Vec<u64>,
    ) -> Result<(), AgentError> {
        self.mutate_context(persona, limits, move |session| {
            session
                .delete_messages(&message_ids)
                .map(|removed| removed != 0)
        })
        .await
    }

    async fn mutate_context<F>(
        &self,
        persona: &str,
        limits: ChatLimits,
        mutation: F,
    ) -> Result<(), AgentError>
    where
        F: FnOnce(&mut ChatSession) -> Result<bool, session::ChatError> + Send + 'static,
    {
        let runtime = self.runtime.read().clone();
        if runtime.active_persona != persona {
            let persistence = runtime
                .memory
                .persistence()
                .ok_or_else(|| AgentError::Persistence("Agent 持久化当前不可用".to_owned()))?;
            let operation = runtime.memory.session_document_lock().reserve();
            return match mutate_persona_session_reserved(
                &persistence,
                persona,
                limits,
                operation,
                mutation,
            )
            .await
            {
                Ok(true) => Ok(()),
                Ok(false) => Err(AgentError::Session("上下文消息不存在".to_owned())),
                Err(error) => Err(AgentError::Persistence(error.to_string())),
            };
        }

        let save = {
            let mut state = self.state.lock();
            if state.session.active_response_id().is_some() {
                abort_active_request(&mut state, "context_edit");
                state.session.interrupt_active_response();
            }
            let changed = mutation(&mut state.session)
                .map_err(|error| AgentError::Session(error.to_string()))?;
            if !changed {
                return Err(AgentError::Session("上下文消息不存在".to_owned()));
            }
            if state.reply_message_id.is_some_and(|message_id| {
                !state
                    .session
                    .messages()
                    .iter()
                    .any(|message| message.id() == message_id)
            }) {
                state.reply_message_id = None;
            }
            state.persist_revision = next_revision(state.persist_revision);
            let operation = state.store.reserve_document_operation();
            state.store.is_available().then(|| {
                (
                    state.store.clone(),
                    state.session.snapshot(state.persist_revision),
                    operation,
                )
            })
        };
        self.publish_live_context_for(&runtime);
        self.notify_state();
        persist_reserved(save).await
    }

    /// 幂等停止请求并等待最终会话快照完成有序写入。
    pub async fn shutdown(&self) -> Result<(), String> {
        let save = {
            let mut state = self.state.lock();
            abort_active_request(&mut state, "shutdown");
            state.session.interrupt_active_response();
            state.shutting_down = true;
            state.pending_voice = None;
            state.persist_revision = next_revision(state.persist_revision);
            let operation = state.store.reserve_document_operation();
            state.store.is_available().then(|| {
                (
                    state.store.clone(),
                    state.session.snapshot(state.persist_revision),
                    operation,
                )
            })
        };
        self.publish_live_context();
        self.notify_state();
        let Some((store, snapshot, operation)) = save else {
            return Ok(());
        };
        store
            .save_reserved(snapshot, operation)
            .await
            .map_err(|error| error.to_string())
    }
}

async fn persist_reserved(
    save: Option<(
        Arc<ChatSessionStore>,
        session::ChatSessionSnapshot,
        SessionOperationReservation,
    )>,
) -> Result<(), AgentError> {
    let Some((store, snapshot, operation)) = save else {
        return Err(AgentError::Persistence("Agent 持久化当前不可用".to_owned()));
    };
    store
        .save_reserved(snapshot, operation)
        .await
        .map_err(|error| AgentError::Persistence(error.to_string()))
}
