//! 集中构造 Provider 网络传输，并固定重定向与超时策略。

use std::time::Duration;

use tokio::net::TcpStream;
use tokio_tungstenite::{
    MaybeTlsStream, WebSocketStream, connect_async,
    tungstenite::{
        Error as WebSocketError,
        handshake::client::{Request, Response},
    },
};

use crate::config::endpoint_is_plaintext_loopback;

pub(crate) fn provider_http_client(
    endpoint: Option<&str>,
    connect_timeout: Duration,
    read_timeout: Duration,
    total_timeout: Duration,
) -> Result<reqwest::Client, reqwest::Error> {
    let mut builder = reqwest::Client::builder()
        .use_rustls_tls()
        .connect_timeout(connect_timeout)
        .read_timeout(read_timeout)
        .timeout(total_timeout)
        .redirect(reqwest::redirect::Policy::none());
    if endpoint.is_some_and(endpoint_is_plaintext_loopback) {
        // 明文回环请求携带凭据和正文，不能依赖用户或系统 NO_PROXY 配置来防止远端代理接管。
        builder = builder.no_proxy();
    }
    builder.build()
}

pub(crate) async fn connect_websocket_once(
    request: Request,
) -> Result<(WebSocketStream<MaybeTlsStream<TcpStream>>, Response), WebSocketError> {
    // tokio-tungstenite 0.28 在这里直接 `TcpStream::connect` 原 URI，不读取 HTTP_PROXY、
    // ALL_PROXY 或 NO_PROXY；3xx 会作为 `Error::Http` 返回。会自动重定向的是
    // tungstenite 的同步 `connect`，这里不能改用它。
    connect_async(request).await
}
