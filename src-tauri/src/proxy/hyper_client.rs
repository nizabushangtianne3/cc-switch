//! Hyper-based HTTP client for proxy forwarding
//!
//! Uses raw TCP/TLS writes to preserve exact original header name casing.
//! Supports HTTP CONNECT tunneling through upstream proxies.
//! Falls back to hyper-util Client (title-case headers) when raw write is not feasible.

use super::upstream_pool::{self, PoolKey, PooledUpstream};
use super::ProxyError;
use bytes::Bytes;
use futures::{stream::Stream, StreamExt};
use http_body_util::BodyExt;
use hyper::body::{Frame, Incoming};
use hyper_rustls::HttpsConnectorBuilder;
use hyper_util::{client::legacy::Client, rt::TokioExecutor};
use std::pin::Pin;
use std::sync::{Arc, OnceLock};
use std::task::{ready, Context, Poll};

/// Our own header case map: maps lowercase header name → original wire-casing bytes.
///
/// This is a backup mechanism independent of hyper's internal `HeaderCaseMap` (which is
/// `pub(crate)` and cannot be directly inspected or constructed from outside hyper).
///
/// Populated in `server.rs` by peeking at raw TCP bytes before hyper parses them.
/// Used in `send_request` to manually write headers with original casing when hyper's
/// own mechanism fails.
#[derive(Clone, Debug, Default)]
pub(crate) struct OriginalHeaderCases {
    /// Ordered list of (lowercase_name, original_wire_bytes) pairs.
    /// Multiple entries with the same name are allowed (for repeated headers).
    pub cases: Vec<(String, Vec<u8>)>,
}

impl OriginalHeaderCases {
    /// Parse raw HTTP request bytes (from TcpStream::peek) to extract original header casings.
    pub fn from_raw_bytes(buf: &[u8]) -> Self {
        let mut headers_buf = [httparse::EMPTY_HEADER; 128];
        let mut req = httparse::Request::new(&mut headers_buf);
        // We don't care if parsing is partial — we just want the header names we can get
        let _ = req.parse(buf);

        let mut cases = Vec::new();
        for header in req.headers.iter() {
            if header.name.is_empty() {
                break;
            }
            cases.push((
                header.name.to_ascii_lowercase(),
                header.name.as_bytes().to_vec(),
            ));
        }

        Self { cases }
    }
}

/// Connection-level extension type so per-request clones stay cheap.
pub(crate) type SharedOriginalHeaderCases = Arc<OriginalHeaderCases>;

/// 裸写连接的写半边（类型按上游协议在分支处装箱：TLS 隧道与明文 TCP 的半边类型不同）。
pub(crate) type BoxedUpstreamWrite = Box<dyn tokio::io::AsyncWrite + Unpin + Send>;
pub(crate) type BoxedUpstreamRead = Box<dyn tokio::io::AsyncRead + Unpin + Send>;

type HyperClient = Client<
    hyper_rustls::HttpsConnector<hyper_util::client::legacy::connect::HttpConnector>,
    http_body_util::Full<Bytes>,
>;

/// Lazily-initialized hyper client with header-case preservation enabled.
fn global_hyper_client() -> &'static HyperClient {
    static CLIENT: OnceLock<HyperClient> = OnceLock::new();
    CLIENT.get_or_init(|| {
        let connector = HttpsConnectorBuilder::new()
            .with_webpki_roots()
            .https_or_http()
            .enable_http1()
            .build();

        Client::builder(TokioExecutor::new())
            .http1_preserve_header_case(true)
            .http1_title_case_headers(true)
            .build(connector)
    })
}

/// 响应体读取上限（128 MiB）。正常非流式补全响应只有几十到几百 KiB；超过则视为
/// 上游异常或恶意 payload，直接拒绝，避免代理进程被超大响应体/压缩炸弹耗尽内存。
pub(crate) const MAX_RESPONSE_BODY_BYTES: usize = 128 * 1024 * 1024;

/// Unified response wrapper that can hold either a hyper or reqwest response.
///
/// The hyper variant is used for the main (direct) path with header-case preservation.
/// The reqwest variant is the fallback when an upstream HTTP/SOCKS5 proxy is configured.
pub enum ProxyResponse {
    Hyper(hyper::Response<PoolBody>),
    Reqwest(reqwest::Response),
    Buffered {
        status: http::StatusCode,
        headers: http::HeaderMap,
        body: Bytes,
    },
    Streamed {
        status: http::StatusCode,
        headers: http::HeaderMap,
        stream: std::pin::Pin<Box<dyn Stream<Item = Result<Bytes, std::io::Error>> + Send>>,
    },
}

/// 裸写连接的响应体：框架读完时把连接归还 keep-alive 池。
///
/// Linux 上连接驱动可能比 body reader 晚一轮才把 sender 置 ready。body
/// 读完但尚未 ready 时先返回 Pending，等 hyper 驱动推进后再归还；连接
/// close / 中断时 `poll_ready` 会报错，连接随各半边落地正常关闭。
pub(crate) struct PoolBody {
    inner: Pin<Box<Incoming>>,
    release: Option<PoolRelease>,
}

/// 连接归还句柄：写半边 + hyper 解析句柄 + 池键。
struct PoolRelease {
    key: PoolKey,
    write_half: BoxedUpstreamWrite,
    sender: hyper::client::conn::http1::SendRequest<http_body_util::Full<Bytes>>,
}

impl PoolRelease {
    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), hyper::Error>> {
        self.sender.poll_ready(cx)
    }

    /// 只在 hyper 确认可复用时入池；不可复用（close / body 未读完就中断）
    /// 时丢弃，连接随各半边落地正常关闭。
    fn release(self) {
        let entry = upstream_pool::PooledUpstream::new(self.write_half, self.sender);
        if entry.is_reusable() {
            upstream_pool::put(self.key, entry);
        }
    }
}

