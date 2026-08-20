use std::sync::Arc;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::oneshot;
use tokio_rustls::TlsAcceptor;
use tracing::{debug, error, info, warn};

use crate::server::session::Session;
use tunx_common::counting_io::CountingIo;
use tunx_common::metrics::ProxyMetrics;
use tunx_common::stream::WorkIo;

/// 组合 AsyncRead + AsyncWrite，便于用 Box<dyn> 统一 TLS/Tcp 流
trait Stream: AsyncRead + AsyncWrite {}
impl<T: AsyncRead + AsyncWrite> Stream for T {}

/// 启动 TCP 代理。
/// `tls_acceptor` 为 Some 时：该端口走 TLS 终止模式
///   - 收到 TLS ClientHello (首字节 0x16) → 走 TLS 握手 → QUIC 转发
///   - 收到明文 HTTP → 解析 Host 头 → 301 跳转到 https
pub async fn start_tcp_proxy(
    proxy_name: String,
    local_addr: String,
    remote_port: u16,
    session: Arc<Session>,
    tls_acceptor: Option<Arc<TlsAcceptor>>,
    mut shutdown_rx: oneshot::Receiver<()>,
    metrics: Arc<ProxyMetrics>,
) {
    let bind = format!("0.0.0.0:{remote_port}");
    let listener = match TcpListener::bind(&bind).await {
        Ok(l) => l,
        Err(e) => {
            error!(proxy = %proxy_name, "bind {bind} failed: {e}");
            return;
        }
    };
    if tls_acceptor.is_some() {
        info!(proxy = %proxy_name, "TCP+TLS proxy listening :{remote_port} → {local_addr}");
    } else {
        info!(proxy = %proxy_name, "TCP proxy listening :{remote_port} → {local_addr}");
    }

    loop {
        tokio::select! {
            accept = listener.accept() => {
                match accept {
                    Ok((tcp, peer)) => {
                        debug!(proxy = %proxy_name, %peer, "public connection");
                        let sess = Arc::clone(&session);
                        let name = proxy_name.clone();
                        let acceptor = tls_acceptor.clone();
                        let m = metrics.clone();
                        tokio::spawn(handle_public_conn(name, sess, tcp, acceptor, m));
                    }
                    Err(e) => warn!(proxy = %proxy_name, "accept: {e}"),
                }
            }
            _ = &mut shutdown_rx => {
                info!(proxy = %proxy_name, "shutting down");
                break;
            }
        }
    }
}

async fn handle_public_conn(
    proxy_name: String,
    session: Arc<Session>,
    public: TcpStream,
    tls_acceptor: Option<Arc<TlsAcceptor>>,
    metrics: Arc<ProxyMetrics>,
) {
    // 无 TLS：走原流程
    let public: Box<dyn Stream + Unpin + Send> = match tls_acceptor {
        Some(acc) => match dispatch_tls_or_http(&proxy_name, public, acc).await {
            Ok(DispatchResult::Tls(stream)) => stream,
            Ok(DispatchResult::Handled) => return, // HTTP 已 301 响应
            Err(e) => {
                warn!(proxy = %proxy_name, "tls/http dispatch: {e}");
                return;
            }
        },
        None => Box::new(public),
    };

    // 请求一条 WorkConn
    let work_rx = match session.request_work_conn(&proxy_name).await {
        Ok(rx) => rx,
        Err(e) => {
            warn!(proxy = %proxy_name, "request_work_conn: {e}");
            return;
        }
    };

    // 等待 client 开好 WorkConn（QUIC bi-stream 或 TCP+HTTP/2 stream，10s 超时）
    let work_io: Box<dyn WorkIo> =
        match tokio::time::timeout(std::time::Duration::from_secs(10), work_rx).await {
            Ok(Ok(io)) => io,
            Ok(Err(_)) => {
                warn!(proxy = %proxy_name, "work conn sender dropped");
                return;
            }
            Err(_) => {
                warn!(proxy = %proxy_name, "work conn timeout");
                return;
            }
        };

    metrics.conn_start();
    // 只在"公网 socket"一侧包计数器，避免在 work_io 侧重复计数
    let mut public = CountingIo::wrap_public_side(public, metrics.clone());
    let mut work_io = work_io;

    let result = tokio::io::copy_bidirectional(&mut public, &mut work_io).await;
    metrics.conn_end();
    match result {
        Ok((a, b)) => debug!(proxy = %proxy_name, sent=a, recv=b, "closed"),
        Err(e) => debug!(proxy = %proxy_name, "copy: {e}"),
    }
}

