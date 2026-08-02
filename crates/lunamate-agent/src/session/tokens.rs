//! 估算文本与图片上下文预算，并按完整历史轮次裁剪请求。

use crate::media::ImageAttachment;

use super::{
    ChatContextMessage, ChatRole, IMAGE_CONTEXT_TOKENS, MISSING_IMAGE_CONTEXT_TOKENS,
    TOKENS_PER_MESSAGE,
};

/// 估算跨 Provider 文本 token 数。模型词表不可用时按 UTF-8 密度与词法片段取较大值，
/// 避免中文、emoji 或长 ASCII 文本被明显低估。
pub(crate) fn estimate_text_tokens(text: &str) -> usize {
    let bytes_estimate = text.len().div_ceil(3);
    let mut lexical_estimate = 0_usize;
    let mut ascii_word = 0_usize;
    for character in text.chars() {
        if character.is_ascii_alphanumeric() || character == '_' {
            ascii_word = ascii_word.saturating_add(1);
            continue;
        }
        lexical_estimate = lexical_estimate.saturating_add(ascii_word.div_ceil(4));
        ascii_word = 0;
        if !character.is_ascii_whitespace() {
            lexical_estimate = lexical_estimate.saturating_add(if character.is_ascii() {
                1
            } else {
                character.len_utf8().div_ceil(2)
            });
        }
    }
    lexical_estimate = lexical_estimate.saturating_add(ascii_word.div_ceil(4));
    bytes_estimate.max(lexical_estimate)
}

pub fn context_message_tokens(content: &str, fixed_tokens: usize) -> usize {
    fixed_tokens.saturating_add(estimate_text_tokens(content))
}

pub(super) fn message_token_count(content: &str, image_tokens: usize) -> usize {
    context_message_tokens(content, TOKENS_PER_MESSAGE.saturating_add(image_tokens))
}

pub(super) fn image_context_tokens(image: Option<&ImageAttachment>) -> usize {
    match image {
        Some(image) if image.bytes().is_some() => IMAGE_CONTEXT_TOKENS,
        Some(_) | None => 0,
    }
}

pub(super) fn request_image_context_tokens(image: Option<&ImageAttachment>) -> usize {
    match image {
        Some(image) if image.bytes().is_some() => IMAGE_CONTEXT_TOKENS,
        Some(_) => MISSING_IMAGE_CONTEXT_TOKENS,
        None => 0,
    }
}

pub(super) fn trim_request_context(context: &mut Vec<ChatContextMessage>, maximum_tokens: usize) {
    let mut total = context.iter().fold(0_usize, |tokens, message| {
        tokens.saturating_add(message_token_count(
            &message.content,
            request_image_context_tokens(message.image.as_ref()),
        ))
    });
    while total > maximum_tokens && context.len() > 1 {
        let remove = if context.len() >= 2
            && context[0].role == ChatRole::User
            && context[1].role == ChatRole::Assistant
        {
            2
        } else {
            1
        };
        if remove >= context.len() {
            break;
        }
        for message in context.drain(..remove) {
            total = total.saturating_sub(message_token_count(
                &message.content,
                request_image_context_tokens(message.image.as_ref()),
            ));
        }
    }
}
