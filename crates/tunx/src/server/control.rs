//! 服务端控制层
//!
//! QUIC 路径：每个 QUIC connection 上
//!   stream[0] (bi) = gRPC 控制流（tonic serve_with_incoming）
//!   stream[1..n] (bi) = 数据 WorkConn，由 stream_id header 路由
//!
//! TCP  路径：单条 TCP+TLS+HTTP/2 连接承载 gRPC + OpenWorkConn RPC（HTTP/2 stream 多路复用）

use std::collections::HashMap;
use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};

use anyhow::Result;
use quinn::{Endpoint, RecvStream, SendStream};
use tunx_common::quic::{STREAM_ID_LEN, WORK_CONN_MAGIC};
use tunx_common::stream::{TonicStreamIo, WorkIo};
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio::net::TcpListener;
use tokio::sync::{mpsc, Mutex};
use tonic::{transport::Server as TonicServer, Request, Response, Status, Streaming};
use tracing::{debug, info, warn};

use tunx_common::config::ServerConfig;
use tunx_proto::{
    client_message,
    control_service_server::{ControlService, ControlServiceServer},
    server_message, ClientMessage, LoginRequest, LoginResponse, Ping, ProxyResult,
    RegisterProxiesRequest, RegisterProxiesResponse, ServerMessage, WorkConnFrame,
    work_conn_frame::Payload as WcfPayload,
};

use crate::server::auth;
use crate::server::ports::PortManager;
use crate::server::proxy::{tcp, udp, ProxyHandle};
use crate::server::session::Session;
use crate::server::tls::ServerTls;

// ─── 全局状态 ─────────────────────────────────────────────────────────────────

#[derive(Clone)]
pub struct AppState {
    pub config: Arc<ServerConfig>,
    pub port_mgr: Arc<PortManager>,
    /// session_id → Session（HMAC 验签后才能查到）
    pub sessions: Arc<Mutex<HashMap<String, Arc<Session>>>>,
    /// TLS 资源：用于 tcp proxy 启用 TLS 终止
    pub server_tls: Arc<ServerTls>,
    /// 预构建的 TlsAcceptor（仅 public_trusted=true 时存在）
    pub tls_acceptor: Option<Arc<tokio_rustls::TlsAcceptor>>,
    /// 预构建的 TCP+TLS acceptor（用于 TCP transport 的 gRPC listener）
    pub tcp_tls_acceptor: Option<Arc<tokio_rustls::TlsAcceptor>>,
}

// ─── 入口：根据 transport 模式启动 listener ──────────────────────────────────

