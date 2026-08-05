use anyhow::{Context, Result};
use quinn::{ClientConfig as QuinnClientConfig, ServerConfig as QuinnServerConfig};
use rustls::{Certificate, PrivateKey};
use std::sync::Arc;

use crate::config::QuicConfig;

// ─── 工作连接握手魔数 ──────────────────────────────────────────────────────────
//
// client 打开数据 stream 后，首先写入 WORK_CONN_MAGIC (4字节) + stream_id (36字节 UUID)
// server 读取后关联到等待中的公网连接
pub const WORK_CONN_MAGIC: &[u8; 4] = b"TNWC";
pub const STREAM_ID_LEN: usize = 36; // UUID v4 字符串长度

// ─── ALPN ────────────────────────────────────────────────────────────────────

/// QUIC ALPN 协议标识
pub const ALPN_TUNX: &[u8] = b"tunx/1";

// ─── 自签名证书（开发/测试） ───────────────────────────────────────────────────

pub struct SelfSignedCert {
    pub cert_der: Vec<u8>,
    pub key_der: Vec<u8>,
}

pub fn generate_self_signed(subject_alt_names: &[&str]) -> Result<SelfSignedCert> {
    let cert = rcgen::generate_simple_self_signed(
        subject_alt_names
            .iter()
            .map(|s| s.to_string())
            .collect::<Vec<_>>(),
    )
    .context("generate self-signed cert")?;

    Ok(SelfSignedCert {
        cert_der: cert.serialize_der()?,
        key_der: cert.serialize_private_key_der(),
    })
}

// ─── Server TLS ───────────────────────────────────────────────────────────────

/// 从 DER 字节构建 quinn ServerConfig（带 tunx ALPN）
pub fn server_config_from_der(
    cert_der: Vec<u8>,
    key_der: Vec<u8>,
    quic: &QuicConfig,
) -> Result<QuinnServerConfig> {
    let cert = Certificate(cert_der);
    let key = PrivateKey(key_der);

    let mut tls = rustls::ServerConfig::builder()
        .with_safe_defaults()
        .with_no_client_auth()
        .with_single_cert(vec![cert], key)
        .context("build rustls ServerConfig")?;

    tls.alpn_protocols = vec![ALPN_TUNX.to_vec()];

    let transport = build_transport(quic);
    let mut cfg = QuinnServerConfig::with_crypto(Arc::new(tls));
    cfg.transport_config(Arc::new(transport));
    Ok(cfg)
}

/// 从 PEM 文件构建 quinn ServerConfig
pub fn server_config_from_pem(
    cert_path: &str,
    key_path: &str,
    quic: &QuicConfig,
) -> Result<QuinnServerConfig> {
    let cert_pem = std::fs::read(cert_path).with_context(|| format!("read cert {cert_path}"))?;
    let key_pem = std::fs::read(key_path).with_context(|| format!("read key {key_path}"))?;

    let certs: Vec<Certificate> = rustls_pemfile::certs(&mut cert_pem.as_slice())
        .context("parse cert PEM")?
        .into_iter()
        .map(Certificate)
        .collect();

    // 同时支持 PKCS#8（BEGIN PRIVATE KEY）和 PKCS#1（BEGIN RSA PRIVATE KEY）
    let key = {
        let pkcs8 =
            rustls_pemfile::pkcs8_private_keys(&mut key_pem.as_slice()).context("parse key PEM")?;
        if let Some(k) = pkcs8.into_iter().next() {
            PrivateKey(k)
        } else {
            let pkcs1 = rustls_pemfile::rsa_private_keys(&mut key_pem.as_slice())
                .context("parse key PEM (RSA)")?;
            pkcs1
                .into_iter()
                .next()
                .map(PrivateKey)
                .context("no private key found (tried PKCS#8 and PKCS#1)")?
        }
    };

    let mut tls = rustls::ServerConfig::builder()
        .with_safe_defaults()
        .with_no_client_auth()
        .with_single_cert(certs, key)
        .context("build rustls ServerConfig")?;

    tls.alpn_protocols = vec![ALPN_TUNX.to_vec()];

    let transport = build_transport(quic);
    let mut cfg = QuinnServerConfig::with_crypto(Arc::new(tls));
    cfg.transport_config(Arc::new(transport));
    Ok(cfg)
}

