//! 集中处理 Agent 状态通知、实时上下文发布和后台持久化。

use std::{sync::atomic::Ordering, time::Duration};

use crate::{Agent, AgentRuntime, AgentState, logging::sanitize_log_field};

const PERSIST_INTERVAL: Duration = Duration::from_secs(3);

impl Agent {
    pub(super) fn persist(&self, force: bool) {
        let runtime = self.runtime.read().clone();
        let save = {
            let mut state = self.state.lock();
            runtime.memory.live_context_usage().publish(
                &runtime.active_persona,
                state.session.usage(),
                state.session.editable_messages(),
            );
            if !state.store.is_available()
                || (!force && state.last_persist.elapsed() < PERSIST_INTERVAL)
            {
                return;
            }
            state.persist_revision = next_revision(state.persist_revision);
            state.last_persist = std::time::Instant::now();
            let operation = state.store.reserve_document_operation();
            (
                state.store.clone(),
                state.session.snapshot(state.persist_revision),
                operation,
            )
        };
        let Some(runtime) = self.persistence_runtime.clone() else {
            log::error!("event=chat_session_persist_failed reason=runtime_unavailable");
            return;
        };
        runtime.spawn(async move {
            if let Err(error) = save.0.save_reserved(save.1, save.2).await {
                log::error!(
                    "event=chat_session_persist_failed error_kind={}",
                    error.diagnostic_kind()
                );
            }
        });
    }

    pub(super) fn publish_live_context(&self) {
        let runtime = self.runtime.read().clone();
        self.publish_live_context_for(&runtime);
    }

    pub(super) fn publish_live_context_for(&self, runtime: &AgentRuntime) {
        let state = self.state.lock();
        runtime.memory.live_context_usage().publish(
            &runtime.active_persona,
            state.session.usage(),
            state.session.editable_messages(),
        );
    }

    pub(super) fn notify_state(&self) {
        let revision = self
            .state_revision
            .fetch_add(1, Ordering::AcqRel)
            .wrapping_add(1)
            .max(1);
        self.state_updates.send_replace(revision);
    }
}

pub(super) fn abort_active_request(state: &mut AgentState, reason: &'static str) {
    if let Some(request) = state.active_request.take() {
        log::debug!(
            "event=agent_request_aborted response_id={} reason={} elapsed_ms={}",
            request.response_id.get(),
            sanitize_log_field(reason),
            request.started_at.elapsed().as_millis()
        );
        request.abort.abort();
    }
}

pub(super) fn next_revision(revision: u64) -> u64 {
    revision.wrapping_add(1).max(1)
}