pub async fn run(
    cfg: ServerConfig,
    server_tls: ServerTls,
    port_mgr: PortManager,
) -> Result<()> {
    let addr: SocketAddr = cfg.bind_addr.parse()?;
    let transport = cfg.transport;

    // 构建 TCP+TLS acceptor（acme/manual 模式才有）
    let tcp_tls_acceptor: Option<Arc<tokio_rustls::TlsAcceptor>> = if server_tls.public_trusted {
        Some(crate::server::tls::build_tls_acceptor(&server_tls)?)
    } else {
        None
    };

    // proxy TLS 终止 acceptor 与 tcp_tls_acceptor 共用（同一张证书）
    let tls_acceptor = tcp_tls_acceptor.clone();

    if tls_acceptor.is_some() {
        info!(
            "TLS termination for TCP proxies enabled, cert domains: {:?}",
            server_tls.cert_domains
        );
    } else {
        info!("TLS termination disabled (self-signed cert in use)");
    }

    let server_tls = Arc::new(server_tls);
    let state = AppState {
        config: Arc::new(cfg),
        port_mgr: Arc::new(port_mgr),
        sessions: Arc::new(Mutex::new(HashMap::new())),
        server_tls,
        tls_acceptor,
        tcp_tls_acceptor,
    };

    // 后台 task：定期清理超时未建立 ControlStream 的 session
    {
        let sessions = state.sessions.clone();
        tokio::spawn(async move {
            let mut iv = tokio::time::interval(std::time::Duration::from_secs(15));
            loop {
                iv.tick().await;
                let mut map = sessions.lock().await;
                let mut to_remove = Vec::new();
                for (id, s) in map.iter() {
                    let has_ctrl = s.server_tx.try_read().map(|g| g.is_some()).unwrap_or(true);
                    if !has_ctrl && s.timer.is_expired() {
                        warn!(session_id = %id, "session TTL expired, removing");
                        to_remove.push((id.clone(), s.clone()));
                    }
                }
                for (id, s) in &to_remove {
                    s.shutdown().await;
                    map.remove(id);
                }
                let removed = to_remove.len();
                if removed > 0 {
                    info!("TTL cleanup: removed {removed} expired sessions");
                }
            }
        });
    }

    // 根据 transport 模式启动对应的 listener
    // QUIC 绑 UDP，TCP/WebSocket/XHTTP 绑 TCP，可在同一端口共存
    let mut tasks = Vec::new();

    if transport.has_quic() {
        let s = state.clone();
        tasks.push(tokio::spawn(async move { run_quic(addr, s).await }));
    }
    if transport.has_tcp() {
        let s = state.clone();
        tasks.push(tokio::spawn(async move { run_tcp(addr, s).await }));
    }
    if transport.has_websocket() {
        let s = state.clone();
        tasks.push(tokio::spawn(async move {
            crate::server::websocket::run_websocket(addr, s).await
        }));
    }
    if transport.has_xhttp() {
        let s = state.clone();
        tasks.push(tokio::spawn(async move {
            crate::server::xhttp::run_xhttp(addr, s).await
        }));
    }

    // 等待所有 listener task 完成，收集首个错误
    let mut first_err: Option<anyhow::Error> = None;
    for task in tasks {
        match task.await {
            Ok(Ok(())) => {}
            Ok(Err(e)) => {
                if first_err.is_none() {
                    first_err = Some(e);
                }
            }
            Err(e) => {
                if first_err.is_none() {
                    first_err = Some(anyhow::anyhow!("transport task panicked: {e}"));
                }
            }
        }
    }

    match first_err {
        Some(e) => Err(e),
        None => Ok(()),
    }
}

// ─── QUIC 路径 ───────────────────────────────────────────────────────────────

async fn run_quic(addr: SocketAddr, state: AppState) -> Result<()> {
    let endpoint = Endpoint::server(state.server_tls.quinn_cfg.clone(), addr)?;
    info!("QUIC listening on {addr}");

    while let Some(incoming) = endpoint.accept().await {
        let state = state.clone();
        tokio::spawn(async move {
            match incoming.await {
                Ok(conn) => {
                    let peer = conn.remote_address();
                    info!(%peer, "QUIC connected");
                    if let Err(e) = handle_quic_connection(conn, state).await {
                        debug!(%peer, "QUIC conn ended: {e}");
                    }
                }
                Err(e) => warn!("QUIC incoming failed: {e}"),
            }
        });
    }
    Ok(())
}

async fn handle_quic_connection(conn: quinn::Connection, state: AppState) -> Result<()> {
    let (ctrl_send, ctrl_recv) = conn.accept_bi().await?;

    let svc = ControlServiceImpl {
        state: state.clone(),
    };

    let grpc_task = tokio::spawn(async move {
        let io = QuicBiStream::new(ctrl_send, ctrl_recv);
        if let Err(e) = TonicServer::builder()
            .add_service(ControlServiceServer::new(svc))
            .serve_with_incoming(futures::stream::once(
                async move { Ok::<_, std::io::Error>(io) },
            ))
            .await
        {
            debug!("gRPC ended: {e}");
        }
    });

    let sessions = state.sessions.clone();
    let data_task = tokio::spawn(async move {
        loop {
            match conn.accept_bi().await {
                Ok((send, recv)) => {
                    let sessions = sessions.clone();
                    tokio::spawn(handle_quic_work_stream(send, recv, sessions));
                }
                Err(e) => {
                    debug!("accept_bi ended: {e}");
                    break;
                }
            }
        }
    });

    tokio::select! {
        _ = grpc_task => {}
        _ = data_task => {}
    }
    Ok(())
}

