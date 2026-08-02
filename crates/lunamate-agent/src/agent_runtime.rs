//! 构造 Agent、发布只读状态并原子应用运行时配置。

use std::{
    sync::{Arc, atomic::Ordering},
    time::Instant,
};

use tokio::{runtime::Handle, sync::watch};

use crate::{
    Agent, AgentError, AgentMemory, AgentRuntime, AgentSnapshot, AgentState, ChatLimits,
    ChatOptions, Client, ModelIden, config::AppLanguage, session::ChatSession,
    store::ChatSessionStore,
};

use super::{
    AGENT_EFFECT_CHANNEL_CAPACITY,
    agent_coordination::{abort_active_request, next_revision},
};

impl Agent {
    /// 使用空会话直接组合运行时组件，不执行任何持久化读取。
    #[expect(
        clippy::too_many_arguments,
        reason = "构造函数显式接收相互独立且可运行时替换的 Agent 组件"
    )]
    pub fn new(
        client: Client,
        model: Option<ModelIden>,
        options: Option<ChatOptions>,
        system_prompt: impl Into<String>,
        memory: AgentMemory,
        active_persona: impl Into<String>,
        limits: ChatLimits,
        language: AppLanguage,
        initial_status: Option<String>,
    ) -> Arc<Self> {
        let (state_updates, _) = watch::channel(0);
        let (effects, effect_events) = async_channel::bounded(AGENT_EFFECT_CHANNEL_CAPACITY);
        let agent = Arc::new(Self {
            runtime: parking_lot::RwLock::new(AgentRuntime {
                revision: 1,
                configuration_revision: 0,
                client,
                model,
                options,
                system_prompt: Arc::from(system_prompt.into()),
                memory,
                active_persona: active_persona.into(),
                limits,
                language,
            }),
            state: parking_lot::Mutex::new(AgentState {
                session: ChatSession::new(limits).unwrap_or_default(),
                store: ChatSessionStore::unavailable(),
                persist_revision: 0,
                last_persist: Instant::now(),
                status: initial_status,
                reply_message_id: None,
                active_request: None,
                request_revision: 1,
                pending_configuration_revision: 0,
                switching_memory: false,
                suspended: false,
                shutting_down: false,
                pending_voice: None,
            }),
            persistence_runtime: Handle::try_current().ok(),
            state_revision: std::sync::atomic::AtomicU64::new(0),
            state_updates,
            effects,
            effect_events,
        });
        agent.publish_live_context();
        agent.notify_state();
        agent
    }

    /// 使用调用方直接提供的运行时组件恢复活动人格会话。
    #[expect(
        clippy::too_many_arguments,
        reason = "构造函数显式接收相互独立且可运行时替换的 Agent 组件"
    )]
    pub async fn load(
        client: Client,
        model: Option<ModelIden>,
        options: Option<ChatOptions>,
        system_prompt: impl Into<String>,
        memory: AgentMemory,
        active_persona: impl Into<String>,
        limits: ChatLimits,
        language: AppLanguage,
        initial_status: Option<String>,
    ) -> Arc<Self> {
        let active_persona = active_persona.into();
        let (session, store, restore_status) =
            load_persona_session(&memory, &active_persona, limits, language).await;
        let (state_updates, _) = watch::channel(0);
        let (effects, effect_events) = async_channel::bounded(AGENT_EFFECT_CHANNEL_CAPACITY);
        let agent = Arc::new(Self {
            runtime: parking_lot::RwLock::new(AgentRuntime {
                revision: 1,
                configuration_revision: 0,
                client,
                model,
                options,
                system_prompt: Arc::from(system_prompt.into()),
                memory,
                active_persona,
                limits,
                language,
            }),
            state: parking_lot::Mutex::new(AgentState {
                persist_revision: store.latest_revision(),
                session,
                store,
                last_persist: Instant::now(),
                status: restore_status.or(initial_status),
                reply_message_id: None,
                active_request: None,
                request_revision: 1,
                pending_configuration_revision: 0,
                switching_memory: false,
                suspended: false,
                shutting_down: false,
                pending_voice: None,
            }),
            persistence_runtime: Some(Handle::current()),
            state_revision: std::sync::atomic::AtomicU64::new(0),
            state_updates,
            effects,
            effect_events,
        });
        agent.publish_live_context();
        agent.notify_state();
        agent
    }

    /// 返回状态变更通知；通知只携带 revision，完整状态通过 [`Self::snapshot`] 读取。
    pub fn subscribe(&self) -> watch::Receiver<u64> {
        self.state_updates.subscribe()
    }

    /// 返回需要宿主可靠处理的本地效果流。
    pub fn effects(&self) -> async_channel::Receiver<crate::AgentEffect> {
        self.effect_events.clone()
    }

    pub fn snapshot(&self) -> AgentSnapshot {
        let runtime = self.runtime.read().clone();
        let state = self.state.lock();
        AgentSnapshot {
            revision: self.state_revision.load(Ordering::Acquire),
            active_persona: runtime.active_persona,
            language: runtime.language,
            messages: state.session.messages().iter().cloned().collect(),
            status: state.status.clone(),
            reply_message_id: state.reply_message_id,
            streaming: state.session.active_response_id().is_some(),
            switching_memory: state.switching_memory,
            shutting_down: state.shutting_down,
            pending_voice: state
                .pending_voice
                .as_ref()
                .map(|pending| pending.utterance_id),
        }
    }

    /// 返回宿主构造下一次输入时必须携带的生命周期 revision。
    pub fn request_revision(&self) -> u64 {
        self.state.lock().request_revision
    }

    pub fn memory(&self) -> AgentMemory {
        self.runtime.read().memory.clone()
    }

    /// 按宿主单调 revision 原子应用一组直接运行时组件。
    ///
    /// 相同人格只短锁替换配置；人格变化时立即使旧请求失效并进入切换状态，保存和恢复完成后
    /// 仅允许最新 revision 安装结果。该接口不依赖宿主配置类型。
    #[expect(
        clippy::too_many_arguments,
        reason = "接口显式接收 Client、模型、提示词和记忆等可独立组合组件"
    )]
    pub async fn apply_configuration(
        &self,
        configuration_revision: u64,
        client: Client,
        model: Option<ModelIden>,
        options: Option<ChatOptions>,
        system_prompt: impl Into<String>,
        memory: AgentMemory,
        active_persona: impl Into<String>,
        limits: ChatLimits,
        language: AppLanguage,
    ) -> Result<bool, AgentError> {
        let active_persona = active_persona.into();
        let system_prompt: Arc<str> = Arc::from(system_prompt.into());
        ChatSession::new(limits)
            .map(|_| ())
            .map_err(|error| AgentError::Session(error.to_string()))?;

        let old_save = {
            let mut runtime = self.runtime.write();
            let mut state = self.state.lock();
            if configuration_revision <= runtime.configuration_revision
                || configuration_revision < state.pending_configuration_revision
                || (configuration_revision == state.pending_configuration_revision
                    && state.switching_memory)
            {
                return Ok(false);
            }
            if state.shutting_down {
                return Err(AgentError::ShuttingDown);
            }
            state.pending_configuration_revision = configuration_revision;
            let persona_changed = runtime.active_persona != active_persona;
            abort_active_request(&mut state, "configuration_changed");
            state.session.interrupt_active_response();
            state.pending_voice = None;
            if !persona_changed {
                state
                    .session
                    .update_limits(limits)
                    .expect("聊天限制已经通过 ChatSession::new 校验");
                state.switching_memory = false;
                runtime.revision = next_revision(runtime.revision);
                runtime.configuration_revision = configuration_revision;
                runtime.client = client;
                runtime.model = model;
                runtime.options = options;
                runtime.system_prompt = system_prompt;
                runtime.memory = memory;
                runtime.active_persona = active_persona;
                runtime.limits = limits;
                runtime.language = language;
                drop(state);
                drop(runtime);
                self.publish_live_context();
                self.notify_state();
                self.persist(false);
                return Ok(true);
            }

            state.switching_memory = true;
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

        if let Some((store, snapshot, operation)) = old_save
            && let Err(error) = store.save_reserved(snapshot, operation).await
        {
            let recovered = {
                let runtime = self.runtime.read();
                let mut state = self.state.lock();
                let latest = state.pending_configuration_revision == configuration_revision
                    && runtime.configuration_revision < configuration_revision;
                if latest {
                    state.switching_memory = false;
                }
                drop(state);
                drop(runtime);
                latest
            };
            if recovered {
                self.publish_live_context();
                self.notify_state();
            }
            return Err(AgentError::Persistence(error.to_string()));
        }
        let (session, store, status) =
            load_persona_session(&memory, &active_persona, limits, language).await;
        {
            let mut runtime = self.runtime.write();
            let mut state = self.state.lock();
            if state.pending_configuration_revision != configuration_revision
                || !state.switching_memory
                || configuration_revision <= runtime.configuration_revision
            {
                return Ok(false);
            }
            if state.shutting_down {
                state.switching_memory = false;
                drop(state);
                drop(runtime);
                self.notify_state();
                return Err(AgentError::ShuttingDown);
            }
            runtime.revision = next_revision(runtime.revision);
            runtime.configuration_revision = configuration_revision;
            runtime.client = client;
            runtime.model = model;
            runtime.options = options;
            runtime.system_prompt = system_prompt;
            runtime.memory = memory;
            runtime.active_persona = active_persona;
            runtime.limits = limits;
            runtime.language = language;
            state.session = session;
            state.store = store;
            state.persist_revision = state.store.latest_revision();
            state.last_persist = Instant::now();
            state.status = status;
            state.reply_message_id = None;
            state.active_request = None;
            state.pending_voice = None;
            state.switching_memory = false;
        }
        self.publish_live_context();
        self.notify_state();
        Ok(true)
    }
}