impl PoolBody {
    fn drop_release(&mut self) {
        self.release = None;
    }
}

impl http_body::Body for PoolBody {
    type Data = Bytes;
    type Error = hyper::Error;

    fn poll_frame(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Self::Data>, Self::Error>>> {
        let this = self.get_mut();
        let frame = ready!(this.inner.as_mut().poll_frame(cx));
        match &frame {
            None => {
                let release_result = match this.release.as_mut() {
                    Some(release) => match release.poll_ready(cx) {
                        Poll::Ready(result) => Some(result),
                        Poll::Pending => return Poll::Pending,
                    },
                    None => None,
                };
                match release_result {
                    Some(Ok(())) => {
                        if let Some(release) = this.release.take() {
                            release.release();
                        }
                    }
                    // 连接已坏：显式放弃复用，避免半死连接被再次取出
                    Some(Err(_)) => this.drop_release(),
                    None => {}
                }
            }
            // 连接已坏：显式放弃复用，避免半死连接被再次取出
            Some(Err(_)) => {
                this.drop_release();
            }
            _ => {}
        }
        Poll::Ready(frame)
    }

    fn is_end_stream(&self) -> bool {
        self.inner.is_end_stream()
    }

    fn size_hint(&self) -> http_body::SizeHint {
        self.inner.size_hint()
    }
}

impl Drop for PoolBody {
    fn drop(&mut self) {
        // 未走到 poll_frame(None) 就被丢弃（客户端断开、上游错误）时，
        // 不等待 driver，也不能把连接送回池，直接正常关闭。
        self.drop_release();
    }
}

impl ProxyResponse {
    pub fn buffered(status: http::StatusCode, headers: http::HeaderMap, body: Bytes) -> Self {
        Self::Buffered {
            status,
            headers,
            body,
        }
    }

    pub fn streamed(
        status: http::StatusCode,
        headers: http::HeaderMap,
        stream: impl Stream<Item = Result<Bytes, std::io::Error>> + Send + 'static,
    ) -> Self {
        Self::Streamed {
            status,
            headers,
            stream: Box::pin(stream),
        }
    }

    pub fn status(&self) -> http::StatusCode {
        match self {
            Self::Hyper(r) => r.status(),
            Self::Reqwest(r) => r.status(),
            Self::Buffered { status, .. } | Self::Streamed { status, .. } => *status,
        }
    }

    pub fn headers(&self) -> &http::HeaderMap {
        match self {
            Self::Hyper(r) => r.headers(),
            Self::Reqwest(r) => r.headers(),
            Self::Buffered { headers, .. } | Self::Streamed { headers, .. } => headers,
        }
    }

    /// Shortcut: extract `content-type` header value as `&str`.
    pub fn content_type(&self) -> Option<&str> {
        self.headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok())
    }

    /// Check if the response is an SSE stream.
    pub fn is_sse(&self) -> bool {
        self.content_type()
            .map(|ct| ct.contains("text/event-stream"))
            .unwrap_or(false)
    }

    /// Check whether the response explicitly declares a JSON media type.
    pub fn is_json(&self) -> bool {
        self.content_type()
            .map(|content_type| {
                let media_type = content_type
                    .split(';')
                    .next()
                    .unwrap_or("")
                    .trim()
                    .to_ascii_lowercase();
                media_type == "application/json" || media_type.ends_with("+json")
            })
            .unwrap_or(false)
    }

    /// Consume the response and collect the full body into `Bytes`, aborting the
    /// read as soon as the accumulated body exceeds `max_bytes`.
    ///
    /// 所有变体都在累积过程中逐块检查、超限即断开（drop stream 中止上游连接），
    /// 而不是先收满再比较——否则超大明文 body 仍会完整进入内存，限制形同虚设。
    pub async fn bytes_with_limit(self, max_bytes: usize) -> Result<Bytes, ProxyError> {
        match self {
            Self::Buffered { body, .. } => {
                // 调用方已把 body 完整缓冲，无法中途截停，只能事后比较
                if body.len() > max_bytes {
                    return Err(ProxyError::ResponseBodyTooLarge(body.len()));
                }
                Ok(body)
            }
            response => {
                // Hyper / Reqwest / Streamed 统一走逐块流式累积，超预算立即报错
                let mut stream = response.bytes_stream();
                let mut body = bytes::BytesMut::new();
                while let Some(chunk) = stream.next().await {
                    let chunk = chunk.map_err(|e| {
                        ProxyError::ForwardFailed(format!("Failed to read response body: {e}"))
                    })?;
                    if body.len() + chunk.len() > max_bytes {
                        return Err(ProxyError::ResponseBodyTooLarge(body.len() + chunk.len()));
                    }
                    body.extend_from_slice(&chunk);
                }
                Ok(body.freeze())
            }
        }
    }

    /// Consume the response and return a byte-chunk stream (for SSE pass-through).
    pub fn bytes_stream(self) -> impl Stream<Item = Result<Bytes, std::io::Error>> + Send {
        use futures::StreamExt;

        match self {
            Self::Hyper(r) => {
                let body = r.into_body();
                let stream = futures::stream::unfold(body, |mut body| async {
                    match body.frame().await {
                        Some(Ok(frame)) => {
                            if let Ok(data) = frame.into_data() {
                                if data.is_empty() {
                                    Some((Ok(Bytes::new()), body))
                                } else {
                                    Some((Ok(data), body))
                                }
                            } else {
                                Some((Ok(Bytes::new()), body))
                            }
                        }
                        Some(Err(e)) => Some((
                            Err(std::io::Error::other(super::error::describe_error_chain(
                                &e,
                            ))),
                            body,
                        )),
                        None => None,
                    }
                })
                .filter(|result| {
                    futures::future::ready(!matches!(result, Ok(ref b) if b.is_empty()))
                });
                Box::pin(stream)
                    as std::pin::Pin<Box<dyn Stream<Item = Result<Bytes, std::io::Error>> + Send>>
            }
            Self::Reqwest(r) => {
                let stream = r.bytes_stream().map(|r| {
                    r.map_err(|e| std::io::Error::other(super::error::describe_reqwest_error(e)))
                });
                Box::pin(stream)
            }
            Self::Buffered { body, .. } => Box::pin(futures::stream::once(async move { Ok(body) }))
                as std::pin::Pin<Box<dyn Stream<Item = Result<Bytes, std::io::Error>> + Send>>,
            Self::Streamed { stream, .. } => stream,
        }
    }
}

