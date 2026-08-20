mod control;
mod proxy;
mod quic;
pub(crate) mod websocket;
mod xhttp;

use anyhow::Result;
use tracing::info;
use tunx_common::config::ClientConfig;
use tunx_common::metrics::MetricsRegistry;

pub async fn run(cfg: ClientConfig, metrics: MetricsRegistry) -> Result<()> {
    info!("tunx client starting, server={}", cfg.server_addr);
    control::run(cfg, metrics).await
}
