//! 提供供应商无关的语音转写输入，并隔离 OpenAI 与豆包协议。

use std::{
    error::Error,
    fmt,
    io::{Read, Write},
    time::Duration,
};

use flate2::{Compression, read::GzDecoder, write::GzEncoder};
use futures::{SinkExt as _, StreamExt as _};
use reqwest::multipart::{Form, Part};
use serde::Deserialize;
use tokio_tungstenite::tungstenite::{Message, client::IntoClientRequest as _};
use uuid::Uuid;

use crate::{
    config::{AppLanguage, LlmModelConfig, LlmProvider, ModelKind, ModelProvider},
    transport::{connect_websocket_once, provider_http_client},
};

const SAMPLE_RATE: u32 = 16_000;
const MAX_SAMPLES: usize = SAMPLE_RATE as usize * 30;
const MAX_RESPONSE_BYTES: usize = 256 * 1024;
const MAX_TRANSCRIPT_BYTES: usize = 64 * 1024;
const REQUEST_TIMEOUT: Duration = Duration::from_secs(75);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const OPENAI_ENDPOINT: &str = "https://api.openai.com/v1/";
const DOUBAO_ENDPOINT: &str = "wss://openspeech.bytedance.com/api/v3/sauc/bigmodel";
const PCM_CHUNK_BYTES: usize = 3_200;

/// 一段已完成端点检测的 16 kHz 单声道 PCM。
pub struct TranscriptionInput {
    samples: Vec<i16>,
}

impl TranscriptionInput {
    /// 创建有界转写输入。
    pub fn new(samples: Vec<i16>) -> Result<Self, TranscriptionError> {
        if samples.is_empty() || samples.len() > MAX_SAMPLES {
            return Err(TranscriptionError::InvalidInput);
        }
        Ok(Self { samples })
    }
}

/// 不携带凭据、音频或供应商响应正文的稳定转写错误。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TranscriptionError {
    InvalidConfiguration,
    InvalidInput,
    UnsupportedProvider,
    Network,
    Timeout,
    Rejected,
    InvalidResponse,
    ResponseTooLarge,
}

impl fmt::Display for TranscriptionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::InvalidConfiguration => "语音转写模型配置无效",
            Self::InvalidInput => "待转写音频无效",
            Self::UnsupportedProvider => "当前 Provider 不支持语音转写",
            Self::Network => "语音转写网络请求失败",
            Self::Timeout => "语音转写请求超时",
            Self::Rejected => "语音转写请求被 Provider 拒绝",
            Self::InvalidResponse => "语音转写响应无效",
            Self::ResponseTooLarge => "语音转写响应超过大小限制",
        })
    }
}

impl Error for TranscriptionError {}

impl TranscriptionError {
    /// 使用单次请求携带的语言生成用户可见错误，不读取进程全局 locale。
    pub fn localized_message(self, language: AppLanguage) -> String {
        match self {
            Self::InvalidConfiguration => {
                rust_i18n::t!("voice.error.invalid_configuration", locale = language.id())
            }
            Self::InvalidInput => {
                rust_i18n::t!("voice.error.invalid_input", locale = language.id())
            }
            Self::UnsupportedProvider => {
                rust_i18n::t!("voice.error.unsupported_provider", locale = language.id())
            }
            Self::Network => rust_i18n::t!("voice.error.network", locale = language.id()),
            Self::Timeout => rust_i18n::t!("voice.error.timeout", locale = language.id()),
            Self::Rejected => rust_i18n::t!("voice.error.rejected", locale = language.id()),
            Self::InvalidResponse => {
                rust_i18n::t!("voice.error.invalid_response", locale = language.id())
            }
            Self::ResponseTooLarge => {
                rust_i18n::t!("voice.error.response_too_large", locale = language.id())
            }
        }
        .to_string()
    }
}

