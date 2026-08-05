mod control;
mod proxy;
mod quic;
pub(crate) mod websocket;
mod xhttp;

use anyhow::Result;
use tracing::info;
use tunx_common::config::ClientConfig;

pub async fn run(cfg: ClientConfig) -> Result<()> {
    info!("tunx client starting, server={}", cfg.server_addr);
    control::run(cfg).await
}
