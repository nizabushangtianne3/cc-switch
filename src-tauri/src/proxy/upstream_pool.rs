//! 上游裸写连接的 keep-alive 池
//!
//! 直连主路径（[`super::hyper_client`] 的 `send_raw_request`）为了保证
//! header 顺序/大小写与 CLI 原始请求字节级一致，每请求手工写完整
//! HTTP/1.1 报文，hyper 只负责解析响应。之前这条路径每个请求都新建
//! TCP+TLS 连接，对真实上游等于每次多付一次握手（50-150ms/请求）。
//!
//! 本池把「响应体框架完整结束、hyper 确认可再发」的连接按目的地复用：
//! - 复用判定完全交给 hyper（`sender.is_ready()`：body 框架读完且上游
//!   允许 keep-alive），本模块不做任何报文框架推断；
//! - 任何失败路径（写入失败、parse 失败、client 中断）都整条丢弃，
//!   失败成本等于「新建连接」，不会比改造前更差；
//! - 条目带 TTL：上游侧 keep-alive 超时关连接是常态，过期条目直接
//!   丢弃新建，不做试探性写入。
//!
//! 连接的读写两侧经 `tokio::io::split` 分家：读半边由 hyper 的连接
//! 驱动独占（我们只往里喂字节），写半边留在池里写下一份手工报文。
//! 两侧不共享可变状态，没有互斥与唤醒的额外设计。

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use bytes::Bytes;
use hyper::client::conn::http1::SendRequest;

use super::hyper_client::BoxedUpstreamWrite;

/// 池内条目空闲多久后视为过期（上游 keep-alive 超时的常见值在 5-75s，
/// 取 60s 覆盖主流网关；过期连接直接丢弃新建，由 fallback 兜住）。
const ENTRY_TTL: Duration = Duration::from_secs(60);
/// 空闲连接总量的上限。超限后先淘汰过期条目，仍满则拒绝入池
/// （连接随条目丢弃而正常关闭，行为与改造前一致）。
const MAX_ENTRIES: usize = 32;

/// 池键：完整目的地（含上游代理隧道），不同代理不混用连接。
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) struct PoolKey {
    proxy: Option<String>,
    target: String,
}

impl PoolKey {
    pub(crate) fn new(proxy: Option<&str>, scheme: &str, host: &str, port: u16) -> Self {
        Self {
            proxy: proxy.map(str::to_string),
            target: format!("{scheme}://{host}:{port}"),
        }
    }
}

/// 一条可复用的裸写连接。
///
/// `write_half` 用来写下一份手工报文；`sender` 用来让 hyper 的连接
/// 驱动解析对应响应（hyper 对这条连接的写半边被丢弃，读半边由
/// 驱动独占）。两者都可 Clone，池、响应体回收钩子各持一份即可。
pub(crate) struct PooledUpstream {
    pub(crate) write_half: BoxedUpstreamWrite,
    pub(crate) sender: SendRequest<http_body_util::Full<Bytes>>,
    created: Instant,
}

impl PooledUpstream {
    pub(crate) fn new(
        write_half: BoxedUpstreamWrite,
        sender: SendRequest<http_body_util::Full<Bytes>>,
    ) -> Self {
        Self {
            write_half,
            sender,
            created: Instant::now(),
        }
    }

    /// hyper 自己说这条连接还能不能再发（body 读完且非 Connection: close）。
    pub(crate) fn is_reusable(&self) -> bool {
        self.sender.is_ready()
    }

    fn is_expired(&self) -> bool {
        self.created.elapsed() >= ENTRY_TTL
    }
}

fn pool() -> &'static Mutex<HashMap<PoolKey, Vec<PooledUpstream>>> {
    static POOL: OnceLock<Mutex<HashMap<PoolKey, Vec<PooledUpstream>>>> = OnceLock::new();
    POOL.get_or_init(|| Mutex::new(HashMap::new()))
}

/// 取出目的地对应的连接（若有且未过期、hyper 确认可复用）。
/// 取出即从池中移除：同一连接同一时刻只服务一个请求。
pub(crate) fn take(key: &PoolKey) -> Option<PooledUpstream> {
    let mut pool = pool().lock().expect("upstream pool lock");
    let entries = pool.get_mut(key)?;
    while let Some(entry) = entries.pop() {
        if !entry.is_expired() && entry.is_reusable() {
            if entries.is_empty() {
                pool.remove(key);
            }
            return Some(entry);
        }
        // 丢弃即关闭连接（读半边随驱动退出、写半边落 dropping），走新建。
    }
    pool.remove(key);
    None
}

