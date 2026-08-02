//! 提供供应商无关的语音合成结果，并隔离 OpenAI 与豆包协议。

use std::{error::Error, fmt, io::Read, time::Duration};

use flate2::read::GzDecoder;
use futures::{SinkExt as _, StreamExt as _};
use serde::Serialize;
use tokio_tungstenite::tungstenite::{Message, client::IntoClientRequest as _};
use uuid::Uuid;

use crate::{
    config::{LlmModelConfig, LlmProvider, ModelKind, ModelProvider},
    transport::{connect_websocket_once, provider_http_client},
};

const SAMPLE_RATE: u32 = 24_000;
const MAX_TEXT_BYTES: usize = 16 * 1024;
const MAX_AUDIO_BYTES: usize = 32 * 1024 * 1024;
const MAX_FRAME_BYTES: usize = 4 * 1024 * 1024;
const REQUEST_TIMEOUT: Duration = Duration::from_secs(75);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const OPENAI_ENDPOINT: &str = "https://api.openai.com/v1/";
const DOUBAO_ENDPOINT: &str = "wss://openspeech.bytedance.com/api/v3/tts/bidirection";

/// 供应商协议已经归一化为 24 kHz 单声道有符号 PCM。
pub struct SynthesizedAudio {
    samples: Vec<i16>,
}

impl SynthesizedAudio {
    pub const fn sample_rate(&self) -> u32 {
        SAMPLE_RATE
    }

    pub fn samples(&self) -> &[i16] {
        &self.samples
    }

    pub fn into_samples(self) -> Vec<i16> {
        self.samples
    }
}

/// 不携带凭据、合成文本或供应商响应正文的稳定语音合成错误。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SpeechSynthesisError {
    InvalidConfiguration,
    InvalidInput,
    UnsupportedProvider,
    Network,
    Timeout,
    Rejected,
    InvalidResponse,
    ResponseTooLarge,
}

impl fmt::Display for SpeechSynthesisError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::InvalidConfiguration => "语音合成模型配置无效",
            Self::InvalidInput => "待合成文本无效",
            Self::UnsupportedProvider => "当前 Provider 不支持语音合成",
            Self::Network => "语音合成网络请求失败",
            Self::Timeout => "语音合成请求超时",
            Self::Rejected => "语音合成请求被 Provider 拒绝",
            Self::InvalidResponse => "语音合成响应无效",
            Self::ResponseTooLarge => "语音合成响应超过大小限制",
        })
    }
}

impl Error for SpeechSynthesisError {}

/// 使用模型条目对应的 Provider 合成完整助手回复。
pub async fn synthesize(
    model: &LlmModelConfig,
    text: &str,
) -> Result<SynthesizedAudio, SpeechSynthesisError> {
    let text = text.trim();
    if model.kind != ModelKind::SpeechSynthesis {
        return Err(SpeechSynthesisError::InvalidConfiguration);
    }
    if text.is_empty() || text.len() > MAX_TEXT_BYTES {
        return Err(SpeechSynthesisError::InvalidInput);
    }
    let bytes = match model.provider {
        ModelProvider::Genai(LlmProvider::OpenAI) => synthesize_openai(model, text).await,
        ModelProvider::Doubao => synthesize_doubao(model, text).await,
        ModelProvider::Genai(_) | ModelProvider::LocalWhisper => {
            Err(SpeechSynthesisError::UnsupportedProvider)
        }
    }?;
    let samples = decode_pcm(bytes)?;
    Ok(SynthesizedAudio { samples })
}

#[derive(Serialize)]
struct OpenAiSpeechRequest<'a> {
    model: &'a str,
    input: &'a str,
    voice: &'a str,
    response_format: &'static str,
}

async fn synthesize_openai(
    model: &LlmModelConfig,
    text: &str,
) -> Result<Vec<u8>, SpeechSynthesisError> {
    let api_key = required(model.api_key.as_deref())?;
    let voice = required(model.voice.as_deref())?;
    let endpoint = model.endpoint.as_deref().unwrap_or(OPENAI_ENDPOINT);
    let client = provider_http_client(
        Some(endpoint),
        CONNECT_TIMEOUT,
        REQUEST_TIMEOUT,
        REQUEST_TIMEOUT,
    )
    .map_err(|_| SpeechSynthesisError::Network)?;
    let response = client
        .post(format!(
            "{}audio/speech",
            endpoint.trim_end_matches('/').to_owned() + "/"
        ))
        .bearer_auth(api_key)
        .json(&OpenAiSpeechRequest {
            model: &model.model,
            input: text,
            voice,
            response_format: "pcm",
        })
        .send()
        .await
        .map_err(map_reqwest_error)?;
    if !response.status().is_success() {
        return Err(SpeechSynthesisError::Rejected);
    }
    read_bounded_response(response, MAX_AUDIO_BYTES).await
}

