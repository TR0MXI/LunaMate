//! 编码 Provider 请求、图片内容和本地工具声明。

use std::collections::HashSet;

use base64::{Engine as _, engine::general_purpose::STANDARD};
use genai::chat::{ChatMessage as GenaiMessage, ChatRequest, ContentPart, MessageContent, Tool};
use rust_i18n::t;
use serde_json::json;

use crate::{
    config::{AppLanguage, LlmProvider},
    media::ImageAttachment,
    session::{ChatContextMessage, ChatRole},
    tools::OutfitOption,
};

use super::{CHANGE_OUTFIT_TOOL, SCREEN_CAPTURE_TOOL};

const MAX_OUTFIT_TOOL_OPTIONS: usize = 128;
const MAX_OUTFIT_NAME_BYTES: usize = 512;

pub(crate) const fn provider_supports_binary_and_tools(provider: LlmProvider) -> bool {
    // genai 0.6.5 的 Cohere adapter 会静默丢弃 Binary 和 tools，必须在本层拒绝降级。
    !matches!(provider, LlmProvider::Cohere)
}

pub(crate) fn outfit_tool_options(
    provider: LlmProvider,
    outfits: &[OutfitOption],
) -> &[OutfitOption] {
    if outfits.len() > 1 && provider_supports_binary_and_tools(provider) {
        outfits
    } else {
        &[]
    }
}

pub(crate) fn build_request(
    system_prompt: String,
    messages: Vec<ChatContextMessage>,
    allow_agent_screenshot: bool,
    outfits: &[OutfitOption],
    language: AppLanguage,
) -> ChatRequest {
    let messages = messages
        .into_iter()
        .map(|message| match message.role {
            ChatRole::User => match message.image.as_ref() {
                Some(image) => match image_content_part(image) {
                    Some(image) => GenaiMessage::user(MessageContent::from_parts(vec![
                        ContentPart::from_text(message.content),
                        image,
                    ])),
                    None => GenaiMessage::user(format!(
                        "{}\n\n[{}]",
                        message.content,
                        t!("chat.history_image_unavailable", locale = language.id())
                    )),
                },
                None => GenaiMessage::user(message.content),
            },
            ChatRole::Assistant => GenaiMessage::assistant(message.content),
        })
        .collect::<Vec<_>>();
    let mut chat_request = ChatRequest::from_messages(messages);
    if !system_prompt.trim().is_empty() {
        chat_request = chat_request.with_system(system_prompt);
    }
    let mut tools = Vec::with_capacity(2);
    if allow_agent_screenshot {
        tools.push(screen_capture_tool(language));
    }
    if outfits.len() > 1 {
        tools.push(change_outfit_tool(outfits, language));
    }
    if !tools.is_empty() {
        chat_request = chat_request.with_tools(tools);
    }
    chat_request
}

fn screen_capture_tool(language: AppLanguage) -> Tool {
    Tool::new(SCREEN_CAPTURE_TOOL)
        .with_description(
            t!(
                "chat.tool.screen_capture_description",
                locale = language.id()
            )
            .to_string(),
        )
        .with_schema(json!({
            "type": "object",
            "properties": {},
            "required": [],
            "additionalProperties": false
        }))
}

fn change_outfit_tool(outfits: &[OutfitOption], language: AppLanguage) -> Tool {
    let labels = outfits.iter().map(OutfitOption::label).collect::<Vec<_>>();
    Tool::new(CHANGE_OUTFIT_TOOL)
        .with_description(
            t!(
                "chat.tool.change_outfit_description",
                locale = language.id()
            )
            .to_string(),
        )
        .with_schema(json!({
            "type": "object",
            "properties": {
                "outfit": {
                    "type": "string",
                    "description": t!(
                        "chat.tool.change_outfit_argument",
                        locale = language.id()
                    ).to_string(),
                    "enum": labels
                }
            },
            "required": ["outfit"],
            "additionalProperties": false
        }))
}

pub(super) fn bounded_outfits(outfits: Vec<OutfitOption>) -> Vec<OutfitOption> {
    let mut ids = HashSet::new();
    let mut labels = HashSet::new();
    outfits
        .into_iter()
        .filter(|outfit| {
            !outfit.id().is_empty()
                && outfit.id().len() <= MAX_OUTFIT_NAME_BYTES
                && !outfit.label().is_empty()
                && outfit.label().len() <= MAX_OUTFIT_NAME_BYTES
                && ids.insert(outfit.id().to_owned())
                && labels.insert(outfit.label().to_owned())
        })
        .take(MAX_OUTFIT_TOOL_OPTIONS)
        .collect()
}

pub(super) fn screen_capture_handoff_prompt(language: AppLanguage) -> String {
    t!("chat.tool.screen_capture_handoff", locale = language.id()).to_string()
}

pub(super) fn image_content_part(image: &ImageAttachment) -> Option<ContentPart> {
    let bytes = image.bytes()?;
    Some(ContentPart::from_binary_base64(
        "image/jpeg",
        STANDARD.encode(bytes),
        Some("image.jpg".to_owned()),
    ))
}

pub(crate) fn append_stateless_capture_result(
    request: &mut ChatRequest,
    image: Option<&ImageAttachment>,
    language: AppLanguage,
) {
    let prompt = if image.is_some() {
        screen_capture_handoff_prompt(language)
    } else {
        t!(
            "chat.tool.screen_capture_unavailable",
            locale = language.id()
        )
        .to_string()
    };
    let mut parts = vec![ContentPart::from_text(format!("\n\n{prompt}"))];
    if let Some(part) = image.and_then(image_content_part) {
        parts.push(part);
    }
    if let Some(message) = request.messages.last_mut() {
        message.content.extend(parts);
    } else {
        request
            .messages
            .push(GenaiMessage::user(MessageContent::from_parts(parts)));
    }
}

pub(super) fn append_stateless_tool_results(
    request: &mut ChatRequest,
    results: &[String],
    language: AppLanguage,
) {
    if results.is_empty() {
        return;
    }
    let prompt = t!(
        "chat.tool.result_handoff",
        locale = language.id(),
        results = results.join("\n")
    )
    .to_string();
    let part = ContentPart::from_text(format!("\n\n{prompt}"));
    if let Some(message) = request.messages.last_mut() {
        message.content.extend(vec![part]);
    } else {
        request
            .messages
            .push(GenaiMessage::user(MessageContent::from_parts(vec![part])));
    }
}
