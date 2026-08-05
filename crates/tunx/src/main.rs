mod client;
mod server;

use anyhow::{Context, Result};
use clap::Parser;
use tunx_common::config::{Mode, TunxConfig};

const VERSION: &str = env!("CARGO_PKG_VERSION");

#[derive(Parser, Debug)]
#[command(name = "tunx", about = "tunx — a lightweight NAT traversal tool", version = VERSION)]
struct Cli {
    /// 配置文件路径
    #[arg(short, long, default_value = "config.toml")]
    config: String,
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    let cfg = TunxConfig::from_file(&cli.config)
        .with_context(|| format!("load config from {}", cli.config))?;

    // 初始化日志
    let log_level = match &cfg.mode {
        Mode::Server => cfg
            .server
            .as_ref()
            .map(|s| s.log_level.as_str())
            .unwrap_or("info"),
        Mode::Client => cfg
            .client
            .as_ref()
            .map(|c| c.log_level.as_str())
            .unwrap_or("info"),
    };
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new(log_level)),
        )
        .init();

    match cfg.mode {
        Mode::Server => {
            let server_cfg = cfg
                .server
                .ok_or_else(|| anyhow::anyhow!("mode=server but [server] section is missing"))?;
            server::run(server_cfg).await
        }
        Mode::Client => {
            let client_cfg = cfg
                .client
                .ok_or_else(|| anyhow::anyhow!("mode=client but [client] section is missing"))?;
            client::run(client_cfg).await
        }
    }
}
