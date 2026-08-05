//! WorkConn 的 IO 抽象与 TCP 模式桥接器
//!
//! 抽象目标：让上层 proxy 代码不感知底层是 QUIC stream 还是 HTTP/2 stream。
//!
//! - QUIC 路径：底层是 `tokio::io::Join<RecvStream, SendStream>`，直接实现 AsyncRead+AsyncWrite
//! - TCP  路径：底层是 tonic 的双向 streaming RPC，桥接 frame ↔ bytes
//!
//! 设计：`TonicStreamIo` 持有
//!   - 入向 mpsc::Receiver<Bytes>：reader task 把 WorkConnFrame.data 拆出来塞进来
//!   - 出向 PollSender<Bytes>：实现正确的 poll-based 背压

use std::pin::Pin;
use std::task::{Context, Poll};
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio::sync::mpsc;
use tokio_util::sync::PollSender;

/// 抽象 trait：AsyncRead + AsyncWrite + Send + Unpin
/// QUIC 的 Join 和 TCP 的 TonicStreamIo 都实现它
pub trait WorkIo: AsyncRead + AsyncWrite + Send + Unpin {}
impl<T: AsyncRead + AsyncWrite + Send + Unpin> WorkIo for T {}

/// 给 tonic streaming RPC 用的桥接 IO。
///
/// - `in_rx`：从远端收到的字节流（reader task 把 WorkConnFrame.data 拆出来塞进来）
/// - `out_tx`：本地要发出去的字节（writer task 把 Bytes 重新打包成 WorkConnFrame{data} 推到 tonic）
pub struct TonicStreamIo {
    in_rx: mpsc::Receiver<bytes::Bytes>,
    /// 当前 frame 的剩余未读字节
    cur: bytes::Bytes,
    /// PollSender 提供正确的 poll-based 写入语义
    out_tx: PollSender<bytes::Bytes>,
}

impl TonicStreamIo {
    pub fn new(
        in_rx: mpsc::Receiver<bytes::Bytes>,
        out_tx: mpsc::Sender<bytes::Bytes>,
    ) -> Self {
        Self {
            in_rx,
            cur: bytes::Bytes::new(),
            out_tx: PollSender::new(out_tx),
        }
    }
}

impl AsyncRead for TonicStreamIo {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        let this = self.get_mut();

        // 当前 frame 还有剩余
        if !this.cur.is_empty() {
            let n = std::cmp::min(this.cur.len(), buf.remaining());
            buf.put_slice(&this.cur.split_to(n));
            return Poll::Ready(Ok(()));
        }

        // 取下一帧
        match this.in_rx.poll_recv(cx) {
            Poll::Pending => Poll::Pending,
            Poll::Ready(None) => Poll::Ready(Ok(())), // EOF
            Poll::Ready(Some(chunk)) => {
                if chunk.is_empty() {
                    return Poll::Ready(Ok(()));
                }
                let n = std::cmp::min(chunk.len(), buf.remaining());
                buf.put_slice(&chunk[..n]);
                if n < chunk.len() {
                    this.cur = chunk.slice(n..);
                }
                Poll::Ready(Ok(()))
            }
        }
    }
}

impl AsyncWrite for TonicStreamIo {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        data: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        let this = self.get_mut();
        match Pin::new(&mut this.out_tx).poll_reserve(cx) {
            Poll::Ready(Ok(())) => {
                // reserved，发送
                let chunk = bytes::Bytes::copy_from_slice(data);
                match this.out_tx.send_item(chunk) {
                    Ok(()) => Poll::Ready(Ok(data.len())),
                    Err(_) => Poll::Ready(Err(std::io::Error::new(
                        std::io::ErrorKind::BrokenPipe,
                        "out channel closed",
                    ))),
                }
            }
            Poll::Ready(Err(_)) => Poll::Ready(Err(std::io::Error::new(
                std::io::ErrorKind::BrokenPipe,
                "out channel closed",
            ))),
            Poll::Pending => Poll::Pending,
        }
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        // PollSender 没有独立 flush 概念，poll_reserve 已背压
        let _ = cx;
        Poll::Ready(Ok(()))
    }

    fn poll_shutdown(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        let this = self.get_mut();
        // 关闭发送方向：close PollSender
        this.out_tx.close();
        Poll::Ready(Ok(()))
    }
}
