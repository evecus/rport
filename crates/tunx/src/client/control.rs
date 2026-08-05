//! 客户端控制层
//!
//! 流程：connect(quic/tcp) → Login → RegisterProxies → ControlStream loop
//! 断线后指数退避重连
//!
//! WorkConn 派发：
//! - quic 路径：调 `proxy::tcp/udp::handle_work_conn`（QUIC open_bi + header）
//! - tcp 路径：调 `proxy::tcp/udp::handle_work_conn_tcp`（OpenWorkConn RPC + TonicStreamIo）

use std::collections::HashMap;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};
use std::time::Duration;

use anyhow::{Context as _, Result};
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tonic::metadata::MetadataValue;
use tonic::transport::{Endpoint, Uri};
use tonic::Request;
use tower::service_fn;
use tracing::{debug, info, warn};
use tunx_common::config::{ClientConfig, ClientTransport, ProxyKind};

use tunx_proto::{
    client_message, control_service_client::ControlServiceClient, server_message, ClientMessage,
    LoginRequest, Pong, ProxyConfig, ProxyType, RegisterProxiesRequest, TcpConfig, UdpConfig,
};

use crate::client::proxy::{tcp, udp};

// ─── 顶层循环（断线重连） ─────────────────────────────────────────────────────

pub async fn run(cfg: ClientConfig) -> Result<()> {
    let client_id = uuid::Uuid::new_v4().to_string();
    let mut backoff = Duration::from_secs(1);
    let max_backoff = Duration::from_secs(cfg.reconnect_max_secs);

    loop {
        match run_session(&cfg, &client_id).await {
            Ok(()) => info!("session ended, reconnecting..."),
            Err(e) => warn!("session error: {e:#}"),
        }
        tokio::time::sleep(backoff).await;
        backoff = (backoff * 2).min(max_backoff);
    }
}

// ─── 单次会话 ─────────────────────────────────────────────────────────────────

async fn run_session(cfg: &ClientConfig, client_id: &str) -> Result<()> {
    match cfg.transport {
        ClientTransport::Quic => run_session_quic(cfg, client_id).await,
        ClientTransport::Tcp => run_session_tcp(cfg, client_id).await,
        ClientTransport::Websocket => run_session_websocket(cfg, client_id).await,
        ClientTransport::Xhttp => run_session_tcp(cfg, client_id).await, // XHTTP 复用 TCP 路径
    }
}

// ─── QUIC 路径 ───────────────────────────────────────────────────────────────

async fn run_session_quic(cfg: &ClientConfig, client_id: &str) -> Result<()> {
    // 1. 建立 QUIC 连接
    let conn = crate::client::quic::connect(cfg).await?;

    // 2. 在 QUIC connection 上构建 gRPC channel
    let conn_for_channel = conn.clone();
    let channel = Endpoint::from_static("http://tunx.local")
        .connect_with_connector(service_fn(move |_: Uri| {
            let c = conn_for_channel.clone();
            async move {
                let (send, recv) = c
                    .open_bi()
                    .await
                    .map_err(|e| std::io::Error::new(std::io::ErrorKind::ConnectionReset, e))?;
                Ok::<_, std::io::Error>(QuicBiStream::new(send, recv))
            }
        }))
        .await
        .context("build gRPC channel")?;

    let mut grpc = ControlServiceClient::new(channel);

    let session_id = do_login(&mut grpc, cfg, client_id).await?;
    let proxy_locals = do_register(&mut grpc, cfg, &session_id).await?;

    // 5. ControlStream（携带 session-id metadata）
    let hb_interval = cfg.heartbeat_interval_secs;
    let mut ctrl_req = Request::new(heartbeat_stream(hb_interval));
    ctrl_req
        .metadata_mut()
        .insert("session-id", MetadataValue::try_from(&session_id).unwrap());

    let mut stream = grpc
        .control_stream(ctrl_req)
        .await
        .context("control_stream")?
        .into_inner();

    info!("control stream ready (QUIC)");

    // 6. 消息循环
    loop {
        match stream.message().await {
            Ok(Some(msg)) => match msg.payload {
                Some(server_message::Payload::Ping(p)) => {
                    debug!(ts = p.timestamp, "ping");
                }
                Some(server_message::Payload::WorkConnReq(req)) => {
                    let proxy_name = req.proxy_name.clone();
                    let stream_id = req.stream_id.clone();
                    let (local, is_udp) = match proxy_locals.get(&proxy_name) {
                        Some(v) => v.clone(),
                        None => {
                            warn!(proxy = %proxy_name, "unknown proxy in WorkConnReq");
                            continue;
                        }
                    };
                    let c = conn.clone();
                    tokio::spawn(async move {
                        let result = if is_udp {
                            udp::handle_work_conn(c, proxy_name.clone(), local, stream_id).await
                        } else {
                            tcp::handle_work_conn(c, proxy_name.clone(), local, stream_id).await
                        };
                        if let Err(e) = result {
                            warn!(proxy = %proxy_name, "work conn: {e}");
                        }
                    });
                }
                Some(server_message::Payload::ProxyShutdown(s)) => {
                    warn!(proxy = %s.proxy_name, reason = %s.reason, "server shutdown proxy");
                }
                None => {}
            },
            Ok(None) => {
                info!("control stream closed by server");
                break;
            }
            Err(e) => {
                warn!("control stream error: {e}");
                break;
            }
        }
    }

    Ok(())
}