async fn load_persona_session(
    memory: &AgentMemory,
    persona: &str,
    limits: ChatLimits,
    language: AppLanguage,
) -> (ChatSession, Arc<ChatSessionStore>, Option<String>) {
    let Some(persistence) = memory.persistence() else {
        return (
            ChatSession::new(limits).unwrap_or_default(),
            ChatSessionStore::unavailable(),
            None,
        );
    };
    match ChatSessionStore::load_with_lock(
        persistence.clone(),
        persona,
        limits,
        memory.session_document_lock(),
    )
    .await
    {
        Ok((session, store)) => (session, store, None),
        Err(error) if error.is_unsupported_document() => (
            ChatSession::new(limits).unwrap_or_default(),
            ChatSessionStore::unavailable(),
            Some(
                rust_i18n::t!("chat.error.unsupported_snapshot", locale = language.id())
                    .to_string(),
            ),
        ),
        Err(error) if error.is_invalid_document() => (
            ChatSession::new(limits).unwrap_or_default(),
            ChatSessionStore::empty_with_lock(persistence, persona, memory.session_document_lock()),
            Some(rust_i18n::t!("chat.error.invalid_snapshot", locale = language.id()).to_string()),
        ),
        Err(error) => {
            log::error!(
                "event=chat_session_restore_failed persistence_disabled=true error_kind={}",
                error.diagnostic_kind()
            );
            (
                ChatSession::new(limits).unwrap_or_default(),
                ChatSessionStore::unavailable(),
                Some(
                    rust_i18n::t!(
                        "chat.persistence_unavailable",
                        locale = language.id(),
                        error = error.diagnostic_kind()
                    )
                    .to_string(),
                ),
            )
        }
    }
}
