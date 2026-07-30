//! 提供不依赖窗口框架与平台实现的 Agent 核心能力。
//!
//! 宿主通过显式配置快照、持久化回调和窄能力对象组合运行时适配。

use std::{
    error::Error,
    fmt,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};

use futures::{
    StreamExt as _,
    channel::mpsc,
    future::{AbortHandle, Abortable},
};
pub use genai::{Client, ModelIden, chat::ChatOptions};
use parking_lot::{Mutex, RwLock};
use rust_i18n::t;
use tokio::sync::watch;

rust_i18n::i18n!("locales", fallback = "en", minify_key = true);

pub mod config;
pub mod media;
pub mod memory;
pub mod persistence;
mod provider;
mod session;
mod store;
pub mod tools;

pub use memory::AgentMemory;
pub use provider::{ScreenshotCapability, client_from_model, model_and_options_from_config};
pub use session::{
    ChatLimits, ChatMessage, ChatMessageState, ChatRole, MAX_SESSION_TEXT_BYTES, ResponseId,
    context_message_tokens,
};

use config::{
    AppLanguage, DEFAULT_MAX_OUTPUT_TOKENS, LlmSettings, MODEL_CONTEXT_RESERVE_TOKENS,
    PersonaConfig,
};
use media::ImageAttachment;
use provider::{ChatServiceRequest, ChatStreamEvent, stream_with_client};
use session::{ChatSession, estimate_text_tokens};
use store::{ChatSessionStore, delete_persona_session_reserved, mutate_persona_session_reserved};
use tools::{AgentOutfitRequest, OutfitOption};