// ─── TCP 路径 ───────────────────────────────────────────────────────────────

async fn run_session_tcp(cfg: &ClientConfig, client_id: &str) -> Result<()> {
    // 1. 取 hostname（用于 TLS SNI）
    let hostname = cfg.tls_sni.clone().unwrap_or_else(|| {
        cfg.server_addr
            .split(':')
            .next()
            .unwrap_or("localhost")
            .to_string()
    });

    // 2. 构建 rustls ClientConfig
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
    // HTTP/2 ALPN
    tls_config.alpn_protocols = vec![b"h2".to_vec()];
    let tls_config = Arc::new(tls_config);
    let connector = tokio_rustls::TlsConnector::from(tls_config);

    // 3. 用 connect_with_connector 建立手动 TLS 通道（与 QUIC 路径模式一致）
    let server_addr = cfg.server_addr.clone();
    let hostname_clone = hostname.clone();
    let channel = Endpoint::from_static("http://tunx.local")
        .connect_timeout(Duration::from_secs(10))
        .connect_with_connector(service_fn(move |_: Uri| {
            let server_addr = server_addr.clone();
            let connector = connector.clone();
            let hostname = hostname_clone.clone();
            async move {
                let tcp = tokio::net::TcpStream::connect(server_addr).await?;
                tcp.set_nodelay(true).ok();
                let server_name = rustls::ServerName::try_from(hostname.as_str())
                    .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidInput, e))?;
                let tls = connector.connect(server_name, tcp).await?;
                Ok::<_, std::io::Error>(TlsIo(tls))
            }
        }))
        .await
        .context("build gRPC channel over TCP+TLS")?;

    run_session_tcp_inner(channel, cfg, client_id, hostname).await
}