/// Send an HTTP request with header-case preservation.
///
/// Uses a two-tier strategy:
/// 1. Primary: raw HTTP/1.1 write via TLS stream with exact original header casing
///    (from `OriginalHeaderCases` captured by peek in server.rs), then hand off to
///    hyper for response parsing.
/// 2. Fallback: hyper-util Client with `title_case_headers(true)` when raw write
///    isn't feasible (e.g., missing original cases).
///
/// The caller is expected to include `Host` in the supplied `headers` at the
/// correct position.
///
/// `proxy_url`: optional upstream HTTP proxy URL (e.g. `http://127.0.0.1:7890`).
/// When set, the raw write path uses HTTP CONNECT tunneling through the proxy,
/// so header-case preservation works even when an upstream proxy is configured.
///
/// `log_display` is a caller-supplied, already-sanitized string used only for
/// logging; this layer never derives a log value from the raw `uri`.
#[allow(clippy::too_many_arguments)]
pub async fn send_request(
    uri: http::Uri,
    log_display: &str,
    method: &http::Method,
    headers: http::HeaderMap,
    original_extensions: &http::Extensions,
    body: Bytes,
    timeout: std::time::Duration,
    proxy_url: Option<&str>,
) -> Result<ProxyResponse, ProxyError> {
    // Extract our own OriginalHeaderCases if available
    let original_cases = original_extensions.get::<SharedOriginalHeaderCases>();
    let has_cases = original_cases
        .as_ref()
        .is_some_and(|cases| !cases.cases.is_empty());
    log::debug!(
        "[HyperClient] Sending request: target={}, header_count={}, \
         has_host={}, has_original_cases={has_cases}, proxy={:?}",
        log_display,
        headers.len(),
        headers.contains_key(http::header::HOST),
        proxy_url.map(super::http_client::mask_url),
    );

    if let Some(original_cases) = original_cases.filter(|cases| !cases.cases.is_empty()) {
        // Primary path: use raw write + hyper handshake for exact header casing
        let result = tokio::time::timeout(
            timeout,
            send_raw_request(&uri, method, &headers, original_cases, &body, proxy_url),
        )
        .await
        .map_err(|_| ProxyError::Timeout(format!("请求超时: {}s", timeout.as_secs())))?;

        match result {
            Ok(resp) => return Ok(resp),
            Err(e) => {
                if proxy_url.is_some() {
                    // Don't bypass configured proxy with direct connect fallback
                    return Err(e);
                }
                log::warn!("[HyperClient] Raw write failed, falling back to hyper-util: {e}");
                // Fall through to hyper-util Client
            }
        }
    }

    // Fallback: hyper-util Client (title-case headers, no proxy support)
    let mut req = http::Request::builder()
        .method(method.clone())
        .uri(&uri)
        .body(http_body_util::Full::new(body))
        .map_err(|e| ProxyError::ForwardFailed(format!("Failed to build request: {e}")))?;

    *req.headers_mut() = headers;
    *req.extensions_mut() = original_extensions.clone();

    let client = global_hyper_client();
    let resp = tokio::time::timeout(timeout, client.request(req))
        .await
        .map_err(|_| ProxyError::Timeout(format!("请求超时: {}s", timeout.as_secs())))?
        .map_err(|e| {
            ProxyError::ForwardFailed(format!(
                "上游请求失败: {}",
                super::error::describe_error_chain(&e)
            ))
        })?;

    Ok(wrap_hyper_response(resp, None))
}

/// TCP or TLS stream returned by `connect_via_proxy`.
///
/// When the proxy URL uses `https://`, the connection to the proxy itself is
/// TLS-wrapped before sending the CONNECT request.  The enum lets
/// `send_raw_request` work with either variant generically.
pub(crate) enum ProxyStream {
    Tcp(tokio::net::TcpStream),
    Tls(Box<tokio_rustls::client::TlsStream<tokio::net::TcpStream>>),
}

impl tokio::io::AsyncRead for ProxyStream {
    fn poll_read(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &mut tokio::io::ReadBuf<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        match self.get_mut() {
            ProxyStream::Tcp(s) => std::pin::Pin::new(s).poll_read(cx, buf),
            ProxyStream::Tls(s) => std::pin::Pin::new(s).poll_read(cx, buf),
        }
    }
}

impl tokio::io::AsyncWrite for ProxyStream {
    fn poll_write(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &[u8],
    ) -> std::task::Poll<std::io::Result<usize>> {
        match self.get_mut() {
            ProxyStream::Tcp(s) => std::pin::Pin::new(s).poll_write(cx, buf),
            ProxyStream::Tls(s) => std::pin::Pin::new(s).poll_write(cx, buf),
        }
    }

    fn poll_flush(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        match self.get_mut() {
            ProxyStream::Tcp(s) => std::pin::Pin::new(s).poll_flush(cx),
            ProxyStream::Tls(s) => std::pin::Pin::new(s).poll_flush(cx),
        }
    }

    fn poll_shutdown(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        match self.get_mut() {
            ProxyStream::Tcp(s) => std::pin::Pin::new(s).poll_shutdown(cx),
            ProxyStream::Tls(s) => std::pin::Pin::new(s).poll_shutdown(cx),
        }
    }
}