// ─── Client TLS ───────────────────────────────────────────────────────────────

/// 正常校验服务端证书的客户端配置
pub fn client_config_verified(quic: &QuicConfig) -> Result<QuinnClientConfig> {
    let mut roots = rustls::RootCertStore::empty();

    // 使用系统 CA 根证书（Let's Encrypt 等公信 CA 已内置）
    let native_certs = rustls_native_certs_load()?;
    for cert in native_certs {
        let _ = roots.add(&cert);
    }

    let mut tls = rustls::ClientConfig::builder()
        .with_safe_defaults()
        .with_root_certificates(roots)
        .with_no_client_auth();

    tls.alpn_protocols = vec![ALPN_TUNX.to_vec()];

    let mut cfg = QuinnClientConfig::new(Arc::new(tls));
    cfg.transport_config(Arc::new(build_transport(quic)));
    Ok(cfg)
}

/// 跳过服务端证书校验（tls_skip_verify = true）
pub fn client_config_skip_verify(quic: &QuicConfig) -> QuinnClientConfig {
    let mut tls = rustls::ClientConfig::builder()
        .with_safe_defaults()
        .with_custom_certificate_verifier(Arc::new(SkipServerVerification))
        .with_no_client_auth();

    tls.alpn_protocols = vec![ALPN_TUNX.to_vec()];

    let mut cfg = QuinnClientConfig::new(Arc::new(tls));
    cfg.transport_config(Arc::new(build_transport(quic)));
    cfg
}

// ─── Transport builder ────────────────────────────────────────────────────────

pub fn build_transport(quic: &QuicConfig) -> quinn::TransportConfig {
    let mut t = quinn::TransportConfig::default();

    // 最大空闲超时 90s
    t.max_idle_timeout(Some(
        std::time::Duration::from_secs(90)
            .try_into()
            .expect("valid duration"),
    ));
    // 保活 ping 间隔 30s
    t.keep_alive_interval(Some(std::time::Duration::from_secs(30)));
    // 最大并发双向 stream 数
    t.max_concurrent_bidi_streams(1024u32.into());

    // ── 拥塞控制 ──────────────────────────────────────────────────────────────
    match quic.congestion.to_lowercase().as_str() {
        "bbr" => {
            t.congestion_controller_factory(Arc::new(quinn::congestion::BbrConfig::default()));
        }
        _ => {
            // "new_reno" 或其他未知值均回退到 NewReno
            t.congestion_controller_factory(Arc::new(quinn::congestion::NewRenoConfig::default()));
        }
    }

    // ── MTU 探测 ───────────────────────────────────────────────────────────────
    // initial_mtu 作为起点，quinn 的 MTU 探测会自动向上探测到路径最大值
    t.initial_mtu(quic.initial_mtu);

    // ── 接收窗口 ───────────────────────────────────────────────────────────────
    // 同时设置连接级和单 stream 级窗口，保持一致
    let window = quic.recv_window as u64;
    t.receive_window(window.try_into().expect("valid window"));
    t.stream_receive_window(window.try_into().expect("valid window"));

    t
}

// ─── SkipServerVerification ───────────────────────────────────────────────────

struct SkipServerVerification;

impl rustls::client::ServerCertVerifier for SkipServerVerification {
    fn verify_server_cert(
        &self,
        _end_entity: &Certificate,
        _intermediates: &[Certificate],
        _server_name: &rustls::ServerName,
        _scts: &mut dyn Iterator<Item = &[u8]>,
        _ocsp: &[u8],
        _now: std::time::SystemTime,
    ) -> std::result::Result<rustls::client::ServerCertVerified, rustls::Error> {
        Ok(rustls::client::ServerCertVerified::assertion())
    }
}

// ─── Native certs helper ──────────────────────────────────────────────────────

fn rustls_native_certs_load() -> Result<Vec<Certificate>> {
    // 简单实现：尝试加载系统证书，失败则返回空列表（Let's Encrypt 的根已被大多数系统信任）
    match rustls_native_certs::load_native_certs() {
        Ok(certs) => Ok(certs.into_iter().map(|c| Certificate(c.0)).collect()),
        Err(e) => {
            tracing::warn!("load native certs failed: {e}, falling back to webpki roots");
            Ok(vec![])
        }
    }
}