async fn run_session_tcp_inner(
    channel: tonic::transport::Channel,
    cfg: &ClientConfig,
    client_id: &str,
    _hostname: String,
) -> Result<()> {
    let mut grpc = ControlServiceClient::new(channel.clone());

    let session_id = do_login(&mut grpc, cfg, client_id).await?;
    let proxy_locals = do_register(&mut grpc, cfg, &session_id).await?;

    // 5. ControlStream（携带 session-id metadata）
    let hb_interval = cfg.heartbeat_interval_secs;
    let mut ctrl_req = Request::new(heartbeat_stream(hb_interval));
    ctrl_req
        .metadata_mut()
        .insert("session-id", MetadataValue::try_from(&session_id).unwrap());

    let mut stream = grpc
        .control_stream(ctrl_req)
        .await
        .context("control_stream")?
        .into_inner();

    info!("control stream ready (TCP)");

    // 6. 消息循环：收到 WorkConnReq 时调 OpenWorkConn RPC（独立 tonic Channel 复用 HTTP/2）
    let channel2 = channel.clone();
    loop {
        match stream.message().await {
            Ok(Some(msg)) => match msg.payload {
                Some(server_message::Payload::Ping(p)) => {
                    debug!(ts = p.timestamp, "ping");
                }
                Some(server_message::Payload::WorkConnReq(req)) => {
                    let proxy_name = req.proxy_name.clone();
                    let stream_id = req.stream_id.clone();
                    let (local, is_udp) = match proxy_locals.get(&proxy_name) {
                        Some(v) => v.clone(),
                        None => {
                            warn!(proxy = %proxy_name, "unknown proxy in WorkConnReq");
                            continue;
                        }
                    };
                    let grpc2 = ControlServiceClient::new(channel2.clone());
                    let sid_meta = session_id.clone();
                    tokio::spawn(async move {
                        let result = if is_udp {
                            udp::handle_work_conn_tcp(
                                grpc2,
                                sid_meta,
                                proxy_name.clone(),
                                local,
                                stream_id,
                            )
                            .await
                        } else {
                            tcp::handle_work_conn_tcp(
                                grpc2,
                                sid_meta,
                                proxy_name.clone(),
                                local,
                                stream_id,
                            )
                            .await
                        };
                        if let Err(e) = result {
                            warn!(proxy = %proxy_name, "work conn: {e}");
                        }
                    });
                }
                Some(server_message::Payload::ProxyShutdown(s)) => {
                    warn!(proxy = %s.proxy_name, reason = %s.reason, "server shutdown proxy");
                }
                None => {}
            },
            Ok(None) => {
                info!("control stream closed by server");
                break;
            }
            Err(e) => {
                warn!("control stream error: {e}");
                break;
            }
        }
    }
    Ok(())
}

// ─── WebSocket 路径 ─────────────────────────────────────────────────────────

async fn run_session_websocket(cfg: &ClientConfig, client_id: &str) -> Result<()> {
    let hostname = cfg.tls_sni.clone().unwrap_or_else(|| {
        cfg.server_addr
            .split(':')
            .next()
            .unwrap_or("localhost")
            .to_string()
    });

    // 建立 WebSocket 连接并构建 gRPC channel
    let channel = crate::client::websocket::connect(cfg).await?;

    // 复用 TCP 路径的 gRPC 会话逻辑（Login → Register → ControlStream → WorkConn）
    run_session_tcp_inner(channel, cfg, client_id, hostname).await
}

// ─── 通用：登录 ──────────────────────────────────────────────────────────────

async fn do_login(
    grpc: &mut ControlServiceClient<tonic::transport::Channel>,
    cfg: &ClientConfig,
    client_id: &str,
) -> Result<String> {
    let login = grpc
        .login(LoginRequest {
            token: cfg.token.clone(),
            client_id: client_id.to_string(),
            version: env!("CARGO_PKG_VERSION").to_string(),
        })
        .await
        .context("login")?
        .into_inner();

    if let Some(e) = &login.error {
        if e.code != 0 {
            anyhow::bail!("login rejected: {} ({})", e.message, e.code);
        }
    }
    let session_id = login.session_id.clone();
    info!(session_id, server_ver = %login.server_version, "logged in");
    Ok(session_id)
}

// ─── 通用：注册代理 ──────────────────────────────────────────────────────────

