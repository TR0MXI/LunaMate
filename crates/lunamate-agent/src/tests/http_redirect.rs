use std::{
    env,
    io::{self, Read as _, Write as _},
    net::{TcpListener, TcpStream},
    process::{Command, Output},
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

use genai::{Error as GenaiError, ModelIden, chat::ChatRequest, webc::Error as GenaiWebError};
use reqwest::StatusCode;

use crate::{
    client_from_model,
    config::{
        AppLanguage, LlmAdvancedOptions, LlmModelConfig, LlmProvider, ModelKind, ModelProvider,
        endpoint_is_plaintext_loopback,
    },
    stt::{TranscriptionError, TranscriptionInput, transcribe},
    transport::connect_websocket_once,
    tts::{SpeechSynthesisError, synthesize},
};
use tokio_tungstenite::tungstenite::client::IntoClientRequest as _;

const SERVER_IO_TIMEOUT: Duration = Duration::from_secs(2);
const SERVER_GRACE: Duration = Duration::from_millis(150);
const SERVER_HARD_TIMEOUT: Duration = Duration::from_secs(90);
const LISTENER_POLL_INTERVAL: Duration = Duration::from_millis(5);
const MAX_HEADER_BYTES: usize = 64 * 1024;
const PROXY_CHILD_MODE_ENV: &str = "LUNAMATE_TEST_PROXY_CHILD_MODE";
const PROXY_CHILD_ENDPOINT_ENV: &str = "LUNAMATE_TEST_PROXY_CHILD_ENDPOINT";
const PROXY_CHILD_TEST: &str = "tests::http_redirect::provider_proxy_policy_child";

#[derive(Default)]
struct RequestStats {
    requests: AtomicUsize,
    body_bytes: AtomicUsize,
}

#[derive(Clone, Copy, Debug)]
struct RequestSnapshot {
    requests: usize,
    body_bytes: usize,
}

impl RequestStats {
    fn snapshot(&self) -> RequestSnapshot {
        RequestSnapshot {
            requests: self.requests.load(Ordering::SeqCst),
            body_bytes: self.body_bytes.load(Ordering::SeqCst),
        }
    }
}

struct RedirectServer {
    endpoint: String,
    completed: Arc<AtomicBool>,
    source_stats: Arc<RequestStats>,
    target_stats: Arc<RequestStats>,
    source_task: JoinHandle<Result<(), String>>,
    target_task: JoinHandle<Result<(), String>>,
}

impl RedirectServer {
    fn start() -> Self {
        let target_listener =
            TcpListener::bind("127.0.0.1:0").expect("测试重定向目标 listener 应可绑定");
        target_listener
            .set_nonblocking(true)
            .expect("测试重定向目标 listener 应可设为非阻塞");
        let target_address = target_listener
            .local_addr()
            .expect("测试重定向目标 listener 应有本地地址");

        let source_listener =
            TcpListener::bind("127.0.0.1:0").expect("测试 Provider listener 应可绑定");
        source_listener
            .set_nonblocking(true)
            .expect("测试 Provider listener 应可设为非阻塞");
        let source_address = source_listener
            .local_addr()
            .expect("测试 Provider listener 应有本地地址");

        let completed = Arc::new(AtomicBool::new(false));
        let source_stats = Arc::new(RequestStats::default());
        let target_stats = Arc::new(RequestStats::default());
        let source_response = format!(
            "HTTP/1.1 307 Temporary Redirect\r\nLocation: http://{target_address}/redirect-target\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
        );
        let target_response =
            "HTTP/1.1 400 Bad Request\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".to_owned();

        let source_task = spawn_listener(
            "provider-redirect-source",
            source_listener,
            source_response,
            Arc::clone(&source_stats),
            Arc::clone(&completed),
            true,
        );
        let target_task = spawn_listener(
            "provider-redirect-target",
            target_listener,
            target_response,
            Arc::clone(&target_stats),
            Arc::clone(&completed),
            false,
        );

        Self {
            endpoint: format!("http://{source_address}/v1/"),
            completed,
            source_stats,
            target_stats,
            source_task,
            target_task,
        }
    }

    fn endpoint(&self) -> &str {
        &self.endpoint
    }

    fn assert_not_followed(self) {
        let Self {
            completed,
            source_stats,
            target_stats,
            source_task,
            target_task,
            ..
        } = self;
        completed.store(true, Ordering::SeqCst);
        join_listener(source_task, "第一跳 Provider listener");
        join_listener(target_task, "第二跳重定向 listener");

        assert_eq!(source_stats.requests.load(Ordering::SeqCst), 1);
        assert!(source_stats.body_bytes.load(Ordering::SeqCst) > 0);
        assert_eq!(target_stats.requests.load(Ordering::SeqCst), 0);
        assert_eq!(target_stats.body_bytes.load(Ordering::SeqCst), 0);
    }
}

struct ProxyBypassServer {
    endpoint: String,
    proxy: String,
    completed: Arc<AtomicBool>,
    endpoint_stats: Arc<RequestStats>,
    proxy_stats: Arc<RequestStats>,
    endpoint_task: JoinHandle<Result<(), String>>,
    proxy_task: JoinHandle<Result<(), String>>,
}

impl ProxyBypassServer {
    fn start(response: String) -> Self {
        let endpoint_listener =
            TcpListener::bind("127.0.0.1:0").expect("测试回环 Provider listener 应可绑定");
        endpoint_listener
            .set_nonblocking(true)
            .expect("测试回环 Provider listener 应可设为非阻塞");
        let endpoint_address = endpoint_listener
            .local_addr()
            .expect("测试回环 Provider listener 应有本地地址");
        let proxy_listener =
            TcpListener::bind("127.0.0.1:0").expect("测试 HTTP proxy listener 应可绑定");
        proxy_listener
            .set_nonblocking(true)
            .expect("测试 HTTP proxy listener 应可设为非阻塞");
        let proxy_address = proxy_listener
            .local_addr()
            .expect("测试 HTTP proxy listener 应有本地地址");

        let completed = Arc::new(AtomicBool::new(false));
        let endpoint_stats = Arc::new(RequestStats::default());
        let proxy_stats = Arc::new(RequestStats::default());
        let endpoint_task = spawn_listener(
            "proxy-policy-endpoint",
            endpoint_listener,
            response.clone(),
            Arc::clone(&endpoint_stats),
            Arc::clone(&completed),
            true,
        );
        let proxy_task = spawn_listener(
            "proxy-policy-proxy",
            proxy_listener,
            response,
            Arc::clone(&proxy_stats),
            Arc::clone(&completed),
            true,
        );
        Self {
            endpoint: format!("http://{endpoint_address}/v1/"),
            proxy: format!("http://{proxy_address}"),
            completed,
            endpoint_stats,
            proxy_stats,
            endpoint_task,
            proxy_task,
        }
    }

    fn endpoint(&self) -> &str {
        &self.endpoint
    }

    fn proxy(&self) -> &str {
        &self.proxy
    }

    fn finish(self) -> (RequestSnapshot, RequestSnapshot) {
        self.completed.store(true, Ordering::SeqCst);
        join_listener(self.endpoint_task, "回环 Provider listener");
        join_listener(self.proxy_task, "HTTP proxy listener");
        (self.endpoint_stats.snapshot(), self.proxy_stats.snapshot())
    }
}

struct ProxyCaptureServer {
    endpoint: String,
    completed: Arc<AtomicBool>,
    stats: Arc<RequestStats>,
    task: JoinHandle<Result<(), String>>,
}

impl ProxyCaptureServer {
    fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("测试系统 proxy listener 应可绑定");
        listener
            .set_nonblocking(true)
            .expect("测试系统 proxy listener 应可设为非阻塞");
        let address = listener
            .local_addr()
            .expect("测试系统 proxy listener 应有本地地址");
        let completed = Arc::new(AtomicBool::new(false));
        let stats = Arc::new(RequestStats::default());
        let task = spawn_listener(
            "remote-system-proxy",
            listener,
            "HTTP/1.1 502 Bad Gateway\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".to_owned(),
            Arc::clone(&stats),
            Arc::clone(&completed),
            true,
        );
        Self {
            endpoint: format!("http://{address}"),
            completed,
            stats,
            task,
        }
    }

    fn endpoint(&self) -> &str {
        &self.endpoint
    }

    fn finish(self) -> RequestSnapshot {
        self.completed.store(true, Ordering::SeqCst);
        join_listener(self.task, "远端系统 proxy listener");
        self.stats.snapshot()
    }
}