/// 使用模型条目对应的云端 Provider 转写完整句段。
pub async fn transcribe(
    model: &LlmModelConfig,
    input: TranscriptionInput,
    language: AppLanguage,
) -> Result<String, TranscriptionError> {
    if model.kind != ModelKind::Transcription {
        return Err(TranscriptionError::InvalidConfiguration);
    }
    let result = match model.provider {
        ModelProvider::Genai(LlmProvider::OpenAI) => {
            transcribe_openai(model, input, language).await
        }
        ModelProvider::Doubao => transcribe_doubao(model, input, language).await,
        ModelProvider::LocalWhisper => Err(TranscriptionError::UnsupportedProvider),
        ModelProvider::Genai(_) => Err(TranscriptionError::UnsupportedProvider),
    }?;
    let result = result.trim();
    if result.is_empty() || result.len() > MAX_TRANSCRIPT_BYTES {
        return Err(TranscriptionError::InvalidResponse);
    }
    Ok(result.to_owned())
}

async fn transcribe_openai(
    model: &LlmModelConfig,
    input: TranscriptionInput,
    language: AppLanguage,
) -> Result<String, TranscriptionError> {
    let api_key = required(model.api_key.as_deref())?;
    let endpoint = model.endpoint.as_deref().unwrap_or(OPENAI_ENDPOINT);
    let wav = encode_wav(&input.samples)?;
    let part = Part::bytes(wav)
        .file_name("utterance.wav")
        .mime_str("audio/wav")
        .map_err(|_| TranscriptionError::InvalidConfiguration)?;
    let mut form = Form::new()
        .text("model", model.model.clone())
        .part("file", part);
    form = form.text("language", openai_language(language));
    let client = provider_http_client(
        Some(endpoint),
        CONNECT_TIMEOUT,
        REQUEST_TIMEOUT,
        REQUEST_TIMEOUT,
    )
    .map_err(|_| TranscriptionError::Network)?;
    let response = client
        .post(format!(
            "{}audio/transcriptions",
            endpoint.trim_end_matches('/').to_owned() + "/"
        ))
        .bearer_auth(api_key)
        .multipart(form)
        .send()
        .await
        .map_err(map_reqwest_error)?;
    if !response.status().is_success() {
        return Err(TranscriptionError::Rejected);
    }
    let bytes = read_bounded_response(response, MAX_RESPONSE_BYTES).await?;
    let response: OpenAiTranscriptionResponse =
        serde_json::from_slice(&bytes).map_err(|_| TranscriptionError::InvalidResponse)?;
    Ok(response.text)
}

#[derive(Deserialize)]
struct OpenAiTranscriptionResponse {
    text: String,
}