/// Send request via raw TCP/TLS with exact original header casing.
///
/// When `proxy_url` is provided, establishes an HTTP CONNECT tunnel through
/// the proxy first, then performs TLS + raw write through the tunnel.
/// This preserves header casing even when an upstream proxy is configured.
///
/// 连接经 [`upstream_pool`] 按 keep-alive 复用：命中池时直接把下一份
/// 手工报文写进既有连接；池条目失效（写入失败/解析失败）时的语义与
/// 「新建连接中途失败」完全一致，交给 forwarder 的既有重试处理，
/// 这里不做静默重发（POST 不能盲重放）。
async fn send_raw_request(
    uri: &http::Uri,
    method: &http::Method,
    headers: &http::HeaderMap,
    original_cases: &OriginalHeaderCases,
    body: &[u8],
    proxy_url: Option<&str>,
) -> Result<ProxyResponse, ProxyError> {
    use tokio::io::AsyncWriteExt;

    let scheme = uri.scheme_str().unwrap_or("https");
    let host = uri
        .host()
        .ok_or_else(|| ProxyError::ForwardFailed("URI has no host".into()))?;
    let port = uri
        .port_u16()
        .unwrap_or(if scheme == "https" { 443 } else { 80 });
    let path_and_query = uri.path_and_query().map(|pq| pq.as_str()).unwrap_or("/");

    // Build the raw HTTP request head only; the caller writes the body directly
    // so a large JSON payload is not copied into yet another allocation.
    let raw_head = build_raw_head(method, path_and_query, headers, original_cases, body.len());
    let key = PoolKey::new(proxy_url, scheme, host, port);

    let mut request_buffers = [
        std::io::IoSlice::new(&raw_head),
        std::io::IoSlice::new(body),
    ];

    async fn write_all_vectored<W>(
        writer: &mut W,
        mut buffers: &mut [std::io::IoSlice<'_>],
    ) -> Result<(), ProxyError>
    where
        W: tokio::io::AsyncWrite + Unpin,
    {
        while !buffers.is_empty() {
            let written = writer
                .write_vectored(buffers)
                .await
                .map_err(|e| ProxyError::ForwardFailed(format!("Write failed: {e}")))?;
            if written == 0 {
                return Err(ProxyError::ForwardFailed("Write failed: EOF".into()));
            }
            std::io::IoSlice::advance_slices(&mut buffers, written);
        }
        Ok(())
    }

    // 池命中：写下一份手工报文，让既有 hyper 连接驱动解析响应。
    if let Some(pooled) = upstream_pool::take(&key) {
        let PooledUpstream {
            mut write_half,
            mut sender,
            ..
        } = pooled;
        let sent = async {
            write_all_vectored(&mut write_half, &mut request_buffers).await?;
            write_half
                .flush()
                .await
                .map_err(|e| ProxyError::ForwardFailed(format!("Write failed: {e}")))?;
            parse_hyper_response(&mut sender, method.clone()).await
        };
        match sent.await {
            Ok(resp) => {
                let release = PoolRelease {
                    key,
                    write_half,
                    sender,
                };
                return Ok(wrap_hyper_response(resp, Some(release)));
            }
            Err(e) => {
                log::debug!("[HyperClient] pooled connection unusable, reconnecting: {e}");
            }
        }
    }

    // Establish TCP connection — either direct or through HTTP CONNECT proxy
    let stream = if let Some(proxy) = proxy_url {
        connect_via_proxy(proxy, host, port).await?
    } else {
        ProxyStream::Tcp(
            tokio::net::TcpStream::connect((host, port))
                .await
                .map_err(|e| ProxyError::ForwardFailed(format!("TCP connect failed: {e}")))?,
        )
    };

    // TLS 握手在连接级完成后再分家：握手需要完整的读写能力。
    // TLS 隧道与明文 TCP 的半边类型不同，此处统一装箱成 trait 对象。
    let (read_half, mut write_half): (BoxedUpstreamRead, BoxedUpstreamWrite) = if scheme == "https"
    {
        let tls_connector = global_tls_connector();
        let server_name = rustls::pki_types::ServerName::try_from(host.to_string())
            .map_err(|e| ProxyError::ForwardFailed(format!("Invalid server name: {e}")))?;
        let tls_stream = tls_connector
            .connect(server_name, stream)
            .await
            .map_err(|e| ProxyError::ForwardFailed(format!("TLS handshake failed: {e}")))?;
        let (read, write) = tokio::io::split(tls_stream);
        (Box::new(read), Box::new(write))
    } else {
        let (read, write) = tokio::io::split(stream);
        (Box::new(read), Box::new(write))
    };

    // 读半边交给 hyper 的连接驱动解析响应；写半边写手工报文并入池。
    let reader = RawPeerReader { read: read_half };

    write_all_vectored(&mut write_half, &mut request_buffers).await?;
    write_half
        .flush()
        .await
        .map_err(|e| ProxyError::ForwardFailed(format!("Write failed: {e}")))?;

    let (mut sender, conn) = hyper::client::conn::http1::Builder::new()
        .preserve_header_case(true)
        .handshake::<_, http_body_util::Full<Bytes>>(hyper_util::rt::TokioIo::new(reader))
        .await
        .map_err(|e| ProxyError::ForwardFailed(format!("Handshake failed: {e}")))?;

    // Spawn the connection driver (reads responses from the stream)
    tokio::spawn(async move {
        if let Err(e) = conn.await {
            log::debug!("[HyperClient] raw conn driver error: {e}");
        }
    });

    let resp = parse_hyper_response(&mut sender, method.clone()).await?;
    let release = PoolRelease {
        key,
        write_half,
        sender,
    };
    Ok(wrap_hyper_response(resp, Some(release)))
}

/// Establish a connection through an HTTP CONNECT proxy tunnel.
///
/// 1. Connect TCP to the proxy server (TLS-wrapped when `https://` proxy)
/// 2. Send `CONNECT host:port HTTP/1.1` with optional `Proxy-Authorization`
/// 3. Read the proxy's 200 response (407 → `AuthError`)
/// 4. Return the tunneled stream (ready for target TLS handshake + raw write)
async fn connect_via_proxy(
    proxy_url: &str,
    target_host: &str,
    target_port: u16,
) -> Result<ProxyStream, ProxyError> {
    use base64::Engine;
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

    let parsed = url::Url::parse(proxy_url)
        .map_err(|e| ProxyError::ForwardFailed(format!("Invalid proxy URL: {e}")))?;

    let proxy_host = parsed
        .host_str()
        .ok_or_else(|| ProxyError::ForwardFailed("Proxy URL has no host".into()))?;
    let proxy_port = parsed
        .port()
        .unwrap_or(if parsed.scheme() == "https" { 443 } else { 80 });

    // Build Proxy-Authorization header if credentials are present
    let proxy_auth = if !parsed.username().is_empty() {
        let password = parsed.password().unwrap_or("");
        let credentials = format!("{}:{}", parsed.username(), password);
        let encoded = base64::engine::general_purpose::STANDARD.encode(credentials);
        Some(format!("Proxy-Authorization: Basic {encoded}\r\n"))
    } else {
        None
    };

    // Connect to the proxy
    let tcp = tokio::net::TcpStream::connect((proxy_host, proxy_port))
        .await
        .map_err(|e| ProxyError::ForwardFailed(format!("Proxy TCP connect failed: {e}")))?;

    // Wrap with TLS if the proxy URL uses https://
    let mut stream: ProxyStream = if parsed.scheme() == "https" {
        let tls_connector = global_tls_connector();
        let server_name = rustls::pki_types::ServerName::try_from(proxy_host.to_string())
            .map_err(|e| ProxyError::ForwardFailed(format!("Invalid proxy server name: {e}")))?;
        let tls_stream = tls_connector
            .connect(server_name, tcp)
            .await
            .map_err(|e| ProxyError::ForwardFailed(format!("Proxy TLS handshake failed: {e}")))?;
        ProxyStream::Tls(Box::new(tls_stream))
    } else {
        ProxyStream::Tcp(tcp)
    };

    // Send CONNECT request
    let mut connect_req = format!(
        "CONNECT {target_host}:{target_port} HTTP/1.1\r\n\
         Host: {target_host}:{target_port}\r\n"
    );
    if let Some(auth) = &proxy_auth {
        connect_req.push_str(auth);
    }
    connect_req.push_str("\r\n");

    stream
        .write_all(connect_req.as_bytes())
        .await
        .map_err(|e| ProxyError::ForwardFailed(format!("CONNECT write failed: {e}")))?;
    stream
        .flush()
        .await
        .map_err(|e| ProxyError::ForwardFailed(format!("CONNECT flush failed: {e}")))?;

    // Read the proxy's response status line
    let mut reader = BufReader::new(&mut stream);
    let mut status_line = String::new();
    reader
        .read_line(&mut status_line)
        .await
        .map_err(|e| ProxyError::ForwardFailed(format!("CONNECT read failed: {e}")))?;

    // Expect "HTTP/1.1 200 ..." or "HTTP/1.0 200 ..."
    if !status_line.contains(" 200 ") {
        if status_line.contains(" 407 ") {
            return Err(ProxyError::AuthError(format!(
                "Proxy authentication required (407): {}",
                status_line.trim()
            )));
        }
        return Err(ProxyError::ForwardFailed(format!(
            "Proxy CONNECT rejected: {}",
            status_line.trim()
        )));
    }

    // Drain remaining response headers (until empty line)
    loop {
        let mut line = String::new();
        reader
            .read_line(&mut line)
            .await
            .map_err(|e| ProxyError::ForwardFailed(format!("CONNECT header read: {e}")))?;
        if line.trim().is_empty() {
            break;
        }
    }
    // BufReader might have buffered data; drop it to get raw stream back.
    // Since CONNECT response is headers-only (no body), and we read until \r\n\r\n,
    // the BufReader buffer should be empty at this point.
    drop(reader);

    log::debug!(
        "[HyperClient] CONNECT tunnel established via {proxy_host}:{proxy_port} -> {target_host}:{target_port}"
    );

    Ok(stream)
}

/// Lazily-initialized TLS connector for raw connections.
///
/// Loads both webpki roots AND native system certificates so that
/// proxy MITM CAs (e.g. Clash, mitmproxy) installed in the system
/// keychain are trusted through the CONNECT tunnel.
fn global_tls_connector() -> &'static tokio_rustls::TlsConnector {
    static CONNECTOR: OnceLock<tokio_rustls::TlsConnector> = OnceLock::new();
    CONNECTOR.get_or_init(|| {
        let mut root_store = rustls::RootCertStore::empty();
        // Baseline: Mozilla/webpki roots
        root_store.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
        // Native system certs (includes user-installed proxy CAs)
        let native = rustls_native_certs::load_native_certs();
        let (added, _errors) = root_store.add_parsable_certificates(native.certs);
        log::debug!("[HyperClient] TLS root store: webpki + {added} native certs");
        let config = rustls::ClientConfig::builder()
            .with_root_certificates(root_store)
            .with_no_client_auth();
        tokio_rustls::TlsConnector::from(std::sync::Arc::new(config))
    })
}