async fn do_register(
    grpc: &mut ControlServiceClient<tonic::transport::Channel>,
    cfg: &ClientConfig,
    session_id: &str,
) -> Result<HashMap<String, (String, bool)>> {
    let proxy_cfgs: Vec<ProxyConfig> = cfg
        .proxies
        .iter()
        .map(|p| match &p.kind {
            ProxyKind::Tcp(t) => ProxyConfig {
                name: p.name.clone(),
                r#type: ProxyType::Tcp as i32,
                tcp: Some(TcpConfig {
                    remote_port: t.remote_port as u32,
                    local_addr: t.local_addr.clone(),
                    tls: t.tls,
                    custom_domain: t.custom_domain.clone().unwrap_or_default(),
                }),
                udp: None,
            },
            ProxyKind::Udp(u) => ProxyConfig {
                name: p.name.clone(),
                r#type: ProxyType::Udp as i32,
                tcp: None,
                udp: Some(UdpConfig {
                    remote_port: u.remote_port as u32,
                    local_addr: u.local_addr.clone(),
                }),
            },
        })
        .collect();

    let reg = grpc
        .register_proxies(RegisterProxiesRequest {
            session_id: session_id.to_string(),
            proxies: proxy_cfgs,
        })
        .await
        .context("register_proxies")?
        .into_inner();

    let mut proxy_locals: HashMap<String, (String, bool)> = HashMap::new();
    for r in &reg.results {
        if r.success {
            info!(proxy = %r.name, remote_port = r.remote_port, "registered");
            if let Some(p) = cfg.proxies.iter().find(|p| p.name == r.name) {
                match &p.kind {
                    ProxyKind::Tcp(t) => {
                        proxy_locals.insert(r.name.clone(), (t.local_addr.clone(), false));
                    }
                    ProxyKind::Udp(u) => {
                        proxy_locals.insert(r.name.clone(), (u.local_addr.clone(), true));
                    }
                }
            }
        } else {
            let msg = r.error.as_ref().map(|e| e.message.as_str()).unwrap_or("?");
            warn!(proxy = %r.name, "register failed: {msg}");
        }
    }
    Ok(proxy_locals)
}

// ─── 心跳流：定期发 Pong ─────────────────────────────────────────────────────

fn heartbeat_stream(
    interval_secs: u64,
) -> impl futures::Stream<Item = ClientMessage> + Send + 'static {
    async_stream::stream! {
        let mut iv = tokio::time::interval(Duration::from_secs(interval_secs));
        loop {
            iv.tick().await;
            yield ClientMessage {
                payload: Some(client_message::Payload::Pong(Pong {
                    timestamp: unix_now(),
                })),
            };
        }
    }
}

fn unix_now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}

// ─── QuicBiStream：QUIC (SendStream, RecvStream) → hyper IO ──────────────────
//
// 使用 http:// scheme 的 Endpoint::connect_with_connector 不会叠加 TLS 层，
// hyper 客户端会自行发送标准 HTTP/2 连接前言 + SETTINGS 帧，
// 因此 QuicBiStream 只需透传读写，无需手动注入前言。

pub struct QuicBiStream {
    send: quinn::SendStream,
    recv: quinn::RecvStream,
}

impl QuicBiStream {
    pub fn new(send: quinn::SendStream, recv: quinn::RecvStream) -> Self {
        Self { send, recv }
    }
}

impl AsyncRead for QuicBiStream {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.recv).poll_read(cx, buf)
    }
}

impl AsyncWrite for QuicBiStream {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        data: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        Pin::new(&mut self.send).poll_write(cx, data)
    }
    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.send).poll_flush(cx)
    }
    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.send).poll_shutdown(cx)
    }
}

impl hyper::client::connect::Connection for QuicBiStream {
    fn connected(&self) -> hyper::client::connect::Connected {
        hyper::client::connect::Connected::new()
    }
}

// ─── TCP+TLS：tokio-rustls TlsStream → tonic IO ──────────────────────────────

pub struct TlsIo(pub tokio_rustls::client::TlsStream<tokio::net::TcpStream>);

impl AsyncRead for TlsIo {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.0).poll_read(cx, buf)
    }
}

impl AsyncWrite for TlsIo {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        data: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        Pin::new(&mut self.0).poll_write(cx, data)
    }
    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.0).poll_flush(cx)
    }
    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.0).poll_shutdown(cx)
    }
}

impl tonic::transport::server::Connected for TlsIo {
    type ConnectInfo = std::net::SocketAddr;
    fn connect_info(&self) -> Self::ConnectInfo {
        self.0
            .get_ref()
            .0
            .peer_addr()
            .unwrap_or_else(|_| "0.0.0.0:0".parse().unwrap())
    }
}

// ─── TLS 跳过证书校验的 verifier（仅 tls_skip_verify=true 时用） ──────────────

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