async fn transcribe_doubao(
    model: &LlmModelConfig,
    input: TranscriptionInput,
    language: AppLanguage,
) -> Result<String, TranscriptionError> {
    let api_key = required(model.api_key.as_deref())?;
    let endpoint = model.endpoint.as_deref().unwrap_or(DOUBAO_ENDPOINT);
    let mut request = endpoint
        .into_client_request()
        .map_err(|_| TranscriptionError::InvalidConfiguration)?;
    insert_header(&mut request, "X-Api-Key", api_key)?;
    insert_header(&mut request, "X-Api-Resource-Id", &model.model)?;
    insert_header(
        &mut request,
        "X-Api-Request-Id",
        &Uuid::new_v4().to_string(),
    )?;
    insert_header(&mut request, "X-Api-Sequence", "-1")?;
    let (mut socket, _) = tokio::time::timeout(CONNECT_TIMEOUT, connect_websocket_once(request))
        .await
        .map_err(|_| TranscriptionError::Timeout)?
        .map_err(|_| TranscriptionError::Network)?;

    let init = serde_json::json!({
        "user": { "uid": Uuid::new_v4().to_string() },
        "audio": {
            "format": "pcm",
            "codec": "raw",
            "rate": SAMPLE_RATE,
            "bits": 16,
            "channel": 1,
            "language": doubao_language(language),
        },
        "request": {
            "model_name": "bigmodel",
            "enable_itn": true,
            "enable_punc": true,
            "enable_ddc": false,
            "show_utterances": false,
        }
    });
    let init = serde_json::to_vec(&init).map_err(|_| TranscriptionError::InvalidConfiguration)?;
    socket
        .send(Message::Binary(
            encode_sauc_frame(0x1, 0x1, 0x1, 0x1, 1, &init)?.into(),
        ))
        .await
        .map_err(|_| TranscriptionError::Network)?;
    tokio::time::timeout(CONNECT_TIMEOUT, async {
        loop {
            match socket.next().await {
                Some(Ok(Message::Binary(data))) => {
                    let response = parse_sauc_response(&data)?;
                    return if response.error {
                        Err(TranscriptionError::Rejected)
                    } else {
                        Ok(())
                    };
                }
                Some(Ok(Message::Ping(payload))) => socket
                    .send(Message::Pong(payload))
                    .await
                    .map_err(|_| TranscriptionError::Network)?,
                Some(Ok(Message::Close(_))) | None => {
                    return Err(TranscriptionError::InvalidResponse);
                }
                Some(Err(_)) => return Err(TranscriptionError::Network),
                Some(Ok(Message::Text(_) | Message::Pong(_) | Message::Frame(_))) => {}
            }
        }
    })
    .await
    .map_err(|_| TranscriptionError::Timeout)??;

    let pcm = pcm16_bytes(&input.samples);
    let mut sequence = 2_i32;
    for chunk in pcm.chunks(PCM_CHUNK_BYTES) {
        socket
            .send(Message::Binary(
                encode_sauc_frame(0x2, 0x1, 0x1, 0x1, sequence, chunk)?.into(),
            ))
            .await
            .map_err(|_| TranscriptionError::Network)?;
        sequence = sequence
            .checked_add(1)
            .ok_or(TranscriptionError::InvalidInput)?;
    }
    socket
        .send(Message::Binary(
            encode_sauc_frame(0x2, 0x3, 0x1, 0x1, -sequence, &[])?.into(),
        ))
        .await
        .map_err(|_| TranscriptionError::Network)?;

    tokio::time::timeout(REQUEST_TIMEOUT, async {
        let mut latest = String::new();
        while let Some(message) = socket.next().await {
            match message.map_err(|_| TranscriptionError::Network)? {
                Message::Binary(data) => {
                    let response = parse_sauc_response(&data)?;
                    if response.error {
                        return Err(TranscriptionError::Rejected);
                    }
                    if let Some(text) = response.text {
                        if text.len() > MAX_TRANSCRIPT_BYTES {
                            return Err(TranscriptionError::ResponseTooLarge);
                        }
                        latest = text;
                    }
                    if response.last {
                        return Ok(latest);
                    }
                }
                Message::Close(_) => break,
                Message::Ping(payload) => {
                    socket
                        .send(Message::Pong(payload))
                        .await
                        .map_err(|_| TranscriptionError::Network)?;
                }
                Message::Text(_) | Message::Pong(_) | Message::Frame(_) => {}
            }
        }
        Err(TranscriptionError::InvalidResponse)
    })
    .await
    .map_err(|_| TranscriptionError::Timeout)?
}

fn encode_wav(samples: &[i16]) -> Result<Vec<u8>, TranscriptionError> {
    let data_len = samples
        .len()
        .checked_mul(2)
        .and_then(|size| u32::try_from(size).ok())
        .ok_or(TranscriptionError::InvalidInput)?;
    let riff_len = data_len
        .checked_add(36)
        .ok_or(TranscriptionError::InvalidInput)?;
    let mut wav = Vec::with_capacity(data_len as usize + 44);
    wav.extend_from_slice(b"RIFF");
    wav.extend_from_slice(&riff_len.to_le_bytes());
    wav.extend_from_slice(b"WAVEfmt ");
    wav.extend_from_slice(&16_u32.to_le_bytes());
    wav.extend_from_slice(&1_u16.to_le_bytes());
    wav.extend_from_slice(&1_u16.to_le_bytes());
    wav.extend_from_slice(&SAMPLE_RATE.to_le_bytes());
    wav.extend_from_slice(&(SAMPLE_RATE * 2).to_le_bytes());
    wav.extend_from_slice(&2_u16.to_le_bytes());
    wav.extend_from_slice(&16_u16.to_le_bytes());
    wav.extend_from_slice(b"data");
    wav.extend_from_slice(&data_len.to_le_bytes());
    for sample in samples {
        wav.extend_from_slice(&sample.to_le_bytes());
    }
    Ok(wav)
}

