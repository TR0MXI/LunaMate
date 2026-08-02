//! 构造 Provider client，并翻译模型高级请求选项。

use std::time::Duration;

use genai::{
    Client,
    chat::ChatOptions,
    resolver::{AuthData, Endpoint},
};

use crate::{
    config::{LlmAdvancedOptions, LlmModelConfig},
    transport::provider_http_client,
};

use super::TOTAL_RESPONSE_TIMEOUT;

const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const READ_TIMEOUT: Duration = Duration::from_secs(45);

/// 把供应商高级参数翻译为 `genai` 请求选项；全部未设置时返回 `None` 以沿用 Provider 默认值。
pub(crate) fn base_chat_options(advanced: &LlmAdvancedOptions) -> Option<ChatOptions> {
    if advanced.reasoning_effort.is_none()
        && advanced.max_output_tokens.is_none()
        && advanced.temperature.is_none()
        && advanced.top_p.is_none()
    {
        return None;
    }

    let mut options = ChatOptions::default();
    if let Some(effort) = advanced.reasoning_effort.clone() {
        options = options.with_reasoning_effort(effort);
    }
    if let Some(tokens) = advanced.max_output_tokens {
        options = options.with_max_tokens(tokens);
    }
    if let Some(temperature) = advanced.temperature {
        options = options.with_temperature(temperature);
    }
    if let Some(top_p) = advanced.top_p {
        options = options.with_top_p(top_p);
    }
    Some(options)
}

/// 构建 Provider client；内部会按 endpoint 同步加载代理策略与 CA 存储，只能在后台任务中调用。
pub(super) fn build_client(model: &LlmModelConfig) -> Client {
    // endpoint 已经过配置校验；Rustls、代理与 timeout 均由 reqwest 延迟用于请求。
    let http_client = provider_http_client(
        model.endpoint.as_deref(),
        CONNECT_TIMEOUT,
        READ_TIMEOUT,
        TOTAL_RESPONSE_TIMEOUT,
    )
    .expect("固定的 Provider HTTP client 配置应可构建");
    let mut builder = Client::builder().with_reqwest(http_client);
    let Some(provider) = model.provider.genai() else {
        return builder.build();
    };
    let auth = auth_data(model);
    builder = builder
        .with_adapter_kind(provider)
        .with_auth_resolver_fn(move |_| Ok(Some(auth.clone())));
    if let Some(endpoint) = &model.endpoint {
        let endpoint = Endpoint::from_owned(endpoint.clone());
        builder =
            builder.with_service_target_resolver_fn(move |mut target: genai::ServiceTarget| {
                target.endpoint = endpoint.clone();
                Ok(target)
            });
    }
    builder.build()
}

pub(crate) fn auth_data(model: &LlmModelConfig) -> AuthData {
    model
        .api_key
        .clone()
        .map(AuthData::from_single)
        .unwrap_or(AuthData::None)
}