// ─── QUIC WorkConn stream（bi-stream 头部带 stream_id 路由） ──────────────────

async fn handle_quic_work_stream(
    send: SendStream,
    mut recv: RecvStream,
    sessions: Arc<Mutex<HashMap<String, Arc<Session>>>>,
) {
    let mut header = [0u8; 4 + STREAM_ID_LEN];
    if let Err(e) = recv.read_exact(&mut header).await {
        warn!("work stream header read failed: {e}");
        return;
    }
    if &header[..4] != WORK_CONN_MAGIC {
        warn!("work stream invalid magic");
        return;
    }
    let stream_id = match std::str::from_utf8(&header[4..]) {
        Ok(s) => s.to_string(),
        Err(_) => {
            warn!("work stream invalid stream_id");
            return;
        }
    };
    debug!(stream_id, "QUIC work stream arrived");

    // 找到持有该 pending_work 的 session（stream_id 全局唯一）
    let sessions_guard = sessions.lock().await;
    let target = sessions_guard
        .values()
        .find(|s| {
            s.pending_work
                .try_lock()
                .map(|g| g.contains_key(&stream_id))
                .unwrap_or(false)
        })
        .cloned();
    drop(sessions_guard);

    if let Some(session) = target {
        // 把 (SendStream, RecvStream) 包成 Box<dyn WorkIo>
        let joined: Box<dyn WorkIo> = Box::new(tokio::io::join(recv, send));
        if !session.deliver_work_conn(&stream_id, joined).await {
            warn!(stream_id, "deliver_work_conn failed");
        }
    } else {
        warn!(stream_id, "no pending work conn matched");
    }
}

// ─── TCP 路径：单个 TcpListener + TLS acceptor ──────────────────────────────

pub async fn run_tcp(addr: SocketAddr, state: AppState) -> Result<()> {
    let acceptor = state
        .tcp_tls_acceptor
        .clone()
        .ok_or_else(|| anyhow::anyhow!("transport=tcp requires acme/manual TLS mode"))?;

    let listener = TcpListener::bind(addr).await?;
    info!("TCP+TLS listening on {addr}");

    loop {
        let (tcp, peer) = match listener.accept().await {
            Ok(v) => v,
            Err(e) => {
                warn!("tcp accept: {e}");
                continue;
            }
        };
        let acceptor = acceptor.clone();
        let state = state.clone();
        tokio::spawn(async move {
            info!(%peer, "TCP connected");
            let tls_stream = match acceptor.accept(tcp).await {
                Ok(s) => s,
                Err(e) => {
                    warn!(%peer, "TLS handshake failed: {e}");
                    return;
                }
            };
            let svc = ControlServiceImpl { state: state.clone() };
            if let Err(e) = TonicServer::builder()
                .add_service(ControlServiceServer::new(svc))
                .serve_with_incoming(futures::stream::once(
                    async move { Ok::<_, std::io::Error>(TlsAsAccepted(tls_stream)) },
                ))
                .await
            {
                debug!(%peer, "gRPC over TCP ended: {e}");
            }
        });
    }
}

/// 把 TlsStream<TcpStream> 包成 tonic 可接受的 IO（实现 Connected）
pub struct TlsAsAccepted(pub tokio_rustls::server::TlsStream<tokio::net::TcpStream>);

impl AsyncRead for TlsAsAccepted {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.0).poll_read(cx, buf)
    }
}

