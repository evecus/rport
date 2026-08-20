//! 运行时管理：持有当前生效配置、可重启的 server/client 任务、
//! 共享的 metrics registry。Web 模块通过这里读状态、改配置、触发热重启。

use std::path::PathBuf;
use std::sync::Arc;

use anyhow::Result;
use tokio::sync::RwLock;
use tokio::task::JoinHandle;
use tracing::{error, info, warn};

use tunx_common::config::{Mode, TunxConfig};
use tunx_common::metrics::MetricsRegistry;

/// server 模式下的运行时句柄：AppState 供状态查询，ServerHandle 持有所有真实
/// listener 的 JoinHandle，热重启时统一 abort
struct ServerRuntime {
    handle: crate::server::ServerHandle,
}

/// client 模式下的运行时句柄
struct ClientRuntime {
    handle: JoinHandle<()>,
}

enum RunningTask {
    Server(ServerRuntime),
    Client(ClientRuntime),
    /// 尚未配置完整（is_runnable() == false），仅 Web UI 可用
    None,
}

pub struct AppRuntime {
    config_path: PathBuf,
    /// 当前内存中生效的配置（与磁盘上的文件保持同步：每次保存都会先写文件再更新这里）
    pub config: RwLock<TunxConfig>,
    running: RwLock<RunningTask>,
    /// server 和 client 模式共用同一个 metrics registry 实例：
    /// 模式切换时不需要新建，key 命名空间天然不会冲突
    /// （server 用 "session_id::proxy_name"，client 用 "proxy_name"）
    pub metrics: MetricsRegistry,
}

impl AppRuntime {
    /// 启动时初始化：加载或生成配置文件，但不自动启动 server/client 逻辑
    /// （由调用方在构造完成后调用 `restart()` 来启动）
    pub async fn init(config_path: impl Into<PathBuf>) -> Result<Arc<Self>> {
        let config_path = config_path.into();
        let path_str = config_path.to_string_lossy().to_string();

        let cfg = match TunxConfig::from_file_loose(&path_str) {
            Ok(cfg) => cfg,
            Err(_) => {
                // 文件不存在或无法解析：生成一份空模板并写盘
                info!(
                    "config file '{path_str}' not found or invalid, generating empty template"
                );
                let cfg = TunxConfig::empty_template();
                cfg.save_to_file(&path_str)?;
                cfg
            }
        };

        let runtime = Arc::new(Self {
            config_path,
            config: RwLock::new(cfg),
            running: RwLock::new(RunningTask::None),
            metrics: MetricsRegistry::new(),
        });

        Ok(runtime)
    }

    pub fn config_path_str(&self) -> String {
        self.config_path.to_string_lossy().to_string()
    }

    /// 当前配置的只读快照
    pub async fn snapshot_config(&self) -> TunxConfig {
        self.config.read().await.clone()
    }

    /// 保存新配置：写盘 + 更新内存态 + 触发热重启
    /// 调用方需自行保证新配置的 `mode` 与其 `server`/`client` 段匹配
    /// （校验在 `restart()` 内部的 `is_runnable()` 检查中体现，不满足则只停不启）
    pub async fn save_and_restart(&self, new_cfg: TunxConfig) -> Result<()> {
        new_cfg.save_to_file(&self.config_path_str())?;
        {
            let mut guard = self.config.write().await;
            *guard = new_cfg;
        }
        self.restart().await
    }

    /// 停止当前运行中的 server/client 任务（若有），
    /// 用新的内存态配置重新启动。配置不完整（is_runnable() == false）时只停不启。
    pub async fn restart(&self) -> Result<()> {
        // 1. 停掉旧任务
        {
            let mut running = self.running.write().await;
            match std::mem::replace(&mut *running, RunningTask::None) {
                RunningTask::Server(rt) => {
                    rt.handle.abort_all();
                    info!("previous server listeners stopped");
                }
                RunningTask::Client(rt) => {
                    rt.handle.abort();
                    info!("previous client task stopped");
                }
                RunningTask::None => {}
            }
        }

        // 给操作系统一点时间释放刚刚关闭的监听端口，减少"端口仍被占用"的报错概率。
        // abort() 是异步取消，端口的实际释放并不与 abort() 调用同步发生。
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;

        let cfg = self.snapshot_config().await;
        if !cfg.is_runnable() {
            warn!("config is not complete yet, skip starting server/client (web UI only)");
            return Ok(());
        }

        match cfg.mode {
            Mode::Server => {
                let server_cfg = cfg
                    .server
                    .clone()
                    .ok_or_else(|| anyhow::anyhow!("mode=server but [server] section missing"))?;
                let metrics = self.metrics.clone();
                let handle = crate::server::run(server_cfg, metrics).await?;
                let mut running = self.running.write().await;
                *running = RunningTask::Server(ServerRuntime { handle });
            }
            Mode::Client => {
                let client_cfg = cfg
                    .client
                    .clone()
                    .ok_or_else(|| anyhow::anyhow!("mode=client but [client] section missing"))?;
                let metrics = self.metrics.clone();
                let handle = tokio::spawn(async move {
                    if let Err(e) = crate::client::run(client_cfg, metrics).await {
                        error!("client task ended with error: {e:#}");
                    }
                });
                let mut running = self.running.write().await;
                *running = RunningTask::Client(ClientRuntime { handle });
            }
        }

        Ok(())
    }

    /// 当前是否处于运行状态（server/client 逻辑已启动）
    pub async fn is_running(&self) -> bool {
        !matches!(&*self.running.read().await, RunningTask::None)
    }

    /// server 模式下获取 AppState（供 web 层查询 sessions）
    pub async fn server_state(&self) -> Option<crate::server::AppState> {
        match &*self.running.read().await {
            RunningTask::Server(rt) => Some(rt.handle.state.clone()),
            _ => None,
        }
    }
}