#[tokio::test]
async fn chat_client_rejects_307_without_forwarding_request_body() {
    let server = RedirectServer::start();
    let model = openai_model(ModelKind::ChatCompletions, server.endpoint());
    let client = client_from_model(&model);

    let result = client
        .exec_chat(
            ModelIden::new(LlmProvider::OpenAI, model.model.clone()),
            ChatRequest::from_user("chat redirect payload"),
            None,
        )
        .await;

    assert!(matches!(
        result,
        Err(GenaiError::WebModelCall {
            webc_error: GenaiWebError::ResponseFailedStatus { status, .. },
            ..
        }) if status == StatusCode::TEMPORARY_REDIRECT
    ));
    server.assert_not_followed();
}

#[tokio::test]
async fn openai_stt_rejects_307_without_forwarding_request_body() {
    let server = RedirectServer::start();
    let model = openai_model(ModelKind::Transcription, server.endpoint());
    let input = TranscriptionInput::new(vec![1_i16; 160]).expect("测试 PCM 输入应有效");

    let result = transcribe(&model, input, AppLanguage::English).await;

    assert!(matches!(result, Err(TranscriptionError::Rejected)));
    server.assert_not_followed();
}

#[tokio::test]
async fn openai_tts_rejects_307_without_forwarding_request_body() {
    let server = RedirectServer::start();
    let model = openai_model(ModelKind::SpeechSynthesis, server.endpoint());

    let result = synthesize(&model, "speech redirect payload").await;

    assert!(matches!(result, Err(SpeechSynthesisError::Rejected)));
    server.assert_not_followed();
}