enum DispatchResult {
    Tls(Box<tokio_rustls::server::TlsStream<TcpStream>>),
    Handled,
}

/// 单端口分流：peek 1 字节
/// - 0x16 → TLS ClientHello，握手后返回 TlsStream
/// - 其他 → 明文 HTTP，回 301 跳转到 https
async fn dispatch_tls_or_http(
    proxy_name: &str,
    tcp: TcpStream,
    acceptor: Arc<TlsAcceptor>,
) -> std::io::Result<DispatchResult> {
    // peek 不会消费数据，TLS 握手或 HTTP 解析能再次读到完整请求
    let mut buf = [0u8; 1];
    let n = tcp.peek(&mut buf).await?;
    if n == 0 {
        return Err(std::io::Error::new(
            std::io::ErrorKind::UnexpectedEof,
            "peek empty",
        ));
    }

    if buf[0] == 0x16 {
        // TLS
        match acceptor.accept(tcp).await {
            Ok(tls) => Ok(DispatchResult::Tls(Box::new(tls))),
            Err(e) => {
                debug!(proxy = %proxy_name, "tls handshake failed: {e}");
                Err(std::io::Error::other(e))
            }
        }
    } else {
        // 当作明文 HTTP 处理：读到 \r\n\r\n 或最多 8KB
        redirect_http_to_https(proxy_name, tcp)
            .await
            .map(|()| DispatchResult::Handled)
    }
}

/// 读取 HTTP 请求头，提取 Host，回 301 跳转到 https://{host}{path}
async fn redirect_http_to_https(proxy_name: &str, mut tcp: TcpStream) -> std::io::Result<()> {
    let mut buf = vec![0u8; 8192];
    let mut total = 0usize;
    loop {
        if total >= buf.len() {
            break;
        }
        let n = tcp.read(&mut buf[total..]).await?;
        if n == 0 {
            break;
        }
        total += n;
        // 检测 \r\n\r\n
        if buf[..total].windows(4).any(|w| w == b"\r\n\r\n") {
            break;
        }
    }
    let header = std::str::from_utf8(&buf[..total]).unwrap_or("");

    // 解析 Host
    let host = header
        .lines()
        .find_map(|line| {
            let line = line.trim();
            let lower = line.to_lowercase();
            if lower.starts_with("host:") {
                Some(line[5..].trim().to_string())
            } else {
                None
            }
        })
        .unwrap_or_default();

    // 解析请求行：GET /path HTTP/1.1
    let path = header
        .lines()
        .next()
        .and_then(|req_line| {
            let mut parts = req_line.split_whitespace();
            let _method = parts.next()?;
            let path = parts.next()?;
            Some(path.to_string())
        })
        .unwrap_or_else(|| "/".to_string());

    let location = if host.is_empty() {
        format!("https:/{path}") // 留给客户端处理
    } else {
        format!("https://{host}{path}")
    };

    let body = "<!DOCTYPE html><html><head><title>301 Moved Permanently</title></head>\
         <body><h1>301 Moved Permanently</h1></body></html>\n"
        .to_string();
    let resp = format!(
        "HTTP/1.1 301 Moved Permanently\r\n\
         Location: {location}\r\n\
         Content-Type: text/html; charset=utf-8\r\n\
         Content-Length: {len}\r\n\
         Connection: close\r\n\
         \r\n\
         {body}",
        len = body.len(),
    );

    if let Err(e) = tcp.write_all(resp.as_bytes()).await {
        debug!(proxy = %proxy_name, "http redirect write: {e}");
    }
    let _ = tcp.shutdown().await;
    debug!(proxy = %proxy_name, %location, "http → 301");
    Ok(())
}