impl AsyncWrite for TlsAsAccepted {
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

impl tonic::transport::server::Connected for TlsAsAccepted {
    type ConnectInfo = SocketAddr;
    fn connect_info(&self) -> Self::ConnectInfo {
        self.0.get_ref().0.peer_addr().unwrap_or_else(|_| "0.0.0.0:0".parse().unwrap())
    }
}

// ─── gRPC ControlService 实现 ─────────────────────────────────────────────────

pub struct ControlServiceImpl {
    pub(crate) state: AppState,
}

#[tonic::async_trait]
impl ControlService for ControlServiceImpl {
    // ── Login ──────────────────────────────────────────────────────────────────
    async fn login(&self, req: Request<LoginRequest>) -> Result<Response<LoginResponse>, Status> {
        let r = req.into_inner();
        let cfg = &self.state.config;

        // 1. Token 校验
        if !cfg.token.is_empty() && r.token != cfg.token {
            warn!(client_id = %r.client_id, "login rejected: bad token");
            return Ok(Response::new(LoginResponse {
                error: Some(tunx_proto::Error {
                    code: 401,
                    message: "invalid token".into(),
                }),
                ..Default::default()
            }));
        }

        // 2. 生成带 HMAC 签名的 session_id
        let session_id = auth::generate_session_id(&cfg.token);

        // 3. 创建 session（30s 内必须建立 ControlStream）
        let session = Session::new(session_id.clone(), r.client_id.clone(), 30, self.state.port_mgr.clone());
        self.state
            .sessions
            .lock()
            .await
            .insert(session_id.clone(), session);

        info!(client_id = %r.client_id, ver = %r.version, session_id, "login ok");
        Ok(Response::new(LoginResponse {
            session_id,
            server_version: env!("CARGO_PKG_VERSION").to_string(),
            error: None,
        }))
    }

