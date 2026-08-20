mod client;
mod runtime;
mod server;
mod web;

use std::sync::Arc;

use anyhow::Result;
use clap::Parser;
use tracing::{info, warn};
use tunx_common::config::Mode;

const VERSION: &str = env!("CARGO_PKG_VERSION");

#[derive(Parser, Debug)]
#[command(name = "tunx", about = "tunx — a lightweight NAT traversal tool", version = VERSION)]
struct Cli {
    /// 配置文件路径；不存在时会自动生成一份空模板
    #[arg(short, long, default_value = "config.toml")]
    config: String,
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();

    // 加载或生成配置（宽松模式：允许 [server]/[client] 段缺失，此时只跑 Web UI）
    let app = runtime::AppRuntime::init(&cli.config).await?;

    // 初始化日志：配置不完整时用默认 info 级别
    let log_level = {
        let cfg = app.snapshot_config().await;
        match cfg.mode {
            Mode::Server => cfg
                .server
                .as_ref()
                .map(|s| s.log_level.clone())
                .unwrap_or_else(|| "info".to_string()),
            Mode::Client => cfg
                .client
                .as_ref()
                .map(|c| c.log_level.clone())
                .unwrap_or_else(|| "info".to_string()),
        }
    };
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new(log_level)),
        )
        .init();

    info!("tunx v{VERSION} starting, config file: {}", cli.config);

    // 首次启动 Web 登录凭据：若配置里没有 password_hash，生成随机密码，
    // 写回配置文件，并在日志里打印一次（这是找回密码的唯一方式，请提醒用户记录）
    web::auth::ensure_web_credentials(&app).await?;

    // 若配置已经完整可运行，启动 server/client 逻辑；
    // 否则保持"待配置"状态，只跑 Web UI，用户在 UI 里补全配置后会自动触发启动
    if app.snapshot_config().await.is_runnable() {
        if let Err(e) = app.restart().await {
            warn!("failed to start server/client on boot: {e:#}");
        }
    } else {
        warn!(
            "config at '{}' is incomplete — open the Web UI to finish setup",
            cli.config
        );
    }

    // 启动 Web UI（固定监听 [web].listen，默认 0.0.0.0:1080），阻塞到进程退出
    let app_for_web = Arc::clone(&app);
    web::serve(app_for_web).await
}
