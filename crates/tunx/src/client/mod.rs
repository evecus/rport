mod control;
mod proxy;
mod quic;
pub(crate) mod websocket;
mod xhttp;

use anyhow::Result;
use tunx_common::config::ClientConfig;
use tracing::info;

pub async fn run(cfg: ClientConfig) -> Result<()> {
    info!("tunx client starting, server={}", cfg.server_addr);
    control::run(cfg).await
}