const STREAM_CHANNEL_CAPACITY: usize = 16;
const PERSIST_INTERVAL: Duration = Duration::from_secs(3);
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
    runtime_revision: u64,
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

    pub const fn runtime_revision(&self) -> u64 {
        self.runtime_revision
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
    switching_memory: bool,
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
    runtime: RwLock<AgentRuntime>,
    state: Mutex<AgentState>,
    state_revision: AtomicU64,
    state_updates: watch::Sender<u64>,
    effects: async_channel::Sender<AgentEffect>,
    effect_events: async_channel::Receiver<AgentEffect>,
}

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
            runtime: RwLock::new(AgentRuntime {
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
            state: Mutex::new(AgentState {
                session: ChatSession::new(limits).unwrap_or_default(),
                store: ChatSessionStore::unavailable(),
                persist_revision: 0,
                last_persist: Instant::now(),
                status: initial_status,
                reply_message_id: None,
                active_request: None,
                switching_memory: false,
                shutting_down: false,
                pending_voice: None,
            }),
            state_revision: AtomicU64::new(0),
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
            runtime: RwLock::new(AgentRuntime {
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
            state: Mutex::new(AgentState {
                persist_revision: store.latest_revision(),
                session,
                store,
                last_persist: Instant::now(),
                status: restore_status.or(initial_status),
                reply_message_id: None,
                active_request: None,
                switching_memory: false,
                shutting_down: false,
                pending_voice: None,
            }),
            state_revision: AtomicU64::new(0),
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
    pub fn effects(&self) -> async_channel::Receiver<AgentEffect> {
        self.effect_events.clone()
    }

    pub fn snapshot(&self) -> AgentSnapshot {
        let runtime = self.runtime.read().clone();
        let state = self.state.lock();
        AgentSnapshot {
            revision: self.state_revision.load(Ordering::Acquire),
            runtime_revision: runtime.revision,
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

    pub fn memory(&self) -> AgentMemory {
        self.runtime.read().memory.clone()
    }

    pub fn set_client(&self, client: Client) {
        let mut runtime = self.runtime.write();
        runtime.client = client;
        runtime.revision = next_revision(runtime.revision);
        drop(runtime);
        self.notify_state();
    }

    pub fn set_model(&self, model: Option<ModelIden>) {
        let mut runtime = self.runtime.write();
        runtime.model = model;
        runtime.revision = next_revision(runtime.revision);
        drop(runtime);
        self.notify_state();
    }

    pub fn set_chat_options(&self, options: Option<ChatOptions>) {
        let mut runtime = self.runtime.write();
        runtime.options = options;
        runtime.revision = next_revision(runtime.revision);
        drop(runtime);
        self.notify_state();
    }

    pub fn set_system_prompt(&self, system_prompt: impl Into<String>) {
        let mut runtime = self.runtime.write();
        runtime.system_prompt = Arc::from(system_prompt.into());
        runtime.revision = next_revision(runtime.revision);
        drop(runtime);
        self.notify_state();
    }

    /// 原子替换 Provider、模型和提示词；已启动请求继续使用自己的旧快照。
    pub fn replace_runtime(
        &self,
        client: Client,
        model: Option<ModelIden>,
        options: Option<ChatOptions>,
        system_prompt: impl Into<String>,
        language: AppLanguage,
    ) {
        let mut runtime = self.runtime.write();
        runtime.client = client;
        runtime.model = model;
        runtime.options = options;
        runtime.system_prompt = Arc::from(system_prompt.into());
        runtime.language = language;
        runtime.revision = next_revision(runtime.revision);
        drop(runtime);
        self.notify_state();
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
        let (persona_changed, old_save) = {
            let mut runtime = self.runtime.write();
            if configuration_revision <= runtime.configuration_revision {
                return Ok(false);
            }
            let persona_changed = runtime.active_persona != active_persona;
            runtime.configuration_revision = configuration_revision;
            runtime.revision = next_revision(runtime.revision);
            runtime.client = client;
            runtime.model = model;
            runtime.options = options;
            runtime.system_prompt = system_prompt;
            runtime.language = language;
            let mut state = self.state.lock();
            if state.shutting_down {
                return Err(AgentError::ShuttingDown);
            }
            abort_active_request(&mut state, "configuration_changed");
            state.session.interrupt_active_response();
            state.pending_voice = None;
            if !persona_changed {
                runtime.memory = memory.clone();
                runtime.limits = limits;
                state
                    .session
                    .update_limits(limits)
                    .map_err(|error| AgentError::Session(error.to_string()))?;
                state.switching_memory = false;
                (false, None)
            } else {
                state.switching_memory = true;
                state.persist_revision = next_revision(state.persist_revision);
                let operation = state.store.reserve_document_operation();
                let save = state.store.is_available().then(|| {
                    (
                        state.store.clone(),
                        state.session.snapshot(state.persist_revision),
                        operation,
                    )
                });
                (true, save)
            }
        };
        self.publish_live_context();
        self.notify_state();
        if !persona_changed {
            self.persist(false);
            return Ok(true);
        }

        if let Some((store, snapshot, operation)) = old_save {
            store
                .save_reserved(snapshot, operation)
                .await
                .map_err(|error| AgentError::Persistence(error.to_string()))?;
        }
        let (session, store, status) =
            load_persona_session(&memory, &active_persona, limits, language).await;
        {
            let mut runtime = self.runtime.write();
            if runtime.configuration_revision != configuration_revision {
                return Ok(false);
            }
            runtime.memory = memory;
            runtime.active_persona = active_persona;
            runtime.limits = limits;
            let mut state = self.state.lock();
            state.session = session;
            state.store = store;
            state.persist_revision = state.store.latest_revision();
            state.last_persist = Instant::now();
            state.status = status;
            state.reply_message_id = None;
            state.active_request = None;
            state.switching_memory = false;
        }
        self.publish_live_context();
        self.notify_state();
        Ok(true)
    }

    /// 切换活动人格记忆；旧会话先完成有序保存，迟到 Provider 事件由 runtime revision 拒绝。
    pub async fn replace_memory(
        self: &Arc<Self>,
        memory: AgentMemory,
        active_persona: impl Into<String>,
        limits: ChatLimits,
        language: AppLanguage,
    ) -> Result<(), AgentError> {
        let active_persona = active_persona.into();
        let old_save = {
            let runtime = self.runtime.read().clone();
            let mut state = self.state.lock();
            if state.shutting_down {
                return Err(AgentError::ShuttingDown);
            }
            abort_active_request(&mut state, "memory_replaced");
            state.session.interrupt_active_response();
            state.pending_voice = None;
            state.switching_memory = true;
            state.persist_revision = next_revision(state.persist_revision);
            let operation = state.store.reserve_document_operation();
            let save = state.store.is_available().then(|| {
                (
                    state.store.clone(),
                    state.session.snapshot(state.persist_revision),
                    operation,
                )
            });
            drop(state);
            self.publish_live_context_for(&runtime);
            save
        };
        self.notify_state();

        if let Some((store, snapshot, operation)) = old_save {
            store
                .save_reserved(snapshot, operation)
                .await
                .map_err(|error| AgentError::Persistence(error.to_string()))?;
        }

        let (session, store, status) =
            load_persona_session(&memory, &active_persona, limits, language).await;
        {
            let mut runtime = self.runtime.write();
            runtime.memory = memory;
            runtime.active_persona = active_persona;
            runtime.limits = limits;
            runtime.language = language;
            runtime.revision = next_revision(runtime.revision);
            let mut state = self.state.lock();
            state.session = session;
            state.store = store;
            state.persist_revision = state.store.latest_revision();
            state.last_persist = Instant::now();
            state.status = status;
            state.reply_message_id = None;
            state.active_request = None;
            state.switching_memory = false;
        }
        self.publish_live_context();
        self.notify_state();
        Ok(())
    }

    /// 更新当前人格的上下文限制；必要时裁剪最早的完整轮次。
    pub fn set_limits(&self, limits: ChatLimits) -> Result<(), AgentError> {
        {
            let mut runtime = self.runtime.write();
            runtime.limits = limits;
            runtime.revision = next_revision(runtime.revision);
            self.state
                .lock()
                .session
                .update_limits(limits)
                .map_err(|error| AgentError::Session(error.to_string()))?;
        }
        self.publish_live_context();
        self.persist(false);
        self.notify_state();
        Ok(())
    }

    /// 创建并执行一轮完整请求，直到收到终态、取消或网络任务结束。
    pub async fn send(self: Arc<Self>, input: AgentInput) -> Result<ResponseId, AgentError> {
        let runtime = self.runtime.read().clone();
        let model = runtime.model.clone().ok_or(AgentError::ModelUnavailable)?;
        if runtime.limits.max_request_tokens < 8 {
            return Err(AgentError::ContextWindowExhausted);
        }
        let (response_id, request, abort_registration) = {
            let mut state = self.state.lock();
            if state.shutting_down {
                return Err(AgentError::ShuttingDown);
            }
            if state.switching_memory {
                return Err(AgentError::MemorySwitching);
            }
            let started = state
                .session
                .start_turn_with_image(input.text, input.image, input.language)
                .map_err(|error| AgentError::Session(error.localized_message(input.language)))?;
            state.pending_voice = None;
            let response_id = started.response_id;
            let request = ChatServiceRequest {
                model,
                options: runtime.options.clone(),
                system_prompt: runtime.system_prompt.to_string(),
                messages: started.context,
                screenshot_capability: input.screenshot_capability,
                outfits: input.outfits,
                outfit_revision: input.outfit_revision,
                language: input.language,
            };
            let (abort, abort_registration) = AbortHandle::new_pair();
            state.active_request = Some(ActiveRequest {
                response_id,
                runtime_revision: runtime.revision,
                abort,
                started_at: Instant::now(),
            });
            state.status = None;
            state.reply_message_id = state.session.messages().back().map(ChatMessage::id);
            (response_id, request, abort_registration)
        };
        self.publish_live_context_for(&runtime);
        self.persist(true);
        self.notify_state();

        let (sender, mut receiver) = mpsc::channel(STREAM_CHANNEL_CAPACITY);
        let provider_task = tokio::spawn(Abortable::new(
            stream_with_client(runtime.client.clone(), request, sender),
            abort_registration,
        ));
        while let Some(event) = receiver.next().await {
            if !self.apply_stream_event(response_id, runtime.revision, event, input.language) {
                break;
            }
        }
        drop(receiver);

        let provider_result = provider_task.await;
        let mut terminal_failure = None;
        {
            let mut state = self.state.lock();
            if request_is_current(&state, response_id, runtime.revision)
                && state.session.active_response_id() == Some(response_id)
            {
                let failure = match provider_result {
                    Ok(Ok(())) => t!("chat.stream_ended", locale = input.language.id()).to_string(),
                    Ok(Err(_)) => return Ok(response_id),
                    Err(error) => t!(
                        "chat.task_ended",
                        locale = input.language.id(),
                        kind = if error.is_cancelled() {
                            "cancelled"
                        } else {
                            "panic"
                        }
                    )
                    .to_string(),
                };
                state.session.fail_response(response_id, failure.clone());
                state.status = Some(failure.clone());
                state.active_request = None;
                terminal_failure = Some(failure);
            }
        }
        if terminal_failure.is_some() {
            self.persist(true);
            self.notify_state();
        }
        Ok(response_id)
    }

    /// 取消当前 Provider 请求并把助手消息转换为明确终态。
    pub fn cancel(&self) -> bool {
        let cancelled = {
            let mut state = self.state.lock();
            let Some(response_id) = state.session.active_response_id() else {
                return false;
            };
            abort_active_request(&mut state, "user_stop");
            let cancelled = state.session.cancel_response(response_id);
            if cancelled {
                state.status = None;
            }
            cancelled
        };
        if cancelled {
            self.persist(true);
            self.notify_state();
        }
        cancelled
    }

    /// 在 VAD 确认新语音开始时打断当前回复，并为下一轮保留语音打断标记。
    pub fn interrupt_by_voice(&self) -> bool {
        let interrupted = {
            let mut state = self.state.lock();
            let Some(response_id) = state.session.active_response_id() else {
                return false;
            };
            abort_active_request(&mut state, "voice_interruption");
            state.session.interrupt_response_by_voice(response_id)
        };
        if interrupted {
            self.persist(true);
            self.notify_state();
        }
        interrupted
    }

    /// 登记最新语音 utterance，并在存在活动回复时按语音语义打断。
    pub fn voice_started(&self, utterance_id: u64, language: AppLanguage) -> bool {
        let runtime = self.runtime.read().clone();
        let interrupted = {
            let mut state = self.state.lock();
            if state.shutting_down
                || state
                    .pending_voice
                    .as_ref()
                    .is_some_and(|pending| pending.utterance_id >= utterance_id)
            {
                return false;
            }
            state.pending_voice = Some(PendingVoice {
                utterance_id,
                runtime_revision: runtime.revision,
                persona: runtime.active_persona,
                language,
            });
            let Some(response_id) = state.session.active_response_id() else {
                return true;
            };
            abort_active_request(&mut state, "voice_interruption");
            state.session.interrupt_response_by_voice(response_id)
        };
        if interrupted {
            self.persist(true);
        }
        self.notify_state();
        true
    }

    /// 消费仍属于当前 runtime 和人格的转写；失效结果返回 `None`。
    pub fn take_voice_transcript(&self, utterance_id: u64) -> Option<AppLanguage> {
        let runtime = self.runtime.read().clone();
        let mut state = self.state.lock();
        if !state
            .pending_voice
            .as_ref()
            .is_some_and(|pending| pending.utterance_id == utterance_id)
        {
            return None;
        }
        let pending = state.pending_voice.take()?;
        (pending.utterance_id == utterance_id
            && pending.runtime_revision == runtime.revision
            && pending.persona == runtime.active_persona
            && !state.switching_memory
            && !state.shutting_down
            && state.session.active_response_id().is_none())
        .then_some(pending.language)
    }

    pub fn cancel_voice(&self, utterance_id: u64) {
        let mut state = self.state.lock();
        if state
            .pending_voice
            .as_ref()
            .is_some_and(|pending| pending.utterance_id == utterance_id)
        {
            state.pending_voice = None;
        }
    }

    pub fn cancel_pending_voice(&self) {
        self.state.lock().pending_voice = None;
    }

    /// 设置只用于宿主展示的状态文本，不会进入 Provider 上下文。
    pub fn set_status(&self, status: Option<String>) {
        let mut state = self.state.lock();
        state.status = status;
        if state.status.is_some() {
            state.reply_message_id = None;
        }
        drop(state);
        self.notify_state();
    }

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

    fn apply_stream_event(
        &self,
        response_id: ResponseId,
        runtime_revision: u64,
        event: ChatStreamEvent,
        language: AppLanguage,
    ) -> bool {
        if let ChatStreamEvent::ChangeOutfit(request) = event {
            if self
                .effects
                .try_send(AgentEffect::ChangeOutfit(request.clone()))
                .is_err()
            {
                request.complete(false);
            }
            return true;
        }
        let (keep_receiving, terminal) = {
            let mut state = self.state.lock();
            if !request_is_current(&state, response_id, runtime_revision)
                || state.session.active_response_id() != Some(response_id)
            {
                return false;
            }
            match event {
                ChatStreamEvent::Delta(chunk) => {
                    if state.session.append_response(response_id, &chunk).is_err() {
                        let failure =
                            t!("chat.reply_too_large", locale = language.id()).to_string();
                        state.session.fail_response(response_id, failure.clone());
                        state.status = Some(failure);
                        state.active_request = None;
                        (false, true)
                    } else {
                        (true, false)
                    }
                }
                ChatStreamEvent::Trace(trace) => {
                    let _ = state.session.attach_response_trace(response_id, trace);
                    (true, false)
                }
                ChatStreamEvent::Finished => {
                    if !state.session.finish_response(response_id) {
                        return false;
                    }
                    state.active_request = None;
                    (false, true)
                }
                ChatStreamEvent::Failed(message) => {
                    if !state.session.fail_response(response_id, message.clone()) {
                        return false;
                    }
                    state.status = Some(message);
                    state.active_request = None;
                    (false, true)
                }
                ChatStreamEvent::ChangeOutfit(_) => unreachable!("换装事件已在加锁前处理"),
            }
        };
        self.publish_live_context();
        self.persist(terminal);
        self.notify_state();
        keep_receiving
    }

    fn persist(&self, force: bool) {
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
            state.last_persist = Instant::now();
            let operation = state.store.reserve_document_operation();
            (
                state.store.clone(),
                state.session.snapshot(state.persist_revision),
                operation,
            )
        };
        tokio::spawn(async move {
            if let Err(error) = save.0.save_reserved(save.1, save.2).await {
                log::error!("保存聊天会话失败：error_kind={}", error.diagnostic_kind());
            }
        });
    }

    fn publish_live_context(&self) {
        let runtime = self.runtime.read().clone();
        self.publish_live_context_for(&runtime);
    }

    fn publish_live_context_for(&self, runtime: &AgentRuntime) {
        let state = self.state.lock();
        runtime.memory.live_context_usage().publish(
            &runtime.active_persona,
            state.session.usage(),
            state.session.editable_messages(),
        );
    }

    fn notify_state(&self) {
        let revision = self
            .state_revision
            .fetch_add(1, Ordering::AcqRel)
            .wrapping_add(1)
            .max(1);
        self.state_updates.send_replace(revision);
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
            Some(t!("chat.error.unsupported_snapshot", locale = language.id()).to_string()),
        ),
        Err(error) if error.is_invalid_document() => (
            ChatSession::new(limits).unwrap_or_default(),
            ChatSessionStore::empty_with_lock(persistence, persona, memory.session_document_lock()),
            Some(t!("chat.error.invalid_snapshot", locale = language.id()).to_string()),
        ),
        Err(error) => {
            log::error!(
                "恢复聊天会话失败，当前人格持久化已禁用：error_kind={}",
                error.diagnostic_kind()
            );
            (
                ChatSession::new(limits).unwrap_or_default(),
                ChatSessionStore::unavailable(),
                Some(
                    t!(
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

fn request_is_current(state: &AgentState, response_id: ResponseId, revision: u64) -> bool {
    state.active_request.as_ref().is_some_and(|request| {
        request.response_id == response_id && request.runtime_revision == revision
    })
}

fn abort_active_request(state: &mut AgentState, reason: &'static str) {
    if let Some(request) = state.active_request.take() {
        log::debug!(
            "Agent 请求已取消：response_id={}, reason={reason}, elapsed_ms={}",
            request.response_id.get(),
            request.started_at.elapsed().as_millis()
        );
        request.abort.abort();
    }
}

async fn persist_reserved(
    save: Option<(
        Arc<ChatSessionStore>,
        session::ChatSessionSnapshot,
        store::SessionOperationReservation,
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

fn next_revision(revision: u64) -> u64 {
    revision.wrapping_add(1).max(1)
}

/// Agent 操作在进入 Provider 前或会话切换期间失败。
#[derive(Debug)]
pub enum AgentError {
    ModelUnavailable,
    ContextWindowExhausted,
    MemorySwitching,
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
            Self::ShuttingDown => write!(formatter, "Agent 正在关闭"),
            Self::Session(message) => write!(formatter, "Agent 会话操作失败：{message}"),
            Self::Persistence(message) => write!(formatter, "Agent 持久化失败：{message}"),
        }
    }
}

impl Error for AgentError {}

#[cfg(test)]
mod tests;