#[test]
fn plaintext_loopback_proxy_policy_is_strict() {
    for endpoint in [
        "http://localhost:11434/v1/",
        "http://127.0.0.1:11434/v1/",
        "http://[::1]:11434/v1/",
        "ws://localhost:8080/speech",
        "ws://127.0.0.1:8080/speech",
        "ws://[::1]:8080/speech",
    ] {
        assert!(
            endpoint_is_plaintext_loopback(endpoint),
            "明文回环 endpoint 应禁用代理：{endpoint}"
        );
    }
    for endpoint in [
        "https://localhost:11434/v1/",
        "wss://127.0.0.1:8080/speech",
        "http://localhost.example/v1/",
        "http://192.0.2.1/v1/",
        "https://api.openai.com/v1/",
        "not-an-endpoint",
    ] {
        assert!(
            !endpoint_is_plaintext_loopback(endpoint),
            "非明文回环 endpoint 不应禁用系统代理：{endpoint}"
        );
    }
}

#[test]
fn loopback_http_clients_bypass_proxy_environment_in_child_processes() {
    for mode in ["chat", "stt", "tts"] {
        let server = ProxyBypassServer::start(success_response(mode));
        let output = run_proxy_child(mode, server.endpoint(), server.proxy());
        let (endpoint, proxy) = server.finish();

        assert_child_succeeded(mode, &output);
        assert_eq!(endpoint.requests, 1, "{mode} 请求应到达回环 Provider");
        assert!(
            endpoint.body_bytes > 0,
            "{mode} 回环 Provider 应收到请求正文"
        );
        assert_eq!(proxy.requests, 0, "{mode} 请求不得到达环境 HTTP proxy");
        assert_eq!(proxy.body_bytes, 0, "环境 HTTP proxy 不得收到 {mode} 正文");
    }
}

#[test]
fn loopback_websocket_bypasses_proxy_environment_in_child_process() {
    let response =
        "HTTP/1.1 400 Bad Request\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".to_owned();
    let server = ProxyBypassServer::start(response);
    let endpoint = server.endpoint().replacen("http://", "ws://", 1);
    let output = run_proxy_child("websocket", &endpoint, server.proxy());
    let (endpoint, proxy) = server.finish();

    assert_child_succeeded("websocket", &output);
    assert_eq!(endpoint.requests, 1, "WebSocket 握手应到达回环 Provider");
    assert_eq!(endpoint.body_bytes, 0, "WebSocket 握手不应携带正文");
    assert_eq!(proxy.requests, 0, "WebSocket 握手不得到达环境 HTTP proxy");
    assert_eq!(
        proxy.body_bytes, 0,
        "环境 HTTP proxy 不得收到 WebSocket 正文"
    );
}

#[test]
fn remote_https_chat_keeps_system_proxy_in_child_process() {
    let proxy = ProxyCaptureServer::start();
    let output = run_proxy_child(
        "remote-chat",
        "https://proxy-policy.invalid/v1/",
        proxy.endpoint(),
    );
    let proxy = proxy.finish();

    assert_child_succeeded("remote-chat", &output);
    assert_eq!(proxy.requests, 1, "远端 HTTPS 请求应连接环境 proxy");
    assert_eq!(proxy.body_bytes, 0, "HTTPS CONNECT 不应发送 Provider 正文");
}