/// 归还一条确认可复用的连接。
pub(crate) fn put(key: PoolKey, entry: PooledUpstream) {
    if !entry.is_reusable() {
        // hyper 判定不可复用（close / body 未读完），丢弃即正常关闭。
        return;
    }
    let mut pool = pool().lock().expect("upstream pool lock");
    let mut count = pool.values().map(Vec::len).sum::<usize>();
    if count >= MAX_ENTRIES {
        // 先清过期条目腾位；仍满则放弃入池（连接随条目关闭）。
        pool.retain(|_, entries| {
            entries.retain(|entry| !entry.is_expired());
            !entries.is_empty()
        });
        count = pool.values().map(Vec::len).sum::<usize>();
        if count >= MAX_ENTRIES {
            return;
        }
    }
    pool.entry(key).or_default().push(entry);
}

/// 仅供测试：把池清空，避免用例间串池。
#[cfg(test)]
pub(crate) fn clear_for_test() {
    pool().lock().expect("upstream pool lock").clear();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pool_keys_distinguish_proxy_and_target() {
        let key = PoolKey::new(None, "https", "same.example", 443);
        assert_eq!(key, PoolKey::new(None, "https", "same.example", 443));
        assert_ne!(
            key,
            PoolKey::new(Some("http://127.0.0.1:7890"), "https", "same.example", 443)
        );
        assert_ne!(key, PoolKey::new(None, "https", "same.example", 8443));
    }

    #[tokio::test]
    async fn take_returns_and_removes_the_entry() {
        super::clear_for_test();
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .expect("bind loopback");
        let addr = listener.local_addr().expect("local addr");
        // 只需让 connect 成功；连接上的数据无人读写，测试结束即关。
        let _accept = tokio::spawn(async move {
            let _ = listener.accept().await;
        });
        let stream = tokio::net::TcpStream::connect(addr).await.expect("connect");

        let key = PoolKey::new(None, "http", "127.0.0.1", addr.port());
        let (_read_half, write_half) = tokio::io::split(stream);
        // duplex 对端必须保活：对端被丢弃会让连接立即 EOF 关闭，
        // hyper 的 is_ready 随之变 false，池条目就会被拒绝。
        let (_server_io, client_io) = tokio::io::duplex(64 * 1024);
        let (sender, conn) =
            hyper::client::conn::http1::handshake::<_, http_body_util::Full<Bytes>>(
                hyper_util::rt::TokioIo::new(client_io),
            )
            .await
            .expect("handshake");
        // is_ready 由连接驱动的首次 poll 置位；生产路径在 do_hyper_response 里
        // spawn 驱动，这里保持一致。
        tokio::spawn(async move {
            let _ = conn.await;
        });
        // 单线程测试运行时：让出一次调度，驱动首轮 poll 置位 is_ready。
        tokio::task::yield_now().await;

        put(
            key.clone(),
            PooledUpstream::new(Box::new(write_half), sender),
        );
        assert!(take(&key).is_some());
        assert!(
            take(&key).is_none(),
            "take 必须移除条目，禁止在途连接被并发取走"
        );
    }

    #[tokio::test]
    async fn pool_retains_multiple_ready_entries_for_same_destination() {
        super::clear_for_test();
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .expect("bind loopback");
        let addr = listener.local_addr().expect("loopback address");
        let _accept =
            tokio::spawn(async move { while let Ok((_socket, _)) = listener.accept().await {} });

        let key = PoolKey::new(None, "http", "127.0.0.1", addr.port());
        let mut entries = Vec::new();
        let mut keepalive = Vec::new();
        for _ in 0..2 {
            let stream = tokio::net::TcpStream::connect(addr).await.expect("connect");
            let (_read_half, write_half) = tokio::io::split(stream);
            let (server_io, client_io) = tokio::io::duplex(64 * 1024);
            let (sender, conn) = hyper::client::conn::http1::handshake::<
                _,
                http_body_util::Full<Bytes>,
            >(hyper_util::rt::TokioIo::new(client_io))
            .await
            .expect("handshake");
            tokio::spawn(async move {
                let _ = conn.await;
            });
            tokio::task::yield_now().await;
            entries.push(PooledUpstream::new(Box::new(write_half), sender));
            keepalive.push(server_io);
        }

        for entry in entries {
            put(key.clone(), entry);
        }
        assert!(take(&key).is_some(), "first pooled entry must be available");
        assert!(
            take(&key).is_some(),
            "concurrent requests to one destination must each get a pooled connection"
        );
        assert!(
            take(&key).is_none(),
            "pooled entries must not be duplicated"
        );
    }
}
