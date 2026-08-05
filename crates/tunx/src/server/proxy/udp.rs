use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::{Duration, Instant};

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::UdpSocket;
use tokio::sync::{oneshot, Mutex};
use tracing::{debug, error, info, warn};
use tunx_common::stream::WorkIo;

use crate::server::session::Session;

/// UDP 数据报在 QUIC stream 上的帧格式（小端序）：
///
///   ┌──────────┬───────────────┐
///   │ len: u16 │ payload: [u8] │
///   └──────────┴───────────────┘
///
/// 每条 QUIC stream 对应一个 (公网客户端 peer addr) 会话。
/// 服务端为每个新的 peer addr 请求一条新的 WorkConn stream。
const UDP_SESSION_TIMEOUT: Duration = Duration::from_secs(60);
const MAX_UDP_PAYLOAD: usize = 65535;

struct UdpSession {
    /// 向 QUIC stream 发数据的 channel（把 datagram 帧化后写入）
    tx: tokio::sync::mpsc::Sender<Vec<u8>>,
    last_active: Instant,
}

pub async fn start_udp_proxy(
    proxy_name: String,
    local_addr: String,
    remote_port: u16,
    session: Arc<Session>,
    mut shutdown_rx: oneshot::Receiver<()>,
) {
    let bind = format!("0.0.0.0:{remote_port}");
    let socket = match UdpSocket::bind(&bind).await {
        Ok(s) => Arc::new(s),
        Err(e) => {
            error!(proxy = %proxy_name, "bind UDP {bind} failed: {e}");
            return;
        }
    };
    info!(proxy = %proxy_name, "UDP proxy listening :{remote_port} → {local_addr}");

    // peer_addr → UdpSession 映射表
    let sessions: Arc<Mutex<HashMap<SocketAddr, UdpSession>>> =
        Arc::new(Mutex::new(HashMap::new()));

    // 后台 task：定期清理超时的 UDP 会话
    {
        let sessions = sessions.clone();
        let name = proxy_name.clone();
        tokio::spawn(async move {
            let mut iv = tokio::time::interval(Duration::from_secs(30));
            loop {
                iv.tick().await;
                let mut map = sessions.lock().await;
                let before = map.len();
                map.retain(|_, s| s.last_active.elapsed() < UDP_SESSION_TIMEOUT);
                let removed = before - map.len();
                if removed > 0 {
                    debug!(proxy = %name, "cleaned {removed} expired UDP sessions");
                }
            }
        });
    }

    let mut buf = vec![0u8; MAX_UDP_PAYLOAD];
    loop {
        tokio::select! {
            result = socket.recv_from(&mut buf) => {
                match result {
                    Ok((len, peer)) => {
                        let data = buf[..len].to_vec();
                        handle_incoming_datagram(
                            proxy_name.clone(),
                            peer,
                            data,
                            socket.clone(),
                            session.clone(),
                            sessions.clone(),
                        )
                        .await;
                    }
                    Err(e) => warn!(proxy = %proxy_name, "recv_from: {e}"),
                }
            }
            _ = &mut shutdown_rx => {
                info!(proxy = %proxy_name, "UDP proxy shutting down");
                break;
            }
        }
    }
}

async fn handle_incoming_datagram(
    proxy_name: String,
    peer: SocketAddr,
    data: Vec<u8>,
    socket: Arc<UdpSocket>,
    session: Arc<Session>,
    sessions: Arc<Mutex<HashMap<SocketAddr, UdpSession>>>,
) {
    let mut map = sessions.lock().await;

    if let Some(udp_sess) = map.get_mut(&peer) {
        // 已有会话，直接转发
        udp_sess.last_active = Instant::now();
        let tx = udp_sess.tx.clone();
        drop(map);
        if tx.send(data).await.is_err() {
            debug!(proxy = %proxy_name, %peer, "UDP session tx closed");
        }
        return;
    }

    // 新 peer：请求一条 WorkConn stream
    drop(map);

    let work_rx = match session.request_work_conn(&proxy_name).await {
        Ok(rx) => rx,
        Err(e) => {
            warn!(proxy = %proxy_name, %peer, "request_work_conn: {e}");
            return;
        }
    };

    // 等待客户端开好 WorkConn
    let work_io: Box<dyn WorkIo> =
        match tokio::time::timeout(Duration::from_secs(10), work_rx).await {
            Ok(Ok(io)) => io,
            Ok(Err(_)) => {
                warn!(proxy = %proxy_name, %peer, "work conn sender dropped");
                return;
            }
            Err(_) => {
                warn!(proxy = %proxy_name, %peer, "work conn timeout");
                return;
            }
        };

    // 建立 datagram 发送 channel
    let (tx, rx) = tokio::sync::mpsc::channel::<Vec<u8>>(256);

    {
        let mut map = sessions.lock().await;
        map.insert(
            peer,
            UdpSession {
                tx: tx.clone(),
                last_active: Instant::now(),
            },
        );
    }

    // 发送第一个数据报
    let _ = tx.send(data).await;

    // 启动双向转发 task
    let name = proxy_name.clone();
    tokio::spawn(async move {
        if let Err(e) =
            run_udp_tunnel(name.clone(), peer, work_io, rx, socket, sessions.clone()).await
        {
            debug!(proxy = %name, %peer, "UDP tunnel: {e}");
        }
        // 会话结束时从 map 删除
        sessions.lock().await.remove(&peer);
    });
}

/// 单个 peer 的双向 UDP ↔ WorkConn（QUIC 或 HTTP/2 stream）转发
async fn run_udp_tunnel(
    proxy_name: String,
    peer: SocketAddr,
    mut work_io: Box<dyn WorkIo>,
    mut from_public: tokio::sync::mpsc::Receiver<Vec<u8>>,
    socket: Arc<UdpSocket>,
    _sessions: Arc<Mutex<HashMap<SocketAddr, UdpSession>>>,
) -> anyhow::Result<()> {
    debug!(proxy = %proxy_name, %peer, "UDP tunnel started");

    let mut recv_buf = vec![0u8; MAX_UDP_PAYLOAD + 2];

    loop {
        tokio::select! {
            // 公网 → WorkConn（加帧头）
            maybe = from_public.recv() => {
                match maybe {
                    Some(data) => {
                        let len = data.len() as u16;
                        let mut frame = Vec::with_capacity(2 + data.len());
                        frame.extend_from_slice(&len.to_le_bytes());
                        frame.extend_from_slice(&data);
                        work_io.write_all(&frame).await?;
                    }
                    None => break,
                }
            }
            // WorkConn → 公网（解帧头）
            result = work_io.read_exact(&mut recv_buf[..2]) => {
                result?;
                let len = u16::from_le_bytes([recv_buf[0], recv_buf[1]]) as usize;
                if len > MAX_UDP_PAYLOAD {
                    anyhow::bail!("UDP frame too large: {len}");
                }
                work_io.read_exact(&mut recv_buf[..len]).await?;
                socket.send_to(&recv_buf[..len], peer).await?;
            }
        }
    }

    debug!(proxy = %proxy_name, %peer, "UDP tunnel closed");
    Ok(())
}
