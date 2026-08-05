use anyhow::Result;
use quinn::Connection;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::UdpSocket;
use tonic::metadata::MetadataValue;
use tonic::Request;
use tracing::debug;

use tunx_common::quic::{STREAM_ID_LEN, WORK_CONN_MAGIC};
use tunx_common::stream::{TonicStreamIo, WorkIo};
use tunx_proto::{
    control_service_client::ControlServiceClient, work_conn_frame::Payload as WcfPayload,
    WorkConnFrame,
};

const MAX_UDP_PAYLOAD: usize = 65535;

/// QUIC 路径：服务端发来 WorkConnRequest（UDP 类型）后，客户端执行：
///   1. open_bi 开新 QUIC stream
///   2. 写 magic(4) + stream_id(36) header
///   3. 绑定临时本地 UDP socket，连接到 local_addr
///   4. 双向转发：QUIC stream ↔ 本地 UDP socket
///
/// 帧格式（与服务端一致）：
///   ┌──────────┬───────────────┐
///   │ len: u16 │ payload: [u8] │  (小端序)
///   └──────────┴───────────────┘
pub async fn handle_work_conn(
    conn: Connection,
    proxy_name: String,
    local_addr: String,
    stream_id: String,
) -> Result<()> {
    // 1. 开新 QUIC 双向 stream
    let (mut send, mut recv) = conn
        .open_bi()
        .await
        .map_err(|e| anyhow::anyhow!("open_bi: {e}"))?;

    // 2. 写 header
    if stream_id.len() != STREAM_ID_LEN {
        anyhow::bail!("stream_id len {} != {STREAM_ID_LEN}", stream_id.len());
    }
    let mut header = [0u8; 4 + STREAM_ID_LEN];
    header[..4].copy_from_slice(WORK_CONN_MAGIC);
    header[4..].copy_from_slice(stream_id.as_bytes());
    send.write_all(&header).await?;

    debug!(proxy = %proxy_name, stream_id, "UDP work stream opened, connecting {local_addr}");

    // 3. 绑定本地临时 UDP socket 并 connect 到目标地址
    let udp = UdpSocket::bind("0.0.0.0:0").await?;
    udp.connect(&local_addr).await?;

    // 4. 双向转发
    let mut buf = vec![0u8; MAX_UDP_PAYLOAD];
    let mut len_buf = [0u8; 2];

    loop {
        tokio::select! {
            // 本地 UDP → QUIC stream（加帧头）
            result = udp.recv(&mut buf) => {
                let n = result?;
                let len = n as u16;
                let mut frame = Vec::with_capacity(2 + n);
                frame.extend_from_slice(&len.to_le_bytes());
                frame.extend_from_slice(&buf[..n]);
                send.write_all(&frame).await?;
            }
            // QUIC stream → 本地 UDP（解帧头）
            result = recv.read_exact(&mut len_buf) => {
                result?;
                let n = u16::from_le_bytes(len_buf) as usize;
                if n > MAX_UDP_PAYLOAD {
                    anyhow::bail!("UDP frame too large: {n}");
                }
                recv.read_exact(&mut buf[..n]).await?;
                udp.send(&buf[..n]).await?;
            }
        }
    }
}

/// TCP 路径：服务端发来 WorkConnRequest（UDP 类型）后，客户端执行：
///   1. 调 OpenWorkConn RPC（首帧 stream_id）
///   2. 用 TonicStreamIo 桥接 tonic 双向流
///   3. 绑定临时本地 UDP socket，连接到 local_addr
///   4. 双向转发：WorkConn stream ↔ 本地 UDP socket（帧格式与 QUIC 路径一致）
pub async fn handle_work_conn_tcp(
    mut grpc: ControlServiceClient<tonic::transport::Channel>,
    session_id: String,
    proxy_name: String,
    local_addr: String,
    stream_id: String,
) -> Result<()> {
    // 构造输入流：首帧 stream_id
    let (in_tx, in_rx) = tokio::sync::mpsc::channel::<WorkConnFrame>(64);
    in_tx
        .send(WorkConnFrame {
            payload: Some(WcfPayload::StreamId(stream_id.clone())),
        })
        .await
        .map_err(|_| anyhow::anyhow!("in_tx closed before stream_id"))?;

    let mut req = Request::new(tokio_stream::wrappers::ReceiverStream::new(in_rx));
    req.metadata_mut()
        .insert("session-id", MetadataValue::try_from(&session_id).unwrap());

    let resp = grpc
        .open_work_conn(req)
        .await
        .map_err(|e| anyhow::anyhow!("open_work_conn: {e}"))?;
    let mut resp_stream = resp.into_inner();

    // 桥���：远端 frame → in_byte_tx，本地字节 → out_byte_tx
    let (out_byte_tx, mut out_byte_rx) = tokio::sync::mpsc::channel::<bytes::Bytes>(64);
    let (in_byte_tx, in_byte_rx) = tokio::sync::mpsc::channel::<bytes::Bytes>(64);

    // reader task：从 resp_stream 读 frame，data 字段塞到 in_byte_tx
    tokio::spawn(async move {
        while let Ok(Some(frame)) = resp_stream.message().await {
            match frame.payload {
                Some(WcfPayload::Data(b)) => {
                    if in_byte_tx.send(bytes::Bytes::from(b)).await.is_err() {
                        break;
                    }
                }
                _ => break,
            }
        }
    });

    // writer task：从 out_byte_rx 取字节，打包成 WorkConnFrame{data} 通过 in_tx 发送
    let in_tx_writer = in_tx.clone();
    tokio::spawn(async move {
        while let Some(b) = out_byte_rx.recv().await {
            let frame = WorkConnFrame {
                payload: Some(WcfPayload::Data(b.to_vec())),
            };
            if in_tx_writer.send(frame).await.is_err() {
                break;
            }
        }
    });

    let mut work_io: Box<dyn WorkIo> = Box::new(TonicStreamIo::new(in_byte_rx, out_byte_tx));

    debug!(proxy = %proxy_name, stream_id, "TCP UDP work stream opened, connecting {local_addr}");

    // 绑定本地临时 UDP socket 并 connect 到目标地址
    let udp = UdpSocket::bind("0.0.0.0:0").await?;
    udp.connect(&local_addr).await?;

    // 双向转发（帧格式与 QUIC 路径一致）
    let mut buf = vec![0u8; MAX_UDP_PAYLOAD];
    let mut len_buf = [0u8; 2];

    loop {
        tokio::select! {
            // 本地 UDP → WorkConn（加帧头）
            result = udp.recv(&mut buf) => {
                let n = result?;
                let len = n as u16;
                let mut frame = Vec::with_capacity(2 + n);
                frame.extend_from_slice(&len.to_le_bytes());
                frame.extend_from_slice(&buf[..n]);
                work_io.write_all(&frame).await?;
            }
            // WorkConn → 本地 UDP（解帧头）
            result = work_io.read_exact(&mut len_buf) => {
                result?;
                let n = u16::from_le_bytes(len_buf) as usize;
                if n > MAX_UDP_PAYLOAD {
                    anyhow::bail!("UDP frame too large: {n}");
                }
                work_io.read_exact(&mut buf[..n]).await?;
                udp.send(&buf[..n]).await?;
            }
        }
    }
}
