use anyhow::{Context, Result};
use quinn::ServerConfig as QuinnServerConfig;
use tunx_common::config::{ServerConfig, ServerTlsConfig};
use rustls::ServerConfig as RustlsServerConfig;
use std::sync::Arc;
use tracing::info;

/// 服务端 TLS 资源：QUIC 配置 + 给 HTTPS proxy 用的 cert/key
pub struct ServerTls {
    pub quinn_cfg: QuinnServerConfig,
    /// 证书 DER（含完整链）
    pub cert_chain_der: Vec<Vec<u8>>,
    /// 私钥 DER
    pub key_der: Vec<u8>,
    /// 证书覆盖的域名（SAN + CN），全部小写
    pub cert_domains: Vec<String>,
    /// 是否为正规 CA 签发（acme / manual）；self_signed 为 false
    pub public_trusted: bool,
}

pub async fn build_server_tls(cfg: &ServerConfig) -> Result<ServerTls> {
    let quic = &cfg.quic;
    match &cfg.tls {
        ServerTlsConfig::SelfSigned { sni } => {
            info!("TLS: generating self-signed certificate for '{sni}'");
            let cert = tunx_common::quic::generate_self_signed(&[sni.as_str()])?;
            let cert_chain_der = vec![cert.cert_der.clone()];
            let cert_domains = extract_cert_domains(&cert.cert_der);
            let quinn_cfg = tunx_common::quic::server_config_from_der(
                cert.cert_der,
                cert.key_der.clone(),
                quic,
            )?;
            Ok(ServerTls {
                quinn_cfg,
                cert_chain_der,
                key_der: cert.key_der,
                cert_domains,
                public_trusted: false,
            })
        }

        ServerTlsConfig::Manual {
            cert_file,
            key_file,
        } => {
            info!("TLS: loading certificate from {:?}", cert_file);
            let (cert_chain_der, key_der) = read_pem_cert_key(cert_file, key_file)?;
            let mut cert_domains = Vec::new();
            for c in &cert_chain_der {
                cert_domains.extend(extract_cert_domains(c));
            }
            let quinn_cfg = build_quinn_from_der(&cert_chain_der, &key_der, quic)?;
            Ok(ServerTls {
                quinn_cfg,
                cert_chain_der,
                key_der,
                cert_domains,
                public_trusted: true,
            })
        }

        ServerTlsConfig::Acme {
            domain,
            email,
            cf_api_token,
            cache_dir,
            staging,
        } => {
            info!("TLS: requesting Let's Encrypt certificate for {domain} via Cloudflare DNS-01");
            let (cert_der, key_der) =
                crate::server::acme::obtain_certificate(domain, email, cache_dir, *staging, cf_api_token)
                    .await?;
            let cert_chain_der = vec![cert_der.clone()];
            let cert_domains = extract_cert_domains(&cert_der);
            let quinn_cfg = build_quinn_from_der(&cert_chain_der, &key_der, quic)?;
            Ok(ServerTls {
                quinn_cfg,
                cert_chain_der,
                key_der,
                cert_domains,
                public_trusted: true,
            })
        }
    }
}

/// 构建用于 TCP proxy 的 tokio-rustls TlsAcceptor
/// 与 QUIC 不同：不设置 ALPN，浏览器可正常握手
pub fn build_tls_acceptor(tls: &ServerTls) -> Result<Arc<tokio_rustls::TlsAcceptor>> {
    let certs: Vec<rustls::Certificate> = tls
        .cert_chain_der
        .iter()
        .map(|d| rustls::Certificate(d.clone()))
        .collect();
    let key = rustls::PrivateKey(tls.key_der.clone());

    let mut rc = RustlsServerConfig::builder()
        .with_safe_defaults()
        .with_no_client_auth()
        .with_single_cert(certs, key)
        .context("build rustls ServerConfig for TCP acceptor")?;
    // 浏览器访问 HTTPS 时 ALPN 通常不强校验，留空即可
    rc.alpn_protocols = Vec::new();

    Ok(Arc::new(tokio_rustls::TlsAcceptor::from(Arc::new(rc))))
}

// ─── helpers ──────────────────────────────────────────────────────────────────

fn read_pem_cert_key(
    cert_path: &std::path::Path,
    key_path: &std::path::Path,
) -> Result<(Vec<Vec<u8>>, Vec<u8>)> {
    let cert_pem = std::fs::read(cert_path)
        .with_context(|| format!("read cert {:?}", cert_path))?;
    let key_pem = std::fs::read(key_path).with_context(|| format!("read key {:?}", key_path))?;

    let certs: Vec<Vec<u8>> = rustls_pemfile::certs(&mut cert_pem.as_slice())
        .context("parse cert PEM")?
        .into_iter()
        .collect();
    anyhow::ensure!(!certs.is_empty(), "no certificate found in {:?}", cert_path);

    // 同时支持 PKCS#8（BEGIN PRIVATE KEY）和 PKCS#1（BEGIN RSA PRIVATE KEY）
    let key = {
        let pkcs8 =
            rustls_pemfile::pkcs8_private_keys(&mut key_pem.as_slice()).context("parse key PEM")?;
        if let Some(k) = pkcs8.into_iter().next() {
            k
        } else {
            let pkcs1 = rustls_pemfile::rsa_private_keys(&mut key_pem.as_slice())
                .context("parse key PEM (RSA)")?;
            pkcs1
                .into_iter()
                .next()
                .context("no private key found (tried PKCS#8 and PKCS#1)")?
        }
    };

    Ok((certs, key))
}

fn build_quinn_from_der(
    cert_chain_der: &[Vec<u8>],
    key_der: &[u8],
    quic: &tunx_common::config::QuicConfig,
) -> Result<QuinnServerConfig> {
    let certs: Vec<rustls::Certificate> = cert_chain_der
        .iter()
        .map(|d| rustls::Certificate(d.clone()))
        .collect();
    let key = rustls::PrivateKey(key_der.to_vec());

    let mut tls = rustls::ServerConfig::builder()
        .with_safe_defaults()
        .with_no_client_auth()
        .with_single_cert(certs, key)
        .context("build rustls ServerConfig")?;
    tls.alpn_protocols = vec![tunx_common::quic::ALPN_TUNX.to_vec()];

    let transport = tunx_common::quic::build_transport(quic);
    let mut cfg = QuinnServerConfig::with_crypto(Arc::new(tls));
    cfg.transport_config(Arc::new(transport));
    Ok(cfg)
}

/// 从 DER 证书中提取所有 SAN + CN，全部转小写
fn extract_cert_domains(cert_der: &[u8]) -> Vec<String> {
    use x509_parser::prelude::*;
    let mut domains = Vec::new();
    let Ok((_, parsed)) = X509Certificate::from_der(cert_der) else {
        return domains;
    };

    // CN
    if let Some(cn) = parsed.subject().iter_common_name().next() {
        if let Ok(s) = cn.attr_value().as_str() {
            let s = s.trim().to_lowercase();
            if !s.is_empty() {
                domains.push(s);
            }
        }
    }

    // SAN
    if let Ok(Some(san_ext)) = parsed.subject_alternative_name() {
        for name in &san_ext.value.general_names {
            if let x509_parser::extensions::GeneralName::DNSName(s) = name {
                domains.push(s.to_lowercase());
            }
        }
    }

    domains
}