async fn synthesize_doubao(
    model: &LlmModelConfig,
    text: &str,
) -> Result<Vec<u8>, SpeechSynthesisError> {
    let api_key = required(model.api_key.as_deref())?;
    let voice_type = required(model.voice_type.as_deref())?;
    let endpoint = model.endpoint.as_deref().unwrap_or(DOUBAO_ENDPOINT);
    let mut request = endpoint
        .into_client_request()
        .map_err(|_| SpeechSynthesisError::InvalidConfiguration)?;
    insert_header(&mut request, "X-Api-Key", api_key)?;
    insert_header(&mut request, "X-Api-Resource-Id", &model.model)?;
    insert_header(
        &mut request,
        "X-Api-Connect-Id",
        &Uuid::new_v4().to_string(),
    )?;
    let (mut socket, _) = tokio::time::timeout(CONNECT_TIMEOUT, connect_websocket_once(request))
        .await
        .map_err(|_| SpeechSynthesisError::Timeout)?
        .map_err(|_| SpeechSynthesisError::Network)?;

    tokio::time::timeout(REQUEST_TIMEOUT, async {
        send_volc(&mut socket, volc_message(1, None, b"{}")).await?;
        wait_for_event(&mut socket, 50).await?;

        let session_id = Uuid::new_v4().to_string();
        let base_payload = serde_json::json!({
            "user": { "uid": Uuid::new_v4().to_string() },
            "req_params": {
                "speaker": voice_type,
                "audio_params": {
                    "format": "pcm",
                    "sample_rate": SAMPLE_RATE,
                    "enable_timestamp": false,
                },
                "additions": "{\"disable_markdown_filter\":true}"
            },
        });
        let payload = serde_json::to_vec(&base_payload)
            .map_err(|_| SpeechSynthesisError::InvalidConfiguration)?;
        send_volc(&mut socket, volc_message(100, Some(&session_id), &payload)).await?;
        wait_for_event(&mut socket, 150).await?;

        let task_payload = serde_json::json!({
            "user": { "uid": Uuid::new_v4().to_string() },
            "req_params": { "text": text }
        });
        let payload = serde_json::to_vec(&task_payload)
            .map_err(|_| SpeechSynthesisError::InvalidConfiguration)?;
        send_volc(&mut socket, volc_message(200, Some(&session_id), &payload)).await?;
        send_volc(&mut socket, volc_message(102, Some(&session_id), b"{}")).await?;

        let mut audio = Vec::new();
        loop {
            let message = recv_volc(&mut socket).await?;
            match message.message_type {
                0x0b => {
                    if audio.len().saturating_add(message.payload.len()) > MAX_AUDIO_BYTES {
                        return Err(SpeechSynthesisError::ResponseTooLarge);
                    }
                    audio.extend_from_slice(&message.payload);
                }
                0x09 if message.event == Some(152) => break,
                0x09 => {}
                0x0f => return Err(SpeechSynthesisError::Rejected),
                _ => return Err(SpeechSynthesisError::InvalidResponse),
            }
        }
        send_volc(&mut socket, volc_message(2, None, b"{}")).await?;
        if audio.is_empty() {
            return Err(SpeechSynthesisError::InvalidResponse);
        }
        Ok(audio)
    })
    .await
    .map_err(|_| SpeechSynthesisError::Timeout)?
}

fn volc_message(event: u32, session_id: Option<&str>, payload: &[u8]) -> Vec<u8> {
    let session = session_id.unwrap_or_default().as_bytes();
    let include_session = session_id.is_some();
    let capacity = 12
        + payload.len()
        + if include_session {
            session.len() + 4
        } else {
            0
        };
    let mut frame = Vec::with_capacity(capacity);
    frame.extend_from_slice(&[0x11, 0x14, 0x10, 0]);
    frame.extend_from_slice(&event.to_be_bytes());
    if include_session {
        frame.extend_from_slice(&(session.len() as u32).to_be_bytes());
        frame.extend_from_slice(session);
    }
    frame.extend_from_slice(&(payload.len() as u32).to_be_bytes());
    frame.extend_from_slice(payload);
    frame
}

