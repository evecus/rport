pub mod tcp;
pub mod udp;

use tokio::sync::oneshot;

/// 代理运行句柄
#[allow(dead_code)]
pub struct ProxyHandle {
    pub local_addr: String,
    pub remote_port: u16,
    /// 发送 () 触发代理关闭
    pub shutdown_tx: oneshot::Sender<()>,
}
