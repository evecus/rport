use tokio::io::{AsyncRead, AsyncWrite};
use tracing::debug;

/// 双向转发两个异步流，任意一侧关闭则结束
/// 返回 (从A读了多少字节, 从B读了多少字节)
pub async fn copy_bidirectional<A, B>(
    proxy_name: &str,
    mut a: A,
    mut b: B,
) -> std::io::Result<(u64, u64)>
where
    A: AsyncRead + AsyncWrite + Unpin,
    B: AsyncRead + AsyncWrite + Unpin,
{
    let result = tokio::io::copy_bidirectional(&mut a, &mut b).await;
    match &result {
        Ok((a2b, b2a)) => {
            debug!(proxy = proxy_name, a2b, b2a, "tunnel closed");
        }
        Err(e) => {
            debug!(proxy = proxy_name, err = %e, "tunnel error");
        }
    }
    result
}