async fn send_volc<S>(socket: &mut S, frame: Vec<u8>) -> Result<(), SpeechSynthesisError>
where
    S: futures::Sink<Message, Error = tokio_tungstenite::tungstenite::Error> + Unpin,
{
    socket
        .send(Message::Binary(frame.into()))
        .await
        .map_err(|_| SpeechSynthesisError::Network)
}

struct VolcResponse {
    message_type: u8,
    event: Option<u32>,
    payload: Vec<u8>,
}

async fn wait_for_event<S>(socket: &mut S, event: u32) -> Result<(), SpeechSynthesisError>
where
    S: futures::Stream<Item = Result<Message, tokio_tungstenite::tungstenite::Error>>
        + futures::Sink<Message, Error = tokio_tungstenite::tungstenite::Error>
        + Unpin,
{
    loop {
        let message = recv_volc(socket).await?;
        if message.message_type == 0x0f {
            return Err(SpeechSynthesisError::Rejected);
        }
        if message.message_type == 0x09 && message.event == Some(event) {
            return Ok(());
        }
    }
}

async fn recv_volc<S>(socket: &mut S) -> Result<VolcResponse, SpeechSynthesisError>
where
    S: futures::Stream<Item = Result<Message, tokio_tungstenite::tungstenite::Error>>
        + futures::Sink<Message, Error = tokio_tungstenite::tungstenite::Error>
        + Unpin,
{
    loop {
        match socket.next().await {
            Some(Ok(Message::Binary(data))) => return parse_volc_response(&data),
            Some(Ok(Message::Ping(payload))) => socket
                .send(Message::Pong(payload))
                .await
                .map_err(|_| SpeechSynthesisError::Network)?,
            Some(Ok(Message::Close(_))) | None => {
                return Err(SpeechSynthesisError::InvalidResponse);
            }
            Some(Err(_)) => return Err(SpeechSynthesisError::Network),
            Some(Ok(Message::Text(_) | Message::Pong(_) | Message::Frame(_))) => {}
        }
    }
}

fn parse_volc_response(data: &[u8]) -> Result<VolcResponse, SpeechSynthesisError> {
    if data.len() < 8 || data[0] >> 4 != 1 || data[0] & 0x0f != 1 {
        return Err(SpeechSynthesisError::InvalidResponse);
    }
    let message_type = data[1] >> 4;
    let flags = data[1] & 0x0f;
    let serialization = data[2] >> 4;
    let compression = data[2] & 0x0f;
    if !matches!(serialization, 0 | 1) || !matches!(compression, 0 | 1) {
        return Err(SpeechSynthesisError::InvalidResponse);
    }
    let mut offset = 4_usize;
    let has_sequence_or_error_code = (matches!(message_type, 0x01 | 0x02 | 0x09 | 0x0b | 0x0c)
        && matches!(flags, 1..=3))
        || message_type == 0x0f;
    if has_sequence_or_error_code {
        offset = checked_advance(data, offset, 4)?;
    }
    let event = if flags == 4 {
        let (event, next) = read_u32(data, offset)?;
        offset = next;
        if !matches!(event, 50..=52) {
            let (_, next) = read_bytes(data, offset, 1024)?;
            offset = next;
        }
        if matches!(event, 50..=52) {
            let (_, next) = read_bytes(data, offset, 1024)?;
            offset = next;
        }
        Some(event)
    } else {
        None
    };
    let (payload, _) = read_bytes(data, offset, MAX_FRAME_BYTES)?;
    let payload = match compression {
        0 => payload.to_vec(),
        1 => gunzip_bounded(payload, MAX_FRAME_BYTES)?,
        _ => return Err(SpeechSynthesisError::InvalidResponse),
    };
    Ok(VolcResponse {
        message_type,
        event,
        payload,
    })
}

fn gunzip_bounded(data: &[u8], max: usize) -> Result<Vec<u8>, SpeechSynthesisError> {
    let decoder = GzDecoder::new(data);
    let mut output = Vec::new();
    decoder
        .take((max + 1) as u64)
        .read_to_end(&mut output)
        .map_err(|_| SpeechSynthesisError::InvalidResponse)?;
    if output.len() > max {
        return Err(SpeechSynthesisError::ResponseTooLarge);
    }
    Ok(output)
}

