mod acme;
mod auth;
mod control;
mod ports;
mod proxy;
mod session;
mod tls;
mod websocket;
mod xhttp;

use anyhow::Result;
use tracing::info;
use tunx_common::config::ServerConfig;
use tunx_common::metrics::MetricsRegistry;

pub use control::{metrics_key, AppState, ServerHandle};

pub async fn run(cfg: ServerConfig, metrics: MetricsRegistry) -> Result<ServerHandle> {
    info!("tunx server starting, bind={}", cfg.bind_addr);

    // 构建 TLS 资源（QUIC + cert + SAN）
    let server_tls = tls::build_server_tls(&cfg).await?;

    // acme 模式下启动后台续签任务
    if let tunx_common::config::ServerTlsConfig::Acme {
        ref domain,
        ref email,
        ref cf_api_token,
        ref cache_dir,
        staging,
    } = cfg.tls
    {
        let days = acme::cert_expires_in_days(cache_dir);
        info!("ACME: certificate valid for {days} days");
        acme::spawn_renewal_task(
            domain.clone(),
            email.clone(),
            cache_dir.clone(),
            staging,
            cf_api_token.clone(),
        );
    }

    // 端口管理器
    let port_mgr = ports::PortManager::new(cfg.proxy_port_range);

    // 启动所有 listener（在后台 task 中运行）并返回 AppState 供 Web UI
    // 查询运行时状态（session 列表、metrics 等）
    control::run(cfg, server_tls, port_mgr, metrics).await
}
