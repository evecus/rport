//! 透明流量计数包装器
//!
//! 包裹任意 `AsyncRead + AsyncWrite`，在读写时把字节数累加到 `ProxyMetrics`。
//! 用于 `tokio::io::copy_bidirectional` 转发路径上做流量统计，不侵入原有转发逻辑。

use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};

use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};

use crate::metrics::ProxyMetrics;

/// 统计口径统一约定："up" = 公网访问者 → 本地服务方向，"down" = 本地服务 → 公网访问者方向。
/// 这与 [`ProxyMetrics`] 里 `bytes_up`/`bytes_down` 的语义一致。
///
/// 用 `copy_bidirectional(&mut a, &mut b)` 转发时，只需要包裹其中一侧（另一侧保持原样），
/// 因为一侧的 read 恰好对应另一侧的 write，包一侧即可拿到完整的双向字节数，避免重复计数。
///
/// - 包裹"公网侧" socket（server 上是 `public` TcpStream，即公网访问者直接连的那个 socket）：
///   用 [`CountingIo::wrap_public_side`]。这一侧的 read = 公网访问者发来的数据 = up；
///   这一侧的 write = 要发给公网访问者的数据 = down。
/// - 包裹"本地服务"侧 socket（client 上连接 `local_addr` 的那个 TcpStream）：
///   用 [`CountingIo::wrap_local_side`]。这一侧的 read = 本地服务返回的数据 = down；
///   这一侧的 write = 要发给本地服务的数据 = up。
pub struct CountingIo<T> {
    inner: T,
    metrics: Arc<ProxyMetrics>,
    /// true：read 计 up / write 计 down（"公网侧"用法）
    /// false：read 计 down / write 计 up（"本地服务侧"用法）
    read_is_up: bool,
}

impl<T> CountingIo<T> {
    /// 包裹"公网访问者"一侧的 socket（server 端 proxy 的 `public` 连接）
    pub fn wrap_public_side(inner: T, metrics: Arc<ProxyMetrics>) -> Self {
        Self {
            inner,
            metrics,
            read_is_up: true,
        }
    }

    /// 包裹"本地服务"一侧的 socket（client 端 proxy 连接 `local_addr` 的连接）
    pub fn wrap_local_side(inner: T, metrics: Arc<ProxyMetrics>) -> Self {
        Self {
            inner,
            metrics,
            read_is_up: false,
        }
    }
}

impl<T: AsyncRead + Unpin> AsyncRead for CountingIo<T> {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        let this = self.get_mut();
        let before = buf.filled().len();
        let poll = Pin::new(&mut this.inner).poll_read(cx, buf);
        if let Poll::Ready(Ok(())) = &poll {
            let n = (buf.filled().len() - before) as u64;
            if n > 0 {
                if this.read_is_up {
                    this.metrics.add_up(n);
                } else {
                    this.metrics.add_down(n);
                }
            }
        }
        poll
    }
}

impl<T: AsyncWrite + Unpin> AsyncWrite for CountingIo<T> {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        data: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        let this = self.get_mut();
        let poll = Pin::new(&mut this.inner).poll_write(cx, data);
        if let Poll::Ready(Ok(n)) = &poll {
            if *n > 0 {
                if this.read_is_up {
                    this.metrics.add_down(*n as u64);
                } else {
                    this.metrics.add_up(*n as u64);
                }
            }
        }
        poll
    }
    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.get_mut().inner).poll_flush(cx)
    }
    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.get_mut().inner).poll_shutdown(cx)
    }
}
