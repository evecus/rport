use anyhow::{Context, Result};
use quinn::{Connection, Endpoint};
use tracing::info;
use tunx_common::config::ClientConfig;
use tunx_common::quic::{client_config_skip_verify, client_config_verified};

pub async fn connect(cfg: &ClientConfig) -> Result<Connection> {
    // 解析服务端地址
    let server_addr = tokio::net::lookup_host(&cfg.server_addr)
        .await
        .with_context(|| format!("resolve {}", cfg.server_addr))?
        .next()
        .with_context(|| format!("no address for {}", cfg.server_addr))?;

    // 取 hostname（用于 TLS SNI）
    // 优先使用显式配置的 tls_sni，否则从 server_addr 提取
    let hostname = cfg.tls_sni.clone().unwrap_or_else(|| {
        cfg.server_addr
            .split(':')
            .next()
            .unwrap_or("localhost")
            .to_string()
    });

    // 构建 QUIC client config
    let client_cfg = if cfg.tls_skip_verify {
        client_config_skip_verify(&cfg.quic)
    } else {
        client_config_verified(&cfg.quic)?
    };

    // 绑定本地端口（0 = 随机）
    let mut endpoint = Endpoint::client("0.0.0.0:0".parse()?)?;
    endpoint.set_default_client_config(client_cfg);

    info!("connecting to {server_addr} (sni={hostname})");
    let conn = endpoint
        .connect(server_addr, &hostname)?
        .await
        .context("QUIC handshake failed")?;

    info!("QUIC connection established");
    Ok(conn)
}
