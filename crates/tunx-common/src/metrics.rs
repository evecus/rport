//! 流量统计
//!
//! 每个 proxy 一份 `ProxyMetrics`，记录累计收发字节数。
//! 由调用方（server/client 的 proxy 转发代码）在数据搬运时调用
//! `add_sent` / `add_recv` 更新计数。
//!
//! 速率计算交给上层（web 模块）：定期采样累计值算 delta。

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use serde::Serialize;
use tokio::sync::RwLock;

/// 单个 proxy 的累计流量计数器
#[derive(Debug, Default)]
pub struct ProxyMetrics {
    /// 从公网 → 本地服务方向（“下行”，对隧道来说是服务端收到公网数据后转发给客户端）
    pub bytes_down: AtomicU64,
    /// 从本地服务 → 公网方向（“上行”）
    pub bytes_up: AtomicU64,
    /// 当前活跃连接数（TCP 建立/UDP 会话建立时 +1，结束时 -1）
    pub active_conns: AtomicU64,
    /// 累计连接数（只增不减，用于展示“历史连接总数”）
    pub total_conns: AtomicU64,
}

impl ProxyMetrics {
    pub fn add_down(&self, n: u64) {
        self.bytes_down.fetch_add(n, Ordering::Relaxed);
    }
    pub fn add_up(&self, n: u64) {
        self.bytes_up.fetch_add(n, Ordering::Relaxed);
    }
    pub fn conn_start(&self) {
        self.active_conns.fetch_add(1, Ordering::Relaxed);
        self.total_conns.fetch_add(1, Ordering::Relaxed);
    }
    pub fn conn_end(&self) {
        // 饱和减，避免并发场景下下溢
        let _ = self
            .active_conns
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |v| {
                Some(v.saturating_sub(1))
            });
    }

    pub fn snapshot(&self) -> ProxyMetricsSnapshot {
        ProxyMetricsSnapshot {
            bytes_down: self.bytes_down.load(Ordering::Relaxed),
            bytes_up: self.bytes_up.load(Ordering::Relaxed),
            active_conns: self.active_conns.load(Ordering::Relaxed),
            total_conns: self.total_conns.load(Ordering::Relaxed),
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Default)]
pub struct ProxyMetricsSnapshot {
    pub bytes_down: u64,
    pub bytes_up: u64,
    pub active_conns: u64,
    pub total_conns: u64,
}

/// 全局 metrics 注册表：proxy_name → ProxyMetrics
///
/// server 端以 "session_id::proxy_name" 作为 key（同名 proxy 可能来自不同 session）；
/// client 端直接以 proxy_name 作为 key（本地只有一份配置，不会重名）。
#[derive(Debug, Default, Clone)]
pub struct MetricsRegistry {
    inner: Arc<RwLock<HashMap<String, Arc<ProxyMetrics>>>>,
}

impl MetricsRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// 获取或创建某个 key 对应的 ProxyMetrics
    pub async fn get_or_create(&self, key: &str) -> Arc<ProxyMetrics> {
        {
            let map = self.inner.read().await;
            if let Some(m) = map.get(key) {
                return m.clone();
            }
        }
        let mut map = self.inner.write().await;
        map.entry(key.to_string())
            .or_insert_with(|| Arc::new(ProxyMetrics::default()))
            .clone()
    }

    /// 移除某个 key（proxy 注销 / session 结束时调用）
    pub async fn remove(&self, key: &str) {
        self.inner.write().await.remove(key);
    }

    /// 批量移除某个前缀下的所有 key（session 结束时清理该 session 名下所有 proxy）
    pub async fn remove_prefix(&self, prefix: &str) {
        let mut map = self.inner.write().await;
        map.retain(|k, _| !k.starts_with(prefix));
    }

    /// 导出全部快照
    pub async fn snapshot_all(&self) -> HashMap<String, ProxyMetricsSnapshot> {
        let map = self.inner.read().await;
        map.iter().map(|(k, v)| (k.clone(), v.snapshot())).collect()
    }
}
