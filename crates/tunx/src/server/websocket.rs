//! WebSocket 服务端
//!
//! 接受 TCP+TLS+WebSocket 连接，在其上运行 gRPC 控制服务
//! CDN 友好：CDN 可代理 WebSocket 流量

use anyhow::Result;
use tokio::net::TcpListener;
use tonic::transport::Server as TonicServer;
use tracing::{debug, info, warn};

use tunx_proto::control_service_server::ControlServiceServer;

use crate::client::websocket::WsBiStream;
use crate::server::control::{AppState, ControlServiceImpl};

pub async fn run_websocket(listener: TcpListener, state: AppState) -> Result<()> {
    let acceptor = state
        .tcp_tls_acceptor
        .clone()
        .ok_or_else(|| anyhow::anyhow!("websocket requires acme/manual TLS mode"))?;

    info!("WebSocket+TLS listening");

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
            info!(%peer, "WebSocket connected");

            // 1. TLS 握手
            let tls_stream = match acceptor.accept(tcp).await {
                Ok(s) => s,
                Err(e) => {
                    warn!(%peer, "TLS handshake failed: {e}");
                    return;
                }
            };

            // 2. WebSocket upgrade（tokio_tungstenite 自动处理 HTTP 升级握手）
            let ws_stream = match tokio_tungstenite::accept_async(tls_stream).await {
                Ok(s) => s,
                Err(e) => {
                    warn!(%peer, "WebSocket upgrade failed: {e}");
                    return;
                }
            };

            // 3. 在 WebSocket 字节流上运行 gRPC
            let io = WsBiStream::new(ws_stream);
            let svc = ControlServiceImpl {
                state: state.clone(),
            };
            if let Err(e) = TonicServer::builder()
                .add_service(ControlServiceServer::new(svc))
                .serve_with_incoming(futures::stream::once(
                    async move { Ok::<_, std::io::Error>(io) },
                ))
                .await
            {
                debug!(%peer, "gRPC over WebSocket ended: {e}");
            }
        });
    }
}