fn pcm16_bytes(samples: &[i16]) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(samples.len() * 2);
    for sample in samples {
        bytes.extend_from_slice(&sample.to_le_bytes());
    }
    bytes
}

fn encode_sauc_frame(
    message_type: u8,
    flags: u8,
    serialization: u8,
    compression: u8,
    sequence: i32,
    payload: &[u8],
) -> Result<Vec<u8>, TranscriptionError> {
    let payload = match compression {
        0 => payload.to_vec(),
        1 => gzip(payload)?,
        _ => return Err(TranscriptionError::InvalidInput),
    };
    let payload_len = u32::try_from(payload.len()).map_err(|_| TranscriptionError::InvalidInput)?;
    let mut frame = Vec::with_capacity(payload.len() + 12);
    frame.extend_from_slice(&[
        0x11,
        (message_type << 4) | flags,
        (serialization << 4) | compression,
        0,
    ]);
    frame.extend_from_slice(&sequence.to_be_bytes());
    frame.extend_from_slice(&payload_len.to_be_bytes());
    frame.extend_from_slice(&payload);
    Ok(frame)
}

struct SaucResponse {
    text: Option<String>,
    last: bool,
    error: bool,
}

fn parse_sauc_response(data: &[u8]) -> Result<SaucResponse, TranscriptionError> {
    if data.len() < 8 || data[0] >> 4 != 1 || data[0] & 0x0f != 1 {
        return Err(TranscriptionError::InvalidResponse);
    }
    let message_type = data[1] >> 4;
    let flags = data[1] & 0x0f;
    let serialization = data[2] >> 4;
    let compression = data[2] & 0x0f;
    if !matches!(compression, 0 | 1) || !matches!(serialization, 0 | 1) {
        return Err(TranscriptionError::InvalidResponse);
    }
    let mut offset = 4_usize;
    if flags & 0x01 != 0 {
        offset = checked_advance(data, offset, 4)?;
    }
    if flags & 0x04 != 0 {
        offset = checked_advance(data, offset, 4)?;
    }
    let error = message_type == 0x0f;
    if error {
        offset = checked_advance(data, offset, 4)?;
    } else if message_type != 0x09 && message_type != 0x01 {
        return Err(TranscriptionError::InvalidResponse);
    }
    let (payload_len, payload_offset) = read_u32(data, offset)?;
    let payload_len =
        usize::try_from(payload_len).map_err(|_| TranscriptionError::InvalidResponse)?;
    if payload_len > MAX_RESPONSE_BYTES {
        return Err(TranscriptionError::ResponseTooLarge);
    }
    let end = payload_offset
        .checked_add(payload_len)
        .filter(|end| *end <= data.len())
        .ok_or(TranscriptionError::InvalidResponse)?;
    let payload = &data[payload_offset..end];
    let payload = match compression {
        0 => payload.to_vec(),
        1 => gunzip_bounded(payload, MAX_RESPONSE_BYTES)?,
        _ => return Err(TranscriptionError::InvalidResponse),
    };
    let text = if !error && serialization == 1 && !payload.is_empty() {
        let payload: DoubaoResponsePayload =
            serde_json::from_slice(&payload).map_err(|_| TranscriptionError::InvalidResponse)?;
        payload.result.map(|result| result.text)
    } else {
        None
    };
    Ok(SaucResponse {
        text,
        last: flags & 0x02 != 0,
        error,
    })
}

#[derive(Deserialize)]
struct DoubaoResponsePayload {
    result: Option<DoubaoResult>,
}

#[derive(Deserialize)]
struct DoubaoResult {
    text: String,
}