/// Build the raw HTTP/1.1 request head with original header casing.
fn build_raw_head(
    method: &http::Method,
    path_and_query: &str,
    headers: &http::HeaderMap,
    original_cases: &OriginalHeaderCases,
    body_len: usize,
) -> Vec<u8> {
    let mut raw = Vec::with_capacity(4096);

    // Request line
    raw.extend_from_slice(method.as_str().as_bytes());
    raw.extend_from_slice(b" ");
    raw.extend_from_slice(path_and_query.as_bytes());
    raw.extend_from_slice(b" HTTP/1.1\r\n");

    // Headers with original casing, emitted in original wire order.
    //
    // Strategy:
    // 1. Walk `original_cases.cases` in order — this preserves the exact
    //    header sequence the client sent.  For each entry, emit the stored
    //    original-casing name plus the current value from `headers` (the
    //    proxy may have rewritten the value, e.g. Authorization).
    //    Repeated headers with the same name are handled by tracking a
    //    per-name value cursor so we step through `get_all()` in order.
    // 2. After the original headers, append any headers that exist in
    //    `headers` but were not present in the original request (i.e. added
    //    by the proxy).  These are emitted in lowercase.
    //
    // This replaces the old `for name in headers.keys()` loop which iterated
    // in hash-map order, destroying the original header sequence.
    let mut emitted: Vec<&str> = Vec::with_capacity(original_cases.cases.len());
    // Per-name cursor: how many values we have already emitted for each name.
    let mut value_cursor: std::collections::HashMap<&str, usize> =
        std::collections::HashMap::with_capacity(original_cases.cases.len());

    for (lower_name, orig_name_bytes) in &original_cases.cases {
        if let Ok(header_name) = http::header::HeaderName::from_bytes(lower_name.as_bytes()) {
            let all_values: Vec<_> = headers.get_all(&header_name).iter().collect();
            let cursor = value_cursor.entry(lower_name.as_str()).or_insert(0);
            if let Some(value) = all_values.get(*cursor) {
                raw.extend_from_slice(orig_name_bytes);
                raw.extend_from_slice(b": ");
                raw.extend_from_slice(value.as_bytes());
                raw.extend_from_slice(b"\r\n");
                *cursor += 1;
                emitted.push(lower_name.as_str());
            }
        }
    }

    // Append proxy-added headers (not present in the original request).
    for name in headers.keys() {
        if !emitted
            .iter()
            .any(|seen| seen.eq_ignore_ascii_case(name.as_str()))
        {
            for value in headers.get_all(name) {
                raw.extend_from_slice(name.as_str().as_bytes());
                raw.extend_from_slice(b": ");
                raw.extend_from_slice(value.as_bytes());
                raw.extend_from_slice(b"\r\n");
            }
            emitted.push(name.as_str());
        }
    }

    // Add Content-Length if not already present
    if !headers.contains_key(http::header::CONTENT_LENGTH) {
        raw.extend_from_slice(b"Content-Length: ");
        raw.extend_from_slice(body_len.to_string().as_bytes());
        raw.extend_from_slice(b"\r\n");
    }

    // End of headers; the body is written separately by the caller.
    raw.extend_from_slice(b"\r\n");

    raw
}

