//! 校验网络 endpoint、本地模型路径和 Whisper 语言输入。

use std::path::PathBuf;

use rust_i18n::t;
use url::{Host, Url};

use super::{
    AgentConfigError, AppLanguage, LlmProvider, WHISPER_LANGUAGE_CODES, invalid, llm_provider_id,
};

const MAX_ENDPOINT_BYTES: usize = 2_048;
const MAX_LOCAL_MODEL_PATH_BYTES: usize = 4 * 1024;

/// 规范化使用 HTTP 或 HTTPS 的自定义 Provider endpoint。
pub fn normalize_endpoint(
    provider: LlmProvider,
    endpoint: Option<&str>,
    language: AppLanguage,
) -> Result<Option<String>, AgentConfigError> {
    let Some(endpoint) = endpoint.map(str::trim).filter(|value| !value.is_empty()) else {
        return Ok(None);
    };
    if endpoint.len() > MAX_ENDPOINT_BYTES {
        return Err(invalid(
            t!(
                "llm.error.too_long",
                locale = language.id(),
                field = t!("llm.endpoint", locale = language.id()),
                max = MAX_ENDPOINT_BYTES
            )
            .to_string(),
        ));
    }
    if matches!(provider, LlmProvider::Zai | LlmProvider::Baidu) {
        let provider_name = match provider {
            LlmProvider::Zai => "ZAI",
            LlmProvider::Baidu => "Baidu",
            _ => llm_provider_id(provider),
        };
        return Err(invalid(
            t!(
                "llm.error.endpoint_unsupported",
                locale = language.id(),
                provider = provider_name
            )
            .to_string(),
        ));
    }
    let mut url = Url::parse(endpoint).map_err(|error| {
        invalid(
            t!(
                "llm.error.endpoint_invalid",
                locale = language.id(),
                error = error
            )
            .to_string(),
        )
    })?;
    if url.host().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err(invalid(
            t!("llm.error.endpoint_requirements", locale = language.id()).to_string(),
        ));
    }
    if !matches!(url.scheme(), "http" | "https") {
        return Err(invalid(
            t!("llm.error.endpoint_scheme", locale = language.id()).to_string(),
        ));
    }
    let path = url.path().trim_end_matches('/');
    let normalized_path = if path.is_empty() {
        "/".to_owned()
    } else {
        format!("{path}/")
    };
    url.set_path(&normalized_path);
    Ok(Some(url.into()))
}

pub(super) fn normalize_speech_endpoint(
    endpoint: Option<&str>,
    language: AppLanguage,
) -> Result<Option<String>, AgentConfigError> {
    normalize_endpoint(LlmProvider::OpenAI, endpoint, language)
}

pub(super) fn normalize_doubao_endpoint(
    endpoint: Option<&str>,
    language: AppLanguage,
) -> Result<Option<String>, AgentConfigError> {
    let Some(endpoint) = endpoint.map(str::trim).filter(|value| !value.is_empty()) else {
        return Ok(None);
    };
    if endpoint.len() > MAX_ENDPOINT_BYTES {
        return Err(invalid(
            t!(
                "llm.error.too_long",
                locale = language.id(),
                field = t!("llm.endpoint", locale = language.id()),
                max = MAX_ENDPOINT_BYTES
            )
            .to_string(),
        ));
    }
    let url = Url::parse(endpoint).map_err(|error| {
        invalid(
            t!(
                "llm.error.endpoint_invalid",
                locale = language.id(),
                error = error
            )
            .to_string(),
        )
    })?;
    if url.host().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err(invalid(
            t!("llm.error.endpoint_requirements", locale = language.id()).to_string(),
        ));
    }
    if !matches!(url.scheme(), "ws" | "wss") {
        return Err(invalid(
            t!(
                "llm.error.websocket_endpoint_scheme",
                locale = language.id()
            )
            .to_string(),
        ));
    }
    Ok(Some(url.into()))
}

/// 判断 endpoint 是否为明文回环传输，供网络层选择代理策略。
pub(crate) fn endpoint_is_plaintext_loopback(endpoint: &str) -> bool {
    Url::parse(endpoint)
        .is_ok_and(|url| matches!(url.scheme(), "http" | "ws") && endpoint_host_is_loopback(&url))
}

fn endpoint_host_is_loopback(url: &Url) -> bool {
    match url.host() {
        Some(Host::Domain(host)) => host.eq_ignore_ascii_case("localhost"),
        Some(Host::Ipv4(address)) => address.is_loopback(),
        Some(Host::Ipv6(address)) => address.is_loopback(),
        None => false,
    }
}

pub(super) fn normalize_local_model_path(
    path: Option<PathBuf>,
    language: AppLanguage,
) -> Result<Option<PathBuf>, AgentConfigError> {
    let Some(path) = path else {
        return Ok(None);
    };
    let Some(value) = path.to_str() else {
        return Err(invalid(
            t!("llm.error.local_path_utf8", locale = language.id()).to_string(),
        ));
    };
    let value = value.trim();
    if value.is_empty() {
        return Ok(None);
    }
    if value.len() > MAX_LOCAL_MODEL_PATH_BYTES {
        return Err(invalid(
            t!(
                "llm.error.too_long",
                locale = language.id(),
                field = t!("llm.local_model_path", locale = language.id()),
                max = MAX_LOCAL_MODEL_PATH_BYTES
            )
            .to_string(),
        ));
    }
    if value.contains('\0') {
        return Err(invalid(
            t!("llm.error.local_path_invalid", locale = language.id()).to_string(),
        ));
    }
    Ok(Some(PathBuf::from(value)))
}

pub(super) fn normalize_whisper_language(
    language_code: Option<String>,
    language: AppLanguage,
) -> Result<Option<String>, AgentConfigError> {
    let Some(language_code) = language_code else {
        return Ok(None);
    };
    let language_code = language_code.trim();
    if language_code.is_empty() {
        return Ok(None);
    }
    if !WHISPER_LANGUAGE_CODES.contains(&language_code) {
        return Err(invalid(
            t!(
                "llm.error.whisper_language",
                locale = language.id(),
                language = language_code
            )
            .to_string(),
        ));
    }
    Ok(Some(language_code.to_owned()))
}
