//! 将 Provider 无关的会话快照转换为 `genai` 流，并输出受限批次事件。

use std::{future::Future, pin::Pin, sync::Arc, time::Duration};

use futures::channel::mpsc;
use genai::{Client, ModelIden, chat::ChatOptions};

use crate::{
    config::{AppLanguage, LlmModelConfig},
    media::{ImageAttachment, ImageInputError},
    memory::AssistantTrace,
    session::ChatContextMessage,
    tools::{AgentOutfitRequest, OutfitOption},
};

mod client;
mod execution;
mod request;
mod stream;
mod tool_execution;

pub(super) const SCREEN_CAPTURE_TOOL: &str = "capture_screen";
pub(super) const CHANGE_OUTFIT_TOOL: &str = "change_outfit";
pub(super) const TOTAL_RESPONSE_TIMEOUT: Duration = Duration::from_secs(5 * 60);

#[cfg(test)]
pub(super) use client::{auth_data, base_chat_options};
#[cfg(test)]
pub(super) use request::{
    append_stateless_capture_result, build_request, outfit_tool_options,
    provider_supports_binary_and_tools,
};
#[cfg(test)]
pub(super) use stream::{
    FLUSH_BYTES, MAX_HANDOFF_CONTENT_BYTES, StreamOutcome, consume_stream, send_completion_events,
    send_terminal_event,
};
#[cfg(test)]
pub(super) use tool_execution::{
    execute_screenshot_tool_for_test, execute_tool_traces_for_test, outfit_argument,
    request_outfit_change,
};

/// 宿主提供的单次截图授权与捕获能力。
pub trait ScreenshotCapability: Send + Sync {
    fn is_authorized(&self) -> bool;

    fn wait_for_revocation(&self) -> Pin<Box<dyn Future<Output = ()> + Send + 'static>>;

    fn capture(
        &self,
    ) -> Pin<Box<dyn Future<Output = Result<ImageAttachment, ImageInputError>> + Send + 'static>>;
}

/// 网络任务发送给聊天实体的有界事件。
pub(crate) enum ChatStreamEvent {
    Delta(String),
    ChangeOutfit(AgentOutfitRequest),
    Trace(AssistantTrace),
    Finished,
    Failed(String),
}

/// 一次请求所需的不可变模型、提示词和上下文快照。
pub(crate) struct ChatServiceRequest {
    pub(crate) model: ModelIden,
    pub(crate) options: Option<ChatOptions>,
    pub(crate) system_prompt: String,
    pub(crate) messages: Vec<ChatContextMessage>,
    pub(crate) screenshot_capability: Option<Arc<dyn ScreenshotCapability>>,
    pub(crate) outfits: Vec<OutfitOption>,
    pub(crate) outfit_revision: u64,
    pub(crate) language: AppLanguage,
}

/// 根据当前模型连接配置构造可并发复用的 `genai` Client。
///
/// Client 本身是不可变的 `Send + Sync` 句柄；调用方需要热更新连接配置时应构造新实例并
/// 替换自己的运行时快照，而不是在网络请求期间持锁。
pub fn client_from_model(model: &LlmModelConfig) -> Client {
    client::build_client(model)
}

/// 把宿主模型配置解析为 Agent 直接持有的模型标识和单次请求选项。
pub fn model_and_options_from_config(
    model: &LlmModelConfig,
) -> Option<(ModelIden, Option<ChatOptions>)> {
    let provider = model.provider.genai()?;
    Some((
        ModelIden::new(provider, model.model.clone()),
        client::base_chat_options(&model.advanced),
    ))
}

/// 使用调用方持有的 Client 执行一次完整流式请求。
///
/// 该入口供核心 [`crate::Agent`] 直接组合 Client，避免再通过只负责缓存 Client 的后端包装。
pub(crate) async fn stream_with_client(
    client: Client,
    request: ChatServiceRequest,
    events: mpsc::Sender<ChatStreamEvent>,
) {
    execution::stream_chat(request, events, client).await;
}