/// 把手工报文的响应交给 hyper 的连接驱动解析。
///
/// hyper 会尝试写自己的请求编码，[`RawPeerReader`] 将其丢弃；它随后从
/// 共享读半边读到上游对真实报文的响应并完成解析。
async fn parse_hyper_response(
    sender: &mut hyper::client::conn::http1::SendRequest<http_body_util::Full<Bytes>>,
    method: http::Method,
) -> Result<hyper::Response<Incoming>, ProxyError> {
    let dummy_req = http::Request::builder()
        .method(method)
        .uri("/")
        .body(http_body_util::Full::new(Bytes::new()))
        .map_err(|e| ProxyError::ForwardFailed(format!("Build dummy request: {e}")))?;

    sender
        .send_request(dummy_req)
        .await
        .map_err(|e| ProxyError::ForwardFailed(format!("Response parse failed: {e}")))
}

fn wrap_hyper_response(
    resp: hyper::Response<Incoming>,
    release: Option<PoolRelease>,
) -> ProxyResponse {
    let (parts, body) = resp.into_parts();
    ProxyResponse::Hyper(http::Response::from_parts(
        parts,
        PoolBody {
            inner: Box::pin(body),
            release,
        },
    ))
}

/// 裸写连接的读半边适配器：把 tokio split 出来的读半边交给 hyper 的
/// 连接驱动；hyper 对这条连接的写全部丢弃（真实报文由池里的写半边
/// 手工写出）。
struct RawPeerReader {
    read: Box<dyn tokio::io::AsyncRead + Unpin + Send>,
}

impl tokio::io::AsyncRead for RawPeerReader {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut tokio::io::ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.get_mut().read).poll_read(cx, buf)
    }
}

impl tokio::io::AsyncWrite for RawPeerReader {
    fn poll_write(
        self: Pin<&mut Self>,
        _cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        // Discard all writes — pretend they succeeded
        Poll::Ready(Ok(buf.len()))
    }

    fn poll_flush(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Poll::Ready(Ok(()))
    }

