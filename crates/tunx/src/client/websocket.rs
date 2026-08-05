//! WebSocket 传输层
//!
//! 客户端：TCP → TLS → WebSocket upgrade → gRPC over WebSocket
//! WebSocket 作为透明字节管道，gRPC/HTTP/2 在其上运行
//! CDN 友好：CDN 看到 WebSocket 流量，可正常代理

use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};
use std::time::Duration;

use anyhow::{Context as _, Result};
use futures::{Sink, Stream};
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio_tungstenite::tungstenite::{client::IntoClientRequest, Message};
use tokio_tungstenite::WebSocketStream;
use tracing::info;

use tonic::transport::{Endpoint, Uri};
use tower::service_fn;
use tunx_common::config::ClientConfig;

// ─── WsBiStream: WebSocket → AsyncRead + AsyncWrite ──────────────────────────
//
// 将 WebSocketStream 包装为 AsyncRead + AsyncWrite，
// 使 tonic 可以在其上运行 gRPC/HTTP/2。
//
// 读取：从 Binary 消息中提取字节，跳过 Ping/Pong/Text，Close 视为 EOF
// 写入：将字节打包为 Binary 消息发送

pub struct WsBiStream<S> {
    ws: WebSocketStream<S>,
    read_buf: Vec<u8>,
}

impl<S> WsBiStream<S> {
    pub fn new(ws: WebSocketStream<S>) -> Self {
        Self {
            ws,
            read_buf: Vec::new(),
        }
    }
}

impl<S: AsyncRead + AsyncWrite + Unpin + Send> AsyncRead for WsBiStream<S> {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        let this = self.get_mut();

        // 优先返回缓冲区中的数据
        if !this.read_buf.is_empty() {
            let n = std::cmp::min(this.read_buf.len(), buf.remaining());
            buf.put_slice(&this.read_buf[..n]);
            this.read_buf.drain(..n);
            return Poll::Ready(Ok(()));
        }

        loop {
            match Pin::new(&mut this.ws).poll_next(cx) {
                Poll::Pending => return Poll::Pending,
                Poll::Ready(None) => return Poll::Ready(Ok(())), // EOF
                Poll::Ready(Some(Ok(msg))) => match msg {
                    Message::Binary(data) => {
                        if data.is_empty() {
                            continue;
                        }
                        let n = std::cmp::min(data.len(), buf.remaining());
                        buf.put_slice(&data[..n]);
                        if n < data.len() {
                            this.read_buf.extend_from_slice(&data[n..]);
                        }
                        return Poll::Ready(Ok(()));
                    }
                    Message::Close(_) => return Poll::Ready(Ok(())), // EOF
                    // 跳过 Ping/Pong/Text
                    _ => continue,
                },
                Poll::Ready(Some(Err(e))) => {
                    return Poll::Ready(Err(std::io::Error::other(e.to_string())));
                }
            }
        }
    }
}

impl<S: AsyncRead + AsyncWrite + Unpin + Send> AsyncWrite for WsBiStream<S> {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        data: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        let this = self.get_mut();
        match Pin::new(&mut this.ws).poll_ready(cx) {
            Poll::Ready(Ok(())) => {
                match Pin::new(&mut this.ws).start_send(Message::Binary(data.to_vec())) {
                    Ok(()) => Poll::Ready(Ok(data.len())),
                    Err(e) => Poll::Ready(Err(std::io::Error::other(e.to_string()))),
                }
            }
            Poll::Ready(Err(e)) => Poll::Ready(Err(std::io::Error::other(e.to_string()))),
            Poll::Pending => Poll::Pending,
        }
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        let this = self.get_mut();
        Pin::new(&mut this.ws)
            .poll_flush(cx)
            .map_err(|e| std::io::Error::other(e.to_string()))
    }

    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        let this = self.get_mut();
        let _ = Pin::new(&mut this.ws).start_send(Message::Close(None));
        Pin::new(&mut this.ws)
            .poll_close(cx)
            .map_err(|e| std::io::Error::other(e.to_string()))
    }
}