fn read_bytes(
    data: &[u8],
    offset: usize,
    max: usize,
) -> Result<(&[u8], usize), SpeechSynthesisError> {
    let (length, offset) = read_u32(data, offset)?;
    let length = usize::try_from(length).map_err(|_| SpeechSynthesisError::InvalidResponse)?;
    if length > max {
        return Err(SpeechSynthesisError::ResponseTooLarge);
    }
    let end = checked_advance(data, offset, length)?;
    Ok((&data[offset..end], end))
}

fn read_u32(data: &[u8], offset: usize) -> Result<(u32, usize), SpeechSynthesisError> {
    let end = checked_advance(data, offset, 4)?;
    let bytes: [u8; 4] = data[offset..end]
        .try_into()
        .map_err(|_| SpeechSynthesisError::InvalidResponse)?;
    Ok((u32::from_be_bytes(bytes), end))
}

fn checked_advance(
    data: &[u8],
    offset: usize,
    length: usize,
) -> Result<usize, SpeechSynthesisError> {
    offset
        .checked_add(length)
        .filter(|end| *end <= data.len())
        .ok_or(SpeechSynthesisError::InvalidResponse)
}

fn decode_pcm(bytes: Vec<u8>) -> Result<Vec<i16>, SpeechSynthesisError> {
    if bytes.is_empty() || !bytes.len().is_multiple_of(2) || bytes.len() > MAX_AUDIO_BYTES {
        return Err(SpeechSynthesisError::InvalidResponse);
    }
    Ok(bytes
        .chunks_exact(2)
        .map(|pair| i16::from_le_bytes([pair[0], pair[1]]))
        .collect())
}

async fn read_bounded_response(
    response: reqwest::Response,
    max: usize,
) -> Result<Vec<u8>, SpeechSynthesisError> {
    if response
        .content_length()
        .is_some_and(|length| length > max as u64)
    {
        return Err(SpeechSynthesisError::ResponseTooLarge);
    }
    let mut stream = response.bytes_stream();
    let mut bytes = Vec::new();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|_| SpeechSynthesisError::Network)?;
        if bytes.len().saturating_add(chunk.len()) > max {
            return Err(SpeechSynthesisError::ResponseTooLarge);
        }
        bytes.extend_from_slice(&chunk);
    }
    if bytes.is_empty() {
        return Err(SpeechSynthesisError::InvalidResponse);
    }
    Ok(bytes)
}

fn insert_header(
    request: &mut tokio_tungstenite::tungstenite::http::Request<()>,
    name: &'static str,
    value: &str,
) -> Result<(), SpeechSynthesisError> {
    let value = value
        .parse()
        .map_err(|_| SpeechSynthesisError::InvalidConfiguration)?;
    request.headers_mut().insert(name, value);
    Ok(())
}

fn required(value: Option<&str>) -> Result<&str, SpeechSynthesisError> {
    value
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or(SpeechSynthesisError::InvalidConfiguration)
}

fn map_reqwest_error(error: reqwest::Error) -> SpeechSynthesisError {
    if error.is_timeout() {
        SpeechSynthesisError::Timeout
    } else {
        SpeechSynthesisError::Network
    }
}

#[cfg(test)]
pub(crate) fn volc_message_for_test(
    event: u32,
    session_id: Option<&str>,
    payload: &[u8],
) -> Vec<u8> {
    volc_message(event, session_id, payload)
}

#[cfg(test)]
pub(crate) fn parse_volc_response_for_test(
    data: &[u8],
) -> Result<(u8, Option<u32>, Vec<u8>), SpeechSynthesisError> {
    parse_volc_response(data)
        .map(|response| (response.message_type, response.event, response.payload))
}

#[cfg(test)]
pub(crate) fn decode_pcm_for_test(bytes: Vec<u8>) -> Result<Vec<i16>, SpeechSynthesisError> {
    decode_pcm(bytes)
}

#[cfg(test)]
pub(crate) fn parse_gzip_response_for_test(
    data: &[u8],
) -> Result<(u8, Option<u32>, Vec<u8>), SpeechSynthesisError> {
    parse_volc_response(data)
        .map(|response| (response.message_type, response.event, response.payload))
}