    fn poll_shutdown(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Poll::Ready(Ok(()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn buffered_with_content_type(content_type: Option<&str>) -> ProxyResponse {
        let mut headers = http::HeaderMap::new();
        if let Some(content_type) = content_type {
            headers.insert(
                http::header::CONTENT_TYPE,
                http::HeaderValue::from_str(content_type).unwrap(),
            );
        }
        ProxyResponse::buffered(http::StatusCode::OK, headers, Bytes::new())
    }

    #[test]
    fn json_content_type_detection_accepts_json_suffixes() {
        assert!(buffered_with_content_type(Some("application/json; charset=utf-8")).is_json());
        assert!(buffered_with_content_type(Some("application/problem+json")).is_json());
        assert!(!buffered_with_content_type(Some("text/event-stream")).is_json());
        assert!(!buffered_with_content_type(None).is_json());
    }

    #[test]
    fn build_raw_head_preserves_original_header_casing() {
        let cases =
            OriginalHeaderCases::from_raw_bytes(b"POST /x HTTP/1.1\r\nX-Cc-Switch: yes\r\n\r\n");
        let mut headers = http::HeaderMap::new();
        headers.insert("x-cc-switch", http::HeaderValue::from_static("yes"));

        let raw = build_raw_head(&http::Method::POST, "/x", &headers, &cases, 5);

        let text = String::from_utf8(raw).unwrap();
        assert!(
            text.starts_with("POST /x HTTP/1.1\r\nX-Cc-Switch: yes\r\nContent-Length: 5\r\n\r\n"),
            "{text}"
        );
    }

    #[test]
    fn build_raw_head_keeps_body_out_of_the_head_buffer() {
        let cases = OriginalHeaderCases::from_raw_bytes(
            b"POST /x HTTP/1.1\r\nContent-Type: application/json\r\n\r\n",
        );
        let mut headers = http::HeaderMap::new();
        headers.insert(
            http::header::CONTENT_TYPE,
            http::HeaderValue::from_static("application/json"),
        );

        let raw = build_raw_head(
            &http::Method::POST,
            "/x",
            &headers,
            &cases,
            b"large-request-body".len(),
        );
        let head = String::from_utf8(raw).unwrap();

        assert!(head.ends_with("Content-Length: 18\r\n\r\n"), "{head}");
        assert!(
            !head.contains("large-request-body"),
            "raw request builder must not copy the body into another allocation"
        );
    }

    #[tokio::test]
    async fn bytes_with_limit_rejects_oversized_buffered_response() {
        let oversized = Bytes::from(vec![0u8; MAX_RESPONSE_BODY_BYTES + 1]);
        let response =
            ProxyResponse::buffered(http::StatusCode::OK, http::HeaderMap::new(), oversized);

        let result = response.bytes_with_limit(MAX_RESPONSE_BODY_BYTES).await;
        assert!(matches!(result, Err(ProxyError::ResponseBodyTooLarge(_))));
    }

    #[tokio::test]
    async fn bytes_with_limit_rejects_oversized_streamed_response() {
        let (tx, rx) = tokio::sync::mpsc::channel::<Result<Bytes, std::io::Error>>(2);
        let stream = futures::stream::unfold(rx, |mut rx| async move {
            rx.recv().await.map(|item| (item, rx))
        });
        let response =
            ProxyResponse::streamed(http::StatusCode::OK, http::HeaderMap::new(), stream);

        tokio::spawn(async move {
            let _ = tx.send(Ok(Bytes::from(vec![0u8; 64 * 1024]))).await;
            let _ = tx
                .send(Ok(Bytes::from(vec![0u8; MAX_RESPONSE_BODY_BYTES])))
                .await;
        });

        let result = response.bytes_with_limit(MAX_RESPONSE_BODY_BYTES).await;
        assert!(matches!(result, Err(ProxyError::ResponseBodyTooLarge(_))));
    }

    /// 启动一个最小 HTTP/1.1 服务器：响应 `Content-Length: body_len` 的全零 body，
    /// 分块写出并统计实际写成功的字节数（客户端断开后写入失败即停）。
    async fn spawn_fixed_body_server(
        body_len: usize,
    ) -> (u16, std::sync::Arc<std::sync::atomic::AtomicUsize>) {
        use std::sync::atomic::{AtomicUsize, Ordering};
        use std::sync::Arc;
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let written = Arc::new(AtomicUsize::new(0));
        let written_report = written.clone();

        tokio::spawn(async move {
            let Ok((mut socket, _)) = listener.accept().await else {
                return;
            };
            // 读完请求头（内容不重要）
            let mut buf = [0u8; 4096];
            let mut filled = 0;
            loop {
                if buf[..filled].windows(4).any(|w| w == b"\r\n\r\n") {
                    break;
                }
                let Ok(n) = socket.read(&mut buf[filled..]).await else {
                    return;
                };
                if n == 0 {
                    return;
                }
                filled += n;
            }
            let header = format!(
                "HTTP/1.1 200 OK\r\ncontent-type: application/octet-stream\r\ncontent-length: {body_len}\r\nconnection: close\r\n\r\n"
            );
            if socket.write_all(header.as_bytes()).await.is_err() {
                return;
            }
            let chunk = [0u8; 16 * 1024];
            let mut remaining = body_len;
            while remaining > 0 {
                let n = remaining.min(chunk.len());
                if socket.write_all(&chunk[..n]).await.is_err() {
                    break;
                }
                written.fetch_add(n, Ordering::SeqCst);
                remaining -= n;
            }
        });

        (port, written_report)
    }

    /// 客户端断开到服务器写入失败之间有时延（loopback 缓冲区会再吞一部分），
    /// 稍等再读计数。只要客户端真的中途截停，服务器绝不可能写出大半个 body。
    async fn assert_server_aborted_early(
        written: std::sync::Arc<std::sync::atomic::AtomicUsize>,
        body_len: usize,
    ) {
        use std::sync::atomic::Ordering;
        tokio::time::sleep(std::time::Duration::from_millis(300)).await;
        let written = written.load(Ordering::SeqCst);
        assert!(
            written < body_len / 2,
            "客户端应在预算耗尽后立即断开，服务器不应写出大部分 body（实际已写 {written}/{body_len} 字节）"
        );
    }

    #[tokio::test]
    async fn bytes_with_limit_aborts_hyper_response_before_full_body_arrives() {
        const BODY_LEN: usize = 16 * 1024 * 1024;
        const LIMIT: usize = 64 * 1024;
        let (port, written) = spawn_fixed_body_server(BODY_LEN).await;

        // 构造真实的 hyper::Response<Incoming>：手工建立 http1 客户端连接
        let stream = tokio::net::TcpStream::connect(("127.0.0.1", port))
            .await
            .unwrap();
        let io = hyper_util::rt::TokioIo::new(stream);
        let (mut sender, conn) = hyper::client::conn::http1::handshake(io).await.unwrap();
        tokio::spawn(async move {
            let _ = conn.await;
        });
        let request = http::Request::builder()
            .uri(format!("http://127.0.0.1:{port}/"))
            .body(http_body_util::Empty::<Bytes>::new())
            .unwrap();
        let response = sender.send_request(request).await.unwrap();
        assert_eq!(response.status(), http::StatusCode::OK);

        let result = wrap_hyper_response(response, None)
            .bytes_with_limit(LIMIT)
            .await;
        assert!(matches!(result, Err(ProxyError::ResponseBodyTooLarge(_))));

        // 关键断言：若退回"先 collect 收满再比较"，服务器会把 16 MiB 全部写完
        assert_server_aborted_early(written, BODY_LEN).await;
    }

    #[tokio::test]
    async fn bytes_with_limit_aborts_reqwest_response_before_full_body_arrives() {
        const BODY_LEN: usize = 16 * 1024 * 1024;
        const LIMIT: usize = 64 * 1024;
        let (port, written) = spawn_fixed_body_server(BODY_LEN).await;

        let response = reqwest::Client::new()
            .get(format!("http://127.0.0.1:{port}/"))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), reqwest::StatusCode::OK);

        let result = ProxyResponse::Reqwest(response)
            .bytes_with_limit(LIMIT)
            .await;
        assert!(matches!(result, Err(ProxyError::ResponseBodyTooLarge(_))));

        assert_server_aborted_early(written, BODY_LEN).await;
    }

    /// 计数 TCP 连接的假上游：每 accept 一条连接计数 +1，响应固定 JSON。
    /// `close_after_first` 为 true 时第一条连接的响应带 `Connection: close`，
    /// 用于验证池不会复用上游声明关闭的连接。
    async fn counting_upstream(
        counter: Arc<std::sync::atomic::AtomicUsize>,
        close_after_first: bool,
    ) -> http::Uri {
        use std::sync::atomic::Ordering;

        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .expect("bind loopback");
        let addr = listener.local_addr().expect("local addr");
        tokio::spawn(async move {
            loop {
                let Ok((socket, _)) = listener.accept().await else {
                    break;
                };
                let seen = counter.fetch_add(1, Ordering::SeqCst) + 1;
                let close = close_after_first && seen == 1;
                tokio::spawn(async move {
                    let service = hyper::service::service_fn(
                        move |_req: hyper::Request<hyper::body::Incoming>| {
                            let mut resp = hyper::Response::new(http_body_util::Full::new(
                                Bytes::from_static(br#"{"ok":true}"#),
                            ));
                            if close {
                                resp.headers_mut().insert(
                                    http::header::CONNECTION,
                                    http::HeaderValue::from_static("close"),
                                );
                            }
                            async move { Ok::<_, std::convert::Infallible>(resp) }
                        },
                    );
                    let _ = hyper::server::conn::http1::Builder::new()
                        .serve_connection(hyper_util::rt::TokioIo::new(socket), service)
                        .await;
                });
            }
        });
        http::Uri::builder()
            .scheme("http")
            .authority(format!("127.0.0.1:{}", addr.port()))
            .path_and_query("/")
            .build()
            .expect("upstream uri")
    }

    fn post_extensions() -> (http::HeaderMap, http::Extensions) {
        let raw = b"POST / HTTP/1.1\r\nHost: upstream\r\nContent-Type: application/json\r\n\r\n";
        let mut headers = http::HeaderMap::new();
        headers.insert(
            http::header::HOST,
            http::HeaderValue::from_static("upstream"),
        );
        headers.insert(
            http::header::CONTENT_TYPE,
            http::HeaderValue::from_static("application/json"),
        );
        let mut extensions = http::Extensions::new();
        extensions.insert(Arc::new(OriginalHeaderCases::from_raw_bytes(raw)));
        (headers, extensions)
    }

    #[tokio::test]
    async fn send_request_reuses_the_upstream_connection_across_requests() {
        use std::sync::atomic::AtomicUsize;
        use std::sync::atomic::Ordering;

        super::upstream_pool::clear_for_test();
        let counter = Arc::new(AtomicUsize::new(0));
        let uri = counting_upstream(counter.clone(), false).await;
        let (headers, extensions) = post_extensions();

        for _ in 0..3 {
            let resp = send_request(
                uri.clone(),
                "test",
                &http::Method::POST,
                headers.clone(),
                &extensions,
                Bytes::from_static(br#"{}"#),
                std::time::Duration::from_secs(10),
                None,
            )
            .await
            .expect("request 1");
            let body = resp.bytes_with_limit(1024).await.expect("body");
            assert_eq!(&body[..], br#"{"ok":true}"#);
        }

        assert_eq!(
            counter.load(Ordering::SeqCst),
            1,
            "三次请求应复用同一条 TCP 连接"
        );
    }

    #[tokio::test]
    async fn send_request_does_not_reuse_connections_the_upstream_closes() {
        use std::sync::atomic::AtomicUsize;
        use std::sync::atomic::Ordering;

        super::upstream_pool::clear_for_test();
        let counter = Arc::new(AtomicUsize::new(0));
        let uri = counting_upstream(counter.clone(), true).await;
        let (headers, extensions) = post_extensions();

        for _ in 0..2 {
            let resp = send_request(
                uri.clone(),
                "test",
                &http::Method::POST,
                headers.clone(),
                &extensions,
                Bytes::from_static(br#"{}"#),
                std::time::Duration::from_secs(10),
                None,
            )
            .await
            .expect("request");
            let body = resp.bytes_with_limit(1024).await.expect("body");
            assert_eq!(&body[..], br#"{"ok":true}"#);
        }

        assert_eq!(
            counter.load(Ordering::SeqCst),
            2,
            "上游声明 Connection: close 时必须各建一条连接"
        );
    }
}
