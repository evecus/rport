//! XHTTP 服务端
//!
//! TLS + HTTP/2 + gRPC，技术实现与 TCP 模式相同
//! 设计目标：在 CDN 后端运行，处理标准 HTTPS 流量
//! CDN 可直接代理 gRPC over HTTP/2 流量

use anyhow::Result;
use tokio::net::TcpListener;

use crate::server::control::{run_tcp, AppState};

pub async fn run_xhttp(listener: TcpListener, state: AppState) -> Result<()> {
    // XHTTP 复用 TCP 路径：TLS + HTTP/2 + gRPC
    run_tcp(listener, state).await
}
