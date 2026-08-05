use anyhow::Result;
use quinn::Connection;
use tonic::metadata::MetadataValue;
use tokio_stream::wrappers::ReceiverStream;
use tonic::Request;
use tracing::debug;

use tunx_common::quic::{STREAM_ID_LEN, WORK_CONN_MAGIC};
use tunx_common::stream::{TonicStreamIo, WorkIo};
use tunx_proto::{
    control_service_client::ControlServiceClient, WorkConnFrame,
    work_conn_frame::Payload as WcfPayload,
};

/// QUIC 路径：服务端发来 WorkConnRequest 后，客户端执行：
///   1. 在同一 QUIC connection 上 open_bi
///   2. 写 magic(4) + stream_id(36) header
///   3. 连接本地服务
///   4. 双向转发
pub async fn handle_work_conn(
    conn: Connection,
    proxy_name: String,
    local_addr: String,
    stream_id: String,
) -> Result<()> {
    // 开新 QUIC 双向 stream
    let (mut send, recv) = conn
        .open_bi()
        .await
        .map_err(|e| anyhow::anyhow!("open_bi: {e}"))?;

    // 写 header
    if stream_id.len() != STREAM_ID_LEN {
        anyhow::bail!("stream_id len {} != {STREAM_ID_LEN}", stream_id.len());
    }
    let mut header = [0u8; 4 + STREAM_ID_LEN];
    header[..4].copy_from_slice(WORK_CONN_MAGIC);
    header[4..].copy_from_slice(stream_id.as_bytes());
    send.write_all(&header).await?;

    debug!(proxy = %proxy_name, stream_id, "work stream opened, connecting {local_addr}");

    // 连接本地服务
    let mut local = tokio::net::TcpStream::connect(&local_addr)
        .await
        .map_err(|e| anyhow::anyhow!("connect {local_addr}: {e}"))?;

    // 将 QUIC stream 合并为单个双向 IO
    let mut quic_io = tokio::io::join(recv, send);

    tokio::io::copy_bidirectional(&mut local, &mut quic_io).await?;

    debug!(proxy = %proxy_name, "work conn closed");
    Ok(())
}

/// TCP 路径：服务端发来 WorkConnRequest 后，客户端执行：
///   1. 调 OpenWorkConn RPC（首帧带 stream_id）
///   2. 用 TonicStreamIo 桥接 tonic 双向流
///   3. 连接本地服务
///   4. 双向转发
pub async fn handle_work_conn_tcp(
    mut grpc: ControlServiceClient<tonic::transport::Channel>,
    session_id: String,
    proxy_name: String,
    local_addr: String,
    stream_id: String,
) -> Result<()> {
    // 构造输入流：首帧 stream_id，之后随写随发
    let (in_tx, in_rx) = tokio::sync::mpsc::channel::<WorkConnFrame>(64);

    // 先发首帧 stream_id
    in_tx
        .send(WorkConnFrame {
            payload: Some(WcfPayload::StreamId(stream_id.clone())),
        })
        .await
        .map_err(|_| anyhow::anyhow!("in_tx closed before stream_id"))?;

    // 写本地字节 → in_tx（打包成 Data frame）
    let in_tx_clone = in_tx.clone();
    tokio::spawn(async move {
        // in_tx_clone 在 writer 关闭时 drop
        let _ = in_tx_clone;
    });

    let mut req = Request::new(ReceiverStream::new(in_rx));
    req.metadata_mut()
        .insert("session-id", MetadataValue::try_from(&session_id).unwrap());

    let resp = grpc
        .open_work_conn(req)
        .await
        .map_err(|e| anyhow::anyhow!("open_work_conn: {e}"))?;
    let mut resp_stream = resp.into_inner();

    // 拿首字节之前必须先建立桥接器：in_rx2（远端 → 本地）+ out_tx（本地 → 远端）
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

    // writer task：从 out_byte_rx 取字节，���装成 WorkConnFrame{data} 通过 in_tx 发送
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

    // 构造 TonicStreamIo
    let mut work_io: Box<dyn WorkIo> = Box::new(TonicStreamIo::new(in_byte_rx, out_byte_tx));

    debug!(proxy = %proxy_name, stream_id, "TCP work stream opened, connecting {local_addr}");

    // 连接本地服务
    let mut local = tokio::net::TcpStream::connect(&local_addr)
        .await
        .map_err(|e| anyhow::anyhow!("connect {local_addr}: {e}"))?;

    tokio::io::copy_bidirectional(&mut local, &mut work_io).await?;

    debug!(proxy = %proxy_name, "work conn closed");
    Ok(())
}