    // ── RegisterProxies ────────────────────────────────────────────────────────
    async fn register_proxies(
        &self,
        req: Request<RegisterProxiesRequest>,
    ) -> Result<Response<RegisterProxiesResponse>, Status> {
        let r = req.into_inner();

        // 验签 session_id
        auth::verify_session_id(&r.session_id, &self.state.config.token)
            .map_err(|e| Status::unauthenticated(e.to_string()))?;

        let sessions = self.state.sessions.lock().await;
        let session = sessions
            .get(&r.session_id)
            .ok_or_else(|| Status::unauthenticated("session not found"))?
            .clone();
        drop(sessions);

        // 清理僵尸 session：control stream 已断开但尚未被清除的旧 session
        // 场景：客户端重启后立刻重连，旧 session 的心跳超时还没触发
        self.evict_stale_sessions(&r.session_id).await;

        let mut results = Vec::new();

        for pc in r.proxies {
            use tunx_proto::ProxyType;

            let result = if pc.r#type == ProxyType::Tcp as i32 {
                match pc.tcp {
                    None => ProxyResult {
                        name: pc.name,
                        success: false,
                        error: Some(tunx_proto::Error {
                            code: 400,
                            message: "missing tcp config".into(),
                        }),
                        ..Default::default()
                    },
                    Some(tc) => {
                        // ── TLS 校验 ──
                        let tls_acceptor_for_proxy: Option<Arc<tokio_rustls::TlsAcceptor>> =
                            if tc.tls {
                                // 1. 必须 public_trusted
                                if !self.state.server_tls.public_trusted {
                                    results.push(ProxyResult {
                                        name: pc.name.clone(),
                                        success: false,
                                        error: Some(tunx_proto::Error {
                                            code: 403,
                                            message: "tls=true requires a CA-signed certificate \
                                                      (acme/manual); self_signed is not allowed"
                                                .into(),
                                        }),
                                        ..Default::default()
                                    });
                                    continue;
                                }
                                // 2. custom_domain 必填且匹配 SAN
                                let domain = tc.custom_domain.trim().to_lowercase();
                                if domain.is_empty() {
                                    results.push(ProxyResult {
                                        name: pc.name.clone(),
                                        success: false,
                                        error: Some(tunx_proto::Error {
                                            code: 400,
                                            message: "tls=true requires custom_domain".into(),
                                        }),
                                        ..Default::default()
                                    });
                                    continue;
                                }
                                let matched = self.state.server_tls.cert_domains.iter().any(|d| {
                                    d == &domain || wildcard_matches(d, &domain)
                                });
                                if !matched {
                                    results.push(ProxyResult {
                                        name: pc.name.clone(),
                                        success: false,
                                        error: Some(tunx_proto::Error {
                                            code: 400,
                                            message: format!(
                                                "custom_domain '{domain}' not covered by \
                                                 server certificate SAN: {:?}",
                                                self.state.server_tls.cert_domains
                                            ),
                                        }),
                                        ..Default::default()
                                    });
                                    continue;
                                }
                                self.state.tls_acceptor.clone()
                            } else {
                                None
                            };

                        // 尝试获取端口，失败时强制抢占旧 session 的端口再重试
                        let port = match self.state.port_mgr.acquire(tc.remote_port as u16) {
                            Ok(p) => p,
                            Err(_) => {
                                // 端口被占用，尝试强制释放旧 session 的端口
                                self.force_release_port(tc.remote_port as u16, &r.session_id).await;
                                match self.state.port_mgr.acquire(tc.remote_port as u16) {
                                    Ok(p) => p,
                                    Err(e) => {
                                        results.push(ProxyResult {
                                            name: pc.name,
                                            success: false,
                                            error: Some(tunx_proto::Error {
                                                code: 500,
                                                message: e.to_string(),
                                            }),
                                            ..Default::default()
                                        });
                                        continue;
                                    }
                                }
                            }
                        };

                        let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel();
                        session
                            .add_proxy(
                                pc.name.clone(),
                                ProxyHandle {
                                    local_addr: tc.local_addr.clone(),
                                    remote_port: port,
                                    shutdown_tx,
                                },
                            )
                            .await;

                        let sess = session.clone();
                        let name = pc.name.clone();
                        let local = tc.local_addr.clone();
                        tokio::spawn(async move {
                            tcp::start_tcp_proxy(
                                name,
                                local,
                                port,
                                sess,
                                tls_acceptor_for_proxy,
                                shutdown_rx,
                            )
                            .await;
                        });

                        ProxyResult {
                            name: pc.name,
                            success: true,
                            remote_port: port as u32,
                            error: None,
                        }
                    }
                }
            } else if pc.r#type == ProxyType::Udp as i32 {
                match pc.udp {
                    None => ProxyResult {
                        name: pc.name,
                        success: false,
                        error: Some(tunx_proto::Error {
                            code: 400,
                            message: "missing udp config".into(),
                        }),
                        ..Default::default()
                    },
                    Some(uc) => {
                        // 尝试获取端口，失败时强制抢占旧 session 的端口再重试
                        let port = match self.state.port_mgr.acquire(uc.remote_port as u16) {
                            Ok(p) => p,
                            Err(_) => {
                                self.force_release_port(uc.remote_port as u16, &r.session_id).await;
                                match self.state.port_mgr.acquire(uc.remote_port as u16) {
                                    Ok(p) => p,
                                    Err(e) => {
                                        results.push(ProxyResult {
                                            name: pc.name,
                                            success: false,
                                            error: Some(tunx_proto::Error {
                                                code: 500,
                                                message: e.to_string(),
                                            }),
                                            ..Default::default()
                                        });
                                        continue;
                                    }
                                }
                            }
                        };

                        let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel();
                        session
                            .add_proxy(
                                pc.name.clone(),
                                ProxyHandle {
                                    local_addr: uc.local_addr.clone(),
                                    remote_port: port,
                                    shutdown_tx,
                                },
                            )
                            .await;

                        let sess = session.clone();
                        let name = pc.name.clone();
                        let local = uc.local_addr.clone();
                        tokio::spawn(async move {
                            udp::start_udp_proxy(name, local, port, sess, shutdown_rx).await;
                        });

                        ProxyResult {
                            name: pc.name,
                            success: true,
                            remote_port: port as u32,
                            error: None,
                        }
                    }
                }
            } else {
                ProxyResult {
                    name: pc.name,
                    success: false,
                    error: Some(tunx_proto::Error {
                        code: 400,
                        message: "unsupported type".into(),
                    }),
                    ..Default::default()
                }
            };
            results.push(result);
        }