#[test]
fn remote_http_chat_is_allowed_and_keeps_system_proxy_in_child_process() {
    let proxy = ProxyCaptureServer::start();
    let output = run_proxy_child(
        "remote-http-chat",
        "http://proxy-policy.invalid/v1/",
        proxy.endpoint(),
    );
    let proxy = proxy.finish();

    assert_child_succeeded("remote-http-chat", &output);
    assert_eq!(proxy.requests, 1, "远端 HTTP 请求应连接环境 proxy");
    assert!(
        proxy.body_bytes > 0,
        "HTTP proxy 应收到明文 Provider 请求正文"
    );
}

#[test]
fn provider_proxy_policy_child() {
    let Ok(mode) = env::var(PROXY_CHILD_MODE_ENV) else {
        return;
    };
    let endpoint = env::var(PROXY_CHILD_ENDPOINT_ENV).expect("代理策略子进程应收到测试 endpoint");
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("代理策略子进程 Tokio runtime 应可构建");
    runtime.block_on(async move {
        match mode.as_str() {
            "chat" | "remote-chat" | "remote-http-chat" => {
                let model = openai_model(ModelKind::ChatCompletions, &endpoint);
                let client = client_from_model(&model);
                let result = client
                    .exec_chat(
                        ModelIden::new(LlmProvider::OpenAI, model.model.clone()),
                        ChatRequest::from_user("proxy policy chat payload"),
                        None,
                    )
                    .await;
                if mode == "chat" {
                    result.expect("回环 Chat 请求应成功");
                } else {
                    assert!(result.is_err(), "测试 proxy 应拒绝远端请求");
                }
            }
            "stt" => {
                let model = openai_model(ModelKind::Transcription, &endpoint);
                let input =
                    TranscriptionInput::new(vec![1_i16; 160]).expect("代理策略测试 PCM 输入应有效");
                transcribe(&model, input, AppLanguage::English)
                    .await
                    .expect("回环 STT 请求应成功");
            }
            "tts" => {
                let model = openai_model(ModelKind::SpeechSynthesis, &endpoint);
                synthesize(&model, "proxy policy speech payload")
                    .await
                    .expect("回环 TTS 请求应成功");
            }
            "websocket" => {
                let mut request = endpoint
                    .into_client_request()
                    .expect("回环 WebSocket endpoint 应可构造握手");
                request.headers_mut().insert(
                    "Authorization",
                    "Bearer websocket-test-key"
                        .parse()
                        .expect("测试 Authorization 应为有效 header"),
                );
                assert!(
                    connect_websocket_once(request).await.is_err(),
                    "测试回环服务应拒绝 WebSocket 升级"
                );
            }
            _ => panic!("未知代理策略子进程模式：{mode}"),
        }
    });
}

fn openai_model(kind: ModelKind, endpoint: &str) -> LlmModelConfig {
    LlmModelConfig {
        id: "redirect-test".to_owned(),
        label: "Redirect test".to_owned(),
        kind,
        provider: ModelProvider::Genai(LlmProvider::OpenAI),
        model: "test-model".to_owned(),
        endpoint: Some(endpoint.to_owned()),
        api_key: Some("test-key".to_owned()),
        voice: (kind == ModelKind::SpeechSynthesis).then(|| "alloy".to_owned()),
        voice_type: None,
        local_path: None,
        use_gpu: false,
        whisper_language: None,
        advanced: LlmAdvancedOptions::default(),
    }
    .normalized(AppLanguage::English)
    .expect("测试 Provider endpoint 应通过当前配置校验")
}