fn gzip(data: &[u8]) -> Result<Vec<u8>, TranscriptionError> {
    let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
    encoder
        .write_all(data)
        .map_err(|_| TranscriptionError::InvalidInput)?;
    encoder
        .finish()
        .map_err(|_| TranscriptionError::InvalidInput)
}

fn gunzip_bounded(data: &[u8], max: usize) -> Result<Vec<u8>, TranscriptionError> {
    let decoder = GzDecoder::new(data);
    let mut output = Vec::new();
    decoder
        .take((max + 1) as u64)
        .read_to_end(&mut output)
        .map_err(|_| TranscriptionError::InvalidResponse)?;
    if output.len() > max {
        return Err(TranscriptionError::ResponseTooLarge);
    }
    Ok(output)
}

async fn read_bounded_response(
    response: reqwest::Response,
    max: usize,
) -> Result<Vec<u8>, TranscriptionError> {
    if response
        .content_length()
        .is_some_and(|length| length > max as u64)
    {
        return Err(TranscriptionError::ResponseTooLarge);
    }
    let mut stream = response.bytes_stream();
    let mut bytes = Vec::new();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|_| TranscriptionError::Network)?;
        if bytes.len().saturating_add(chunk.len()) > max {
            return Err(TranscriptionError::ResponseTooLarge);
        }
        bytes.extend_from_slice(&chunk);
    }
    Ok(bytes)
}

fn insert_header(
    request: &mut tokio_tungstenite::tungstenite::http::Request<()>,
    name: &'static str,
    value: &str,
) -> Result<(), TranscriptionError> {
    let value = value
        .parse()
        .map_err(|_| TranscriptionError::InvalidConfiguration)?;
    request.headers_mut().insert(name, value);
    Ok(())
}

fn required(value: Option<&str>) -> Result<&str, TranscriptionError> {
    value
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or(TranscriptionError::InvalidConfiguration)
}

fn read_u32(data: &[u8], offset: usize) -> Result<(u32, usize), TranscriptionError> {
    let end = checked_advance(data, offset, 4)?;
    let bytes: [u8; 4] = data[offset..end]
        .try_into()
        .map_err(|_| TranscriptionError::InvalidResponse)?;
    Ok((u32::from_be_bytes(bytes), end))
}

fn checked_advance(data: &[u8], offset: usize, length: usize) -> Result<usize, TranscriptionError> {
    offset
        .checked_add(length)
        .filter(|end| *end <= data.len())
        .ok_or(TranscriptionError::InvalidResponse)
}

fn map_reqwest_error(error: reqwest::Error) -> TranscriptionError {
    if error.is_timeout() {
        TranscriptionError::Timeout
    } else {
        TranscriptionError::Network
    }
}

const fn openai_language(language: AppLanguage) -> &'static str {
    match language {
        AppLanguage::SimplifiedChinese | AppLanguage::TraditionalChinese => "zh",
        AppLanguage::English => "en",
        AppLanguage::Japanese => "ja",
    }
}

const fn doubao_language(language: AppLanguage) -> &'static str {
    match language {
        AppLanguage::SimplifiedChinese | AppLanguage::TraditionalChinese => "zh-CN",
        AppLanguage::English => "en-US",
        AppLanguage::Japanese => "ja-JP",
    }
}

#[cfg(test)]
pub(crate) fn encode_sauc_frame_for_test(
    message_type: u8,
    flags: u8,
    serialization: u8,
    compression: u8,
    sequence: i32,
    payload: &[u8],
) -> Result<Vec<u8>, TranscriptionError> {
    encode_sauc_frame(
        message_type,
        flags,
        serialization,
        compression,
        sequence,
        payload,
    )
}

#[cfg(test)]
pub(crate) fn parse_sauc_response_for_test(
    data: &[u8],
) -> Result<(Option<String>, bool, bool), TranscriptionError> {
    parse_sauc_response(data).map(|response| (response.text, response.last, response.error))
}

#[cfg(test)]
pub(crate) fn encode_wav_for_test(samples: &[i16]) -> Result<Vec<u8>, TranscriptionError> {
    encode_wav(samples)
}