        Ok(Response::new(RegisterProxiesResponse { results }))
    }

    // ── ControlStream ──────────────────────────────────────────────────────────
    type ControlStreamStream =
        tokio_stream::wrappers::ReceiverStream<Result<ServerMessage, Status>>;

    async fn control_stream(
        &self,
        req: Request<Streaming<ClientMessage>>,
    ) -> Result<Response<Self::ControlStreamStream>, Status> {
        let session_id = req
            .metadata()
            .get("session-id")
            .and_then(|v| v.to_str().ok())
            .ok_or_else(|| Status::unauthenticated("missing session-id metadata"))?
            .to_string();

        // 验签 session_id
        auth::verify_session_id(&session_id, &self.state.config.token)
            .map_err(|e| Status::unauthenticated(e.to_string()))?;

        let sessions = self.state.sessions.lock().await;
        let session = sessions
            .get(&session_id)
            .ok_or_else(|| Status::unauthenticated("session not found"))?
            .clone();
        drop(sessions);

        // 检查 session TTL（Login 后是否超时）
        if session.timer.is_expired() {
            self.state.sessions.lock().await.remove(&session_id);
            return Err(Status::unauthenticated("session expired, please re-login"));
        }

        // 建立 server→client 消息通道
        let (raw_tx, mut raw_rx) = mpsc::channel::<ServerMessage>(64);
        session.set_server_tx(raw_tx).await;

        let (out_tx, out_rx) = mpsc::channel::<Result<ServerMessage, Status>>(64);

        // 桥接：raw_rx → out_tx（带 Result 包装）
        let bridge_tx = out_tx.clone();
        tokio::spawn(async move {
            while let Some(msg) = raw_rx.recv().await {
                if bridge_tx.send(Ok(msg)).await.is_err() {
                    break;
                }
            }
        });

        // 心跳 Ping
        let hb_tx = out_tx.clone();
        let hb_secs = self.state.config.heartbeat_timeout_secs / 3;
        tokio::spawn(async move {
            let mut iv = tokio::time::interval(std::time::Duration::from_secs(hb_secs));
            loop {
                iv.tick().await;
                let msg = ServerMessage {
                    payload: Some(server_message::Payload::Ping(Ping {
                        timestamp: unix_now(),
                    })),
                };
                if hb_tx.send(Ok(msg)).await.is_err() {
                    break;
                }
            }
        });

        // 接收 client 消息，同时更新最后活跃时间用于超时检测
        let mut incoming = req.into_inner();
        let sess2 = session.clone();
        let sessions_map = self.state.sessions.clone();
        let sid = session_id.clone();
        let last_pong = Arc::new(std::sync::atomic::AtomicI64::new(unix_now()));
        // 同步到 session 的 last_pong，供端口抢占时判断 session 是否失活
        session.last_pong.store(unix_now(), std::sync::atomic::Ordering::Relaxed);

        // 超时检测任务
        let timeout_secs = self.state.config.heartbeat_timeout_secs as i64;
        let last_pong_watcher = last_pong.clone();
        let sess_timeout = session.clone();
        let sessions_timeout = self.state.sessions.clone();
        let sid_timeout = session_id.clone();
        tokio::spawn(async move {
            let mut iv = tokio::time::interval(std::time::Duration::from_secs(
                (timeout_secs / 3).max(5) as u64,
            ));
            loop {
                iv.tick().await;
                let last = last_pong_watcher.load(std::sync::atomic::Ordering::Relaxed);
                let now = unix_now();
                if now - last > timeout_secs {
                    warn!(session_id = %sid_timeout, "heartbeat timeout, closing session");
                    sess_timeout.shutdown().await;
                    sessions_timeout.lock().await.remove(&sid_timeout);
                    break;
                }
            }
        });

        tokio::spawn(async move {
            while let Ok(Some(msg)) = incoming.message().await {
                match msg.payload {
                    Some(client_message::Payload::Pong(p)) => {
                        debug!(session_id = %sid, ts = p.timestamp, "pong");
                        let now = unix_now();
                        last_pong.store(now, std::sync::atomic::Ordering::Relaxed);
                        sess2.last_pong.store(now, std::sync::atomic::Ordering::Relaxed);
                    }
                    Some(client_message::Payload::WorkConnAck(ack)) => {
                        debug!(session_id = %sid, stream_id = %ack.stream_id, success = ack.success, "work_conn_ack");
                    }
                    None => {}
                }
            }
            info!(session_id = %sid, "client disconnected");
            sess2.shutdown().await;
            sessions_map.lock().await.remove(&sid);
        });

        Ok(Response::new(tokio_stream::wrappers::ReceiverStream::new(
            out_rx,
        )))
    }

    // ── OpenWorkConn（TCP 模式专用） ──────────────────────────────────────────
    type OpenWorkConnStream =
        tokio_stream::wrappers::ReceiverStream<Result<WorkConnFrame, Status>>;

    async fn open_work_conn(
        &self,
        req: Request<Streaming<WorkConnFrame>>,
    ) -> Result<Response<Self::OpenWorkConnStream>, Status> {
        // session-id 必须在 metadata 里
        let session_id = req
            .metadata()
            .get("session-id")
            .and_then(|v| v.to_str().ok())
            .ok_or_else(|| Status::unauthenticated("missing session-id metadata"))?
            .to_string();

        auth::verify_session_id(&session_id, &self.state.config.token)
            .map_err(|e| Status::unauthenticated(e.to_string()))?;

        let mut incoming = req.into_inner();

        // 首帧必须是 stream_id
        let first = incoming
            .message()
            .await
            .map_err(|e| Status::internal(format!("read first frame: {e}")))?
            .ok_or_else(|| Status::invalid_argument("missing stream_id first frame"))?;

        let stream_id = match first.payload {
            Some(WcfPayload::StreamId(s)) => s,
            _ => {
                return Err(Status::invalid_argument(
                    "first frame must carry stream_id",
                ));
            }
        };

        // 找到 session
        let sessions = self.state.sessions.lock().await;
        let session = sessions
            .get(&session_id)
            .ok_or_else(|| Status::unauthenticated("session not found"))?
            .clone();
        drop(sessions);

        // 桥接通道：in_rx 拿远端字节，out_tx 发本地字节给远端
        let (in_tx, in_rx) = mpsc::channel::<bytes::Bytes>(64);
        let (out_tx_raw, mut out_rx_raw) = mpsc::channel::<bytes::Bytes>(64);

        // reader task：把 tonic Streaming 的 frame 拆出来塞到 in_tx
        tokio::spawn(async move {
            while let Ok(Some(frame)) = incoming.message().await {
                let chunk = match frame.payload {
                    Some(WcfPayload::Data(b)) => bytes::Bytes::from(b),
                    // 后续 stream_id 帧视为关闭
                    Some(WcfPayload::StreamId(_)) => break,
                    None => continue,
                };
                if in_tx.send(chunk).await.is_err() {
                    break;
                }
            }
        });

        // 输出 stream：从 out_rx_raw 取字节打包成 WorkConnFrame{data}
        let (frame_tx, frame_rx) = mpsc::channel::<Result<WorkConnFrame, Status>>(64);
        tokio::spawn(async move {
            while let Some(b) = out_rx_raw.recv().await {
                let frame = WorkConnFrame {
                    payload: Some(WcfPayload::Data(b.to_vec())),
                };
                if frame_tx.send(Ok(frame)).await.is_err() {
                    break;
                }
            }
        });

        // 构造 TonicStreamIo
        let io = TonicStreamIo::new(in_rx, out_tx_raw);
        let work_io: Box<dyn WorkIo> = Box::new(io);

        if !session.deliver_work_conn(&stream_id, work_io).await {
            // 没人接收：session 已结束或 stream_id 失配
            return Err(Status::not_found(format!(
                "no pending work conn for stream_id={stream_id}"
            )));
        }
        debug!(stream_id, "TCP work conn delivered");

        Ok(Response::new(tokio_stream::wrappers::ReceiverStream::new(
            frame_rx,
        )))
    }
}

