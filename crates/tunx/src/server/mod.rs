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
use tunx_common::config::ServerConfig;
use tracing::info;

pub async fn run(cfg: ServerConfig) -> Result<()> {
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

    // 启动 QUIC listener + gRPC 控制服务
    control::run(cfg, server_tls, port_mgr).await
}