fn success_response(mode: &str) -> String {
    let (content_type, body) = match mode {
        "chat" => (
            "application/json",
            r#"{"model":"test-model","choices":[{"message":{"content":"ok"},"finish_reason":"stop"}]}"#,
        ),
        "stt" => ("application/json", r#"{"text":"ok"}"#),
        "tts" => ("application/octet-stream", "\0\0"),
        _ => panic!("未知测试响应模式：{mode}"),
    };
    format!(
        "HTTP/1.1 200 OK\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )
}

fn run_proxy_child(mode: &str, endpoint: &str, proxy: &str) -> Output {
    let executable = env::current_exe().expect("应可定位当前 Agent 测试二进制");
    let mut command = Command::new(executable);
    command
        .arg("--exact")
        .arg(PROXY_CHILD_TEST)
        .arg("--nocapture")
        .arg("--test-threads=1")
        .env(PROXY_CHILD_MODE_ENV, mode)
        .env(PROXY_CHILD_ENDPOINT_ENV, endpoint);
    for variable in [
        "HTTP_PROXY",
        "http_proxy",
        "ALL_PROXY",
        "all_proxy",
        "HTTPS_PROXY",
        "https_proxy",
        "NO_PROXY",
        "no_proxy",
        "REQUEST_METHOD",
    ] {
        command.env_remove(variable);
    }
    command.env("HTTP_PROXY", proxy).env("ALL_PROXY", proxy);
    command.output().expect("代理策略测试子进程应可启动")
}

fn assert_child_succeeded(mode: &str, output: &Output) {
    assert!(
        output.status.success(),
        "{mode} 代理策略子进程失败\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

fn spawn_listener(
    name: &str,
    listener: TcpListener,
    response: String,
    stats: Arc<RequestStats>,
    completed: Arc<AtomicBool>,
    stop_after_first: bool,
) -> JoinHandle<Result<(), String>> {
    thread::Builder::new()
        .name(name.to_owned())
        .spawn(move || serve_listener(listener, &response, &stats, &completed, stop_after_first))
        .expect("测试 HTTP listener 线程应可启动")
}

fn serve_listener(
    listener: TcpListener,
    response: &str,
    stats: &RequestStats,
    completed: &AtomicBool,
    stop_after_first: bool,
) -> Result<(), String> {
    let started_at = Instant::now();
    let mut completed_at = None;
    loop {
        match listener.accept() {
            Ok((mut stream, _)) => {
                stream
                    .set_nonblocking(false)
                    .map_err(|error| error.to_string())?;
                stats.requests.fetch_add(1, Ordering::SeqCst);
                let body_bytes = consume_request(&mut stream).map_err(|error| error.to_string())?;
                stats.body_bytes.fetch_add(body_bytes, Ordering::SeqCst);
                stream
                    .write_all(response.as_bytes())
                    .map_err(|error| error.to_string())?;
                stream.flush().map_err(|error| error.to_string())?;
                if stop_after_first {
                    return Ok(());
                }
            }
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {}
            Err(error) => return Err(error.to_string()),
        }

        if completed.load(Ordering::SeqCst) {
            let completed_at = completed_at.get_or_insert_with(Instant::now);
            if completed_at.elapsed() >= SERVER_GRACE {
                return Ok(());
            }
        }
        if started_at.elapsed() >= SERVER_HARD_TIMEOUT {
            return Err("测试 HTTP listener 等待超时".to_owned());
        }
        thread::sleep(LISTENER_POLL_INTERVAL);
    }
}

fn consume_request(stream: &mut TcpStream) -> io::Result<usize> {
    stream.set_read_timeout(Some(SERVER_IO_TIMEOUT))?;
    stream.set_write_timeout(Some(SERVER_IO_TIMEOUT))?;
    let headers = read_headers(stream)?;
    let body_length = content_length(&headers)?.unwrap_or(0);
    let mut remaining = body_length;
    let mut discard = [0_u8; 8 * 1024];
    while remaining > 0 {
        let capacity = remaining.min(discard.len());
        let read = stream.read(&mut discard[..capacity])?;
        if read == 0 {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "测试 HTTP 请求正文提前结束",
            ));
        }
        remaining -= read;
    }
    Ok(body_length)
}

fn read_headers(stream: &mut TcpStream) -> io::Result<Vec<u8>> {
    let mut headers = Vec::new();
    let mut byte = [0_u8; 1];
    while !headers.ends_with(b"\r\n\r\n") {
        if headers.len() >= MAX_HEADER_BYTES {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "测试 HTTP 请求头超过限制",
            ));
        }
        if stream.read(&mut byte)? == 0 {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "测试 HTTP 请求头提前结束",
            ));
        }
        headers.push(byte[0]);
    }
    Ok(headers)
}

fn content_length(headers: &[u8]) -> io::Result<Option<usize>> {
    let headers = std::str::from_utf8(headers)
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "测试 HTTP 请求头不是 UTF-8"))?;
    let Some(value) = headers
        .split("\r\n")
        .filter_map(|line| line.split_once(':'))
        .find(|(name, _)| name.eq_ignore_ascii_case("content-length"))
        .map(|(_, value)| value)
    else {
        return Ok(None);
    };
    value
        .trim()
        .parse()
        .map(Some)
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "测试 Content-Length 无效"))
}

fn join_listener(task: JoinHandle<Result<(), String>>, label: &str) {
    match task.join() {
        Ok(Ok(())) => {}
        Ok(Err(error)) => panic!("{label} 失败: {error}"),
        Err(_) => panic!("{label} 线程 panic"),
    }
}