impl ControlServiceImpl {
    /// 清理僵尸 session：扫描所有 session，找出 control stream 已断开或心跳超时的旧 session
    /// 跳过当前正在注册的 session（current_session_id）
    async fn evict_stale_sessions(&self, current_session_id: &str) {
        let timeout_secs = self.state.config.heartbeat_timeout_secs as i64;
        let now = unix_now();
        let mut to_evict = Vec::new();

        {
            let sessions = self.state.sessions.lock().await;
            for (id, s) in sessions.iter() {
                // 跳过当前 session
                if id == current_session_id {
                    continue;
                }

                // 判断是否为僵尸 session：
                // 1. last_pong 为 0 → control stream 从未建立（但 session 已过期）
                // 2. last_pong > 0 但距现在已超过心跳超时 → control stream 已断开
                let last = s.last_pong.load(std::sync::atomic::Ordering::Relaxed);
                let is_stale = if last == 0 {
                    s.timer.is_expired()
                } else {
                    now - last > timeout_secs
                };

                if is_stale {
                    warn!(session_id = %id, last_pong = last, "evicting stale session");
                    to_evict.push((id.clone(), s.clone()));
                }
            }
        }

        for (id, s) in &to_evict {
            s.shutdown().await;
            self.state.sessions.lock().await.remove(id);
            info!(session_id = %id, "stale session evicted, ports released");
        }
    }

