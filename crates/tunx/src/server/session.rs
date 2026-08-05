use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::AtomicI64;

use tunx_common::stream::WorkIo;
use tokio::sync::{mpsc, oneshot, Mutex, RwLock};
use tracing::{info, warn};

use crate::server::auth::SessionTimer;
use crate::server::ports::PortManager;
use crate::server::proxy::ProxyHandle;
use tunx_proto::{server_message, ServerMessage, WorkConnRequest};

pub struct Session {
    pub session_id: String,
    #[allow(dead_code)]
    pub client_id: String,
    /// 建立时间 + ControlStream deadline
    pub timer: SessionTimer,
    /// 向 ControlStream 发 ServerMessage 的通道（ControlStream 建立后注入）
    pub server_tx: RwLock<Option<mpsc::Sender<ServerMessage>>>,
    /// 已注册的代理
    pub proxies: Mutex<HashMap<String, ProxyHandle>>,
    /// 等待中的 WorkConn：stream_id → oneshot sender
    pub pending_work: Mutex<HashMap<String, PendingWork>>,
    /// 端口管理器引用：shutdown 时释放占用的端口
    pub port_mgr: Arc<PortManager>,
    /// 最后一次收到 Pong 的时间戳（unix 秒）
    /// 用于判断 session 是否已失活，端口抢占时清理僵尸 session
    pub last_pong: AtomicI64,
}

pub struct PendingWork {
    pub stream_tx: oneshot::Sender<Box<dyn WorkIo>>,
}

impl Session {
    /// `control_stream_deadline_secs`：Login 后必须在此秒数内建立 ControlStream
    pub fn new(
        session_id: String,
        client_id: String,
        control_stream_deadline_secs: u64,
        port_mgr: Arc<PortManager>,
    ) -> Arc<Self> {
        Arc::new(Self {
            session_id,
            client_id,
            timer: SessionTimer::new(control_stream_deadline_secs),
            server_tx: RwLock::new(None),
            proxies: Mutex::new(HashMap::new()),
            pending_work: Mutex::new(HashMap::new()),
            port_mgr,
            last_pong: AtomicI64::new(0),
        })
    }

    pub async fn set_server_tx(&self, tx: mpsc::Sender<ServerMessage>) {
        *self.server_tx.write().await = Some(tx);
    }

    pub async fn add_proxy(&self, name: String, handle: ProxyHandle) {
        self.proxies.lock().await.insert(name, handle);
    }

    pub async fn request_work_conn(
        &self,
        proxy_name: &str,
    ) -> anyhow::Result<oneshot::Receiver<Box<dyn WorkIo>>> {
        let stream_id = uuid::Uuid::new_v4().to_string();
        let (tx, rx) = oneshot::channel();

        self.pending_work
            .lock()
            .await
            .insert(stream_id.clone(), PendingWork { stream_tx: tx });

        let guard = self.server_tx.read().await;
        let sender = guard
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("control stream not yet established"))?;

        sender
            .send(ServerMessage {
                payload: Some(server_message::Payload::WorkConnReq(WorkConnRequest {
                    proxy_name: proxy_name.to_string(),
                    stream_id,
                })),
            })
            .await
            .map_err(|_| anyhow::anyhow!("control stream closed"))?;

        Ok(rx)
    }

    pub async fn deliver_work_conn(
        &self,
        stream_id: &str,
        work_io: Box<dyn WorkIo>,
    ) -> bool {
        if let Some(pw) = self.pending_work.lock().await.remove(stream_id) {
            pw.stream_tx.send(work_io).is_ok()
        } else {
            warn!(stream_id, "no pending work conn found");
            false
        }
    }

    pub async fn shutdown(&self) {
        let mut proxies = self.proxies.lock().await;
        for (name, handle) in proxies.drain() {
            // 先释放端口，再关闭代理 task
            self.port_mgr.release(handle.remote_port);
            info!(proxy = %name, port = handle.remote_port, "released port");
            if handle.shutdown_tx.send(()).is_err() {
                warn!(proxy = %name, "proxy already stopped");
            }
        }
    }
}