// 服务端 serve_with_incoming 需要 Connected trait
impl<S: AsyncRead + AsyncWrite + Unpin + Send> tonic::transport::server::Connected
    for WsBiStream<S>
{
    type ConnectInfo = std::net::SocketAddr;
    fn connect_info(&self) -> Self::ConnectInfo {
        "0.0.0.0:0".parse().unwrap()
    }
}

// ─── TLS 跳过证书校验（与 control.rs 中的实现独立，避免可见性问题） ──────────

#[derive(Debug)]
struct NoVerifyVerifier;

impl rustls::client::ServerCertVerifier for NoVerifyVerifier {
    fn verify_server_cert(
        &self,
        _end_entity: &rustls::Certificate,
        _intermediates: &[rustls::Certificate],
        _server_name: &rustls::ServerName,
        _scts: &mut dyn Iterator<Item = &[u8]>,
        _ocsp_response: &[u8],
        _now: std::time::SystemTime,
    ) -> std::result::Result<rustls::client::ServerCertVerified, rustls::Error> {
        Ok(rustls::client::ServerCertVerified::assertion())
    }
}

// ─── 建立 WebSocket 连接并构建 gRPC channel ──────────────────────────────────

pub async fn connect(cfg: &ClientConfig) -> Result<tonic::transport::Channel> {
    let hostname = cfg.tls_sni.clone().unwrap_or_else(|| {
        cfg.server_addr
            .split(':')
            .next()
            .unwrap_or("localhost")
            .to_string()
    });

    // 构建 rustls ClientConfig（与 TCP 路径一致）
    let mut tls_config = if cfg.tls_skip_verify {
        rustls::ClientConfig::builder()
            .with_safe_defaults()
            .with_custom_certificate_verifier(Arc::new(NoVerifyVerifier))
            .with_no_client_auth()
    } else {
        let mut root_store = rustls::RootCertStore::empty();
        for cert in rustls_native_certs::load_native_certs().with_context(|| "load native certs")? {
            root_store.add(&rustls::Certificate(cert.0)).ok();
        }
        rustls::ClientConfig::builder()
            .with_safe_defaults()
            .with_root_certificates(root_store)
            .with_no_client_auth()
    };
    // HTTP/2 ALPN（gRPC 需要）
    tls_config.alpn_protocols = vec![b"h2".to_vec()];
    let tls_config = Arc::new(tls_config);
    let connector = tokio_rustls::TlsConnector::from(tls_config);

    let server_addr = cfg.server_addr.clone();
    let hostname_clone = hostname.clone();
    let ws_path = cfg.ws_path.clone();

    let channel = Endpoint::from_static("http://tunx.local")
        .connect_timeout(Duration::from_secs(10))
        .connect_with_connector(service_fn(move |_: Uri| {
            let server_addr = server_addr.clone();
            let connector = connector.clone();
            let hostname = hostname_clone.clone();
            let ws_path = ws_path.clone();
            async move {
                // 1. TCP 连接
                let tcp = tokio::net::TcpStream::connect(&server_addr).await?;
                tcp.set_nodelay(true).ok();

                // 2. TLS 握手
                let server_name = rustls::ServerName::try_from(hostname.as_str())
                    .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidInput, e))?;
                let tls = connector.connect(server_name, tcp).await?;

                // 3. WebSocket upgrade
                let ws_url = format!("ws://tunx.local{}", ws_path);
                let mut request = ws_url
                    .into_client_request()
                    .map_err(std::io::Error::other)?;
                // 覆盖 Host 头为实际域名
                if let Ok(host) = hostname.parse() {
                    request.headers_mut().insert("Host", host);
                }

                let (ws_stream, _response) = tokio_tungstenite::client_async(request, tls)
                    .await
                    .map_err(std::io::Error::other)?;

                Ok::<_, std::io::Error>(WsBiStream::new(ws_stream))
            }
        }))
        .await
        .context("build gRPC channel over WebSocket")?;

    info!("WebSocket connection established (sni={hostname})");
    Ok(channel)
}