    /// 强制释放被指定端口占用的 session（端口抢占）
    /// 当新客户端注册同一端口时，旧 session 会被清理
    /// 返回 true 表示成功释放（或端口本来就空闲）
    async fn force_release_port(&self, port: u16, current_session_id: &str) -> bool {
        let mut to_evict = Vec::new();

        {
            let sessions = self.state.sessions.lock().await;
            for (id, s) in sessions.iter() {
                if id == current_session_id {
                    continue;
                }
                let proxies = s.proxies.lock().await;
                let holds_port = proxies.values().any(|h| h.remote_port == port);
                if holds_port {
                    warn!(session_id = %id, port, "evicting session holding required port");
                    to_evict.push((id.clone(), s.clone()));
                }
            }
        }

        for (id, s) in &to_evict {
            s.shutdown().await;
            self.state.sessions.lock().await.remove(id);
            info!(session_id = %id, port, "session evicted for port takeover, ports released");
        }

        !to_evict.is_empty()
    }
}

fn unix_now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}

/// 通配域名匹配：pattern 形如 "*.example.com"，可匹配 "a.example.com"、"a.b.example.com"
/// 单域名直接相等比较即可，不走此函数
fn wildcard_matches(pattern: &str, name: &str) -> bool {
    if let Some(rest) = pattern.strip_prefix("*.") {
        if let Some(idx) = name.find('.') {
            &name[idx + 1..] == rest
        } else {
            false
        }
    } else {
        false
    }
}

// ─── QuicBiStream ─────────────────────────────────────────────────────────────

pub struct QuicBiStream {
    send: SendStream,
    recv: RecvStream,
}

impl QuicBiStream {
    pub fn new(send: SendStream, recv: RecvStream) -> Self {
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

impl tonic::transport::server::Connected for QuicBiStream {
    type ConnectInfo = SocketAddr;
    fn connect_info(&self) -> Self::ConnectInfo {
        "0.0.0.0:0".parse().unwrap()
    }
}
