//! ACME v2 DNS-01 实现（Cloudflare）
//!
//! 流程：
//!   1. 从 Cloudflare API 查出域名对应的 Zone ID
//!   2. 在该 Zone 下创建 _acme-challenge.<domain> TXT 记录
//!   3. 等待 DNS 传播后通知 LE 验证
//!   4. 验证通过后删除 TXT 记录，下载并缓存证书

use anyhow::{bail, Context, Result};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use hyper::{Body, Client, Method, Request as HyperRequest, Uri};
use rcgen::{Certificate, CertificateParams, DistinguishedName};
use ring::{
    rand::SystemRandom,
    signature::{EcdsaKeyPair, KeyPair, ECDSA_P256_SHA256_FIXED_SIGNING},
};
use serde_json::{json, Value};
use std::path::PathBuf;
use tracing::{info, warn};

const LE_PROD: &str = "https://acme-v02.api.letsencrypt.org/directory";
const LE_STAGING: &str = "https://acme-staging-v02.api.letsencrypt.org/directory";
const CF_API: &str = "https://api.cloudflare.com/client/v4";

pub async fn obtain_certificate(
    domain: &str,
    email: &str,
    cache_dir: &PathBuf,
    staging: bool,
    cf_api_token: &str,
) -> Result<(Vec<u8>, Vec<u8>)> {
    tokio::fs::create_dir_all(cache_dir).await?;
    let cert_path = cache_dir.join("cert.der");
    let key_path = cache_dir.join("key.der");

    // 检查缓存
    if cert_path.exists() && key_path.exists() {
        info!("ACME: using cached certificate");
        return Ok((
            tokio::fs::read(&cert_path).await?,
            tokio::fs::read(&key_path).await?,
        ));
    }

    let https = hyper_rustls::HttpsConnectorBuilder::new()
        .with_native_roots()
        .https_or_http()
        .enable_http1()
        .build();
    let http_client: Client<_, Body> = Client::builder().build(https);

    let acme = AcmeClient::new(http_client.clone())?;
    let cf = CloudflareClient::new(http_client, cf_api_token.to_string());

    let dir_url = if staging { LE_STAGING } else { LE_PROD };
    info!("ACME: requesting certificate for {domain} (DNS-01 via Cloudflare)");

    // ── 1. ACME directory ────────────────────────────────────────────────────
    let dir: Value = acme.get_json(dir_url).await?;
    let new_nonce_url = str_field(&dir, "newNonce")?;
    let new_account_url = str_field(&dir, "newAccount")?;
    let new_order_url = str_field(&dir, "newOrder")?;

    // ── 2. 获取 nonce ────────────────────────────────────────────────────────
    let nonce = acme.head_nonce(new_nonce_url).await?;

    // ── 3. 注册账号 ──────────────────────────────────────────────────────────
    let payload = json!({
        "termsOfServiceAgreed": true,
        "contact": [format!("mailto:{email}")],
    });
    let (_body, headers, nonce) = acme
        .jws_post(new_account_url, &payload, JwsKid::Jwk, nonce)
        .await?;
    let account_url = headers
        .get("location")
        .and_then(|v| v.to_str().ok())
        .context("account location header")?
        .to_string();
    info!("ACME: account registered: {account_url}");

    // ── 4. 新建订单 ──────────────────────────────────────────────────────────
    let payload = json!({
        "identifiers": [{"type": "dns", "value": domain}]
    });
    let (body, headers, nonce) = acme
        .jws_post(new_order_url, &payload, JwsKid::Kid(&account_url), nonce)
        .await?;
    let order_url = headers
        .get("location")
        .and_then(|v| v.to_str().ok())
        .context("order location")?
        .to_string();
    let order: Value = serde_json::from_str(&body)?;

    // ── 5. 获取 DNS-01 challenge ─────────────────────────────────────────────
    let auth_url = order["authorizations"][0].as_str().context("auth url")?;
    let (body, _, nonce) = acme
        .jws_post(auth_url, &Value::Null, JwsKid::Kid(&account_url), nonce)
        .await?;
    let auth: Value = serde_json::from_str(&body)?;

    let challenge = auth["challenges"]
        .as_array()
        .context("challenges")?
        .iter()
        .find(|c| c["type"] == "dns-01")
        .context("no dns-01 challenge")?;
    let token = challenge["token"].as_str().context("token")?.to_string();
    let challenge_url = challenge["url"]
        .as_str()
        .context("challenge url")?
        .to_string();

    // DNS-01 key authorization = base64url(SHA256(token + "." + thumbprint))
    let key_auth = format!("{}.{}", token, acme.thumbprint());
    let dns_value = {
        use ring::digest;
        let hash = digest::digest(&digest::SHA256, key_auth.as_bytes());
        URL_SAFE_NO_PAD.encode(hash.as_ref())
    };
    let txt_name = format!("_acme-challenge.{domain}");

    // ── 6. 查找 Cloudflare Zone ID ───────────────────────────────────────────
    let zone_id = cf.find_zone_id(domain).await?;
    info!("ACME: Cloudflare zone_id={zone_id}");

    // ── 7. 创建 TXT 记录 ─────────────────────────────────────────────────────
    let record_id = cf
        .create_txt_record(&zone_id, &txt_name, &dns_value)
        .await?;
    info!("ACME: created TXT record {txt_name} = {dns_value}");

    // ── 8. 等待 DNS 传播 ─────────────────────────────────────────────────────
    info!("ACME: waiting for DNS propagation (15s)...");
    tokio::time::sleep(std::time::Duration::from_secs(15)).await;

    // ── 9. 触发验证 ──────────────────────────────────────────────────────────
    let (_, _, mut nonce) = acme
        .jws_post(&challenge_url, &json!({}), JwsKid::Kid(&account_url), nonce)
        .await?;

    // ── 10. 轮询订单状态 → ready ─────────────────────────────────────────────
    info!("ACME: waiting for domain validation...");
    let finalize_url;
    loop {
        tokio::time::sleep(std::time::Duration::from_secs(3)).await;
        let (body, _, n) = acme
            .jws_post(&order_url, &Value::Null, JwsKid::Kid(&account_url), nonce)
            .await?;
        nonce = n;
        let o: Value = serde_json::from_str(&body)?;
        match o["status"].as_str() {
            Some("ready") | Some("valid") => {
                finalize_url = o["finalize"].as_str().context("finalize url")?.to_string();
                break;
            }
            Some("invalid") => {
                // 验证失败���要清理 TXT 记录
                let _ = cf.delete_txt_record(&zone_id, &record_id).await;
                bail!("ACME order invalid — check domain DNS and Cloudflare API token permissions");
            }
            s => info!("ACME: order status = {s:?}"),
        }
    }

    // ── 11. 删除 TXT 记录 ────────────────────────────────────────────────────
    match cf.delete_txt_record(&zone_id, &record_id).await {
        Ok(_) => info!("ACME: deleted TXT record"),
        Err(e) => warn!("ACME: failed to delete TXT record: {e}"),
    }

    // ── 12. 生成 CSR ─────────────────────────────────────────────────────────
    let mut params = CertificateParams::new(vec![domain.to_string()]);
    params.distinguished_name = DistinguishedName::new();
    let cert_key = Certificate::from_params(params)?;
    let csr_der = cert_key.serialize_request_der()?;
    let csr_b64 = URL_SAFE_NO_PAD.encode(&csr_der);

    // ── 13. Finalize ─────────────────────────────────────────────────────────
    let (_, _, mut nonce) = acme
        .jws_post(
            &finalize_url,
            &json!({"csr": csr_b64}),
            JwsKid::Kid(&account_url),
            nonce,
        )
        .await?;

    // ── 14. 等待证书就绪 ─────────────────────────────────────────────────────
    let cert_url;
    loop {
        tokio::time::sleep(std::time::Duration::from_secs(2)).await;
        let (body, _, n) = acme
            .jws_post(&order_url, &Value::Null, JwsKid::Kid(&account_url), nonce)
            .await?;
        nonce = n;
        let o: Value = serde_json::from_str(&body)?;
        if o["status"] == "valid" {
            cert_url = o["certificate"]
                .as_str()
                .context("certificate url")?
                .to_string();
            break;
        }
    }

    // ── 15. 下载并缓存证书 ───────────────────────────────────────────────────
    let (pem, _, _) = acme
        .jws_post(&cert_url, &Value::Null, JwsKid::Kid(&account_url), nonce)
        .await?;

    let cert_der = rustls_pemfile::certs(&mut pem.as_bytes())
        .context("parse cert PEM")?
        .into_iter()
        .next()
        .context("empty cert chain")?;
    let key_der = cert_key.serialize_private_key_der();

    tokio::fs::write(&cert_path, &cert_der).await?;
    tokio::fs::write(&key_path, &key_der).await?;
    info!("ACME: certificate cached in {}", cache_dir.display());

    Ok((cert_der, key_der))
}

// ─── Cloudflare 客户端 ────────────────────────────────────────────────────────

struct CloudflareClient {
    http: Client<hyper_rustls::HttpsConnector<hyper::client::HttpConnector>, Body>,
    token: String,
}

impl CloudflareClient {
    fn new(
        http: Client<hyper_rustls::HttpsConnector<hyper::client::HttpConnector>, Body>,
        token: String,
    ) -> Self {
        Self { http, token }
    }

    /// 根据域名找到对应的 Zone ID
    /// 支持子域名：tunx.example.com → 查 example.com 的 zone
    async fn find_zone_id(&self, domain: &str) -> Result<String> {
        // 从完整域名里逐级提取根域名尝试查询
        // 例：a.b.example.com → 尝试 example.com、b.example.com
        let parts: Vec<&str> = domain.split('.').collect();
        for i in (1..parts.len()).rev() {
            let candidate = parts[i..].join(".");
            let url = format!("{CF_API}/zones?name={candidate}&status=active");
            let resp = self.cf_get(&url).await?;
            let zones = resp["result"].as_array().context("zones result")?;
            if let Some(zone) = zones.first() {
                let id = zone["id"].as_str().context("zone id")?.to_string();
                return Ok(id);
            }
        }
        bail!(
            "Cloudflare: no active zone found for domain '{domain}'. \
               Check that the domain is managed by this Cloudflare account."
        );
    }

    /// 创建 TXT 记录，返回记录 ID
    async fn create_txt_record(&self, zone_id: &str, name: &str, value: &str) -> Result<String> {
        let url = format!("{CF_API}/zones/{zone_id}/dns_records");
        let body = json!({
            "type": "TXT",
            "name": name,
            "content": value,
            "ttl": 60,   // 最短 TTL，验证完马上删
        });
        let resp = self.cf_post(&url, &body).await?;
        let id = resp["result"]["id"]
            .as_str()
            .context("dns record id")?
            .to_string();
        Ok(id)
    }

    /// 删除 TXT 记录
    async fn delete_txt_record(&self, zone_id: &str, record_id: &str) -> Result<()> {
        let url = format!("{CF_API}/zones/{zone_id}/dns_records/{record_id}");
        let req = HyperRequest::builder()
            .method(Method::DELETE)
            .uri(url.parse::<Uri>()?)
            .header("Authorization", format!("Bearer {}", self.token))
            .header("Content-Type", "application/json")
            .body(Body::empty())?;
        let resp = self.http.request(req).await?;
        let body = hyper::body::to_bytes(resp.into_body()).await?;
        let v: Value = serde_json::from_slice(&body)?;
        if v["success"] != true {
            bail!("Cloudflare delete record failed: {v}");
        }
        Ok(())
    }

    async fn cf_get(&self, url: &str) -> Result<Value> {
        let req = HyperRequest::builder()
            .method(Method::GET)
            .uri(url.parse::<Uri>()?)
            .header("Authorization", format!("Bearer {}", self.token))
            .header("Content-Type", "application/json")
            .body(Body::empty())?;
        let resp = self.http.request(req).await?;
        let body = hyper::body::to_bytes(resp.into_body()).await?;
        let v: Value = serde_json::from_slice(&body)?;
        if v["success"] != true {
            bail!("Cloudflare API error: {v}");
        }
        Ok(v)
    }

    async fn cf_post(&self, url: &str, payload: &Value) -> Result<Value> {
        let req = HyperRequest::builder()
            .method(Method::POST)
            .uri(url.parse::<Uri>()?)
            .header("Authorization", format!("Bearer {}", self.token))
            .header("Content-Type", "application/json")
            .body(Body::from(serde_json::to_string(payload)?))?;
        let resp = self.http.request(req).await?;
        let body = hyper::body::to_bytes(resp.into_body()).await?;
        let v: Value = serde_json::from_slice(&body)?;
        if v["success"] != true {
            bail!("Cloudflare API error: {v}");
        }
        Ok(v)
    }
}

// ─── ACME 客户端（与之前相同） ────────────────────────────────────────────────

type HyperClient = Client<hyper_rustls::HttpsConnector<hyper::client::HttpConnector>, Body>;

struct AcmeClient {
    http: HyperClient,
    key_pair: EcdsaKeyPair,
    rng: SystemRandom,
}

enum JwsKid<'a> {
    Jwk,
    Kid(&'a str),
}

impl AcmeClient {
    fn new(http: HyperClient) -> Result<Self> {
        let rng = SystemRandom::new();
        let pkcs8 = EcdsaKeyPair::generate_pkcs8(&ECDSA_P256_SHA256_FIXED_SIGNING, &rng)
            .map_err(|e| anyhow::anyhow!("keygen: {e:?}"))?;
        let key_pair =
            EcdsaKeyPair::from_pkcs8(&ECDSA_P256_SHA256_FIXED_SIGNING, pkcs8.as_ref(), &rng)
                .map_err(|e| anyhow::anyhow!("keypair: {e:?}"))?;
        Ok(Self {
            http,
            key_pair,
            rng,
        })
    }

    async fn get_json(&self, url: &str) -> Result<Value> {
        let resp = self.http.get(url.parse::<Uri>()?).await?;
        let body = hyper::body::to_bytes(resp.into_body()).await?;
        Ok(serde_json::from_slice(&body)?)
    }

    async fn head_nonce(&self, url: &str) -> Result<String> {
        let req = HyperRequest::builder()
            .method(Method::HEAD)
            .uri(url.parse::<Uri>()?)
            .body(Body::empty())?;
        let resp = self.http.request(req).await?;
        resp.headers()
            .get("replay-nonce")
            .and_then(|v| v.to_str().ok())
            .map(|s| s.to_string())
            .context("replay-nonce missing")
    }

    fn jwk_value(&self) -> Value {
        let pub_key = self.key_pair.public_key().as_ref();
        let x = URL_SAFE_NO_PAD.encode(&pub_key[1..33]);
        let y = URL_SAFE_NO_PAD.encode(&pub_key[33..65]);
        json!({"crv":"P-256","kty":"EC","x":x,"y":y})
    }

    fn thumbprint(&self) -> String {
        let pub_key = self.key_pair.public_key().as_ref();
        let x = URL_SAFE_NO_PAD.encode(&pub_key[1..33]);
        let y = URL_SAFE_NO_PAD.encode(&pub_key[33..65]);
        let canonical = format!(r#"{{"crv":"P-256","kty":"EC","x":"{x}","y":"{y}"}}"#);
        use ring::digest;
        let hash = digest::digest(&digest::SHA256, canonical.as_bytes());
        URL_SAFE_NO_PAD.encode(hash.as_ref())
    }

    async fn jws_post<'a>(
        &self,
        url: &str,
        payload: &Value,
        kid: JwsKid<'a>,
        nonce: String,
    ) -> Result<(String, hyper::HeaderMap, String)> {
        let payload_b64 = if matches!(payload, Value::Null) {
            "".to_string()
        } else {
            URL_SAFE_NO_PAD.encode(serde_json::to_string(payload)?.as_bytes())
        };

        let protected = match kid {
            JwsKid::Jwk => json!({
                "alg": "ES256",
                "jwk": self.jwk_value(),
                "nonce": nonce,
                "url": url,
            }),
            JwsKid::Kid(k) => json!({
                "alg": "ES256",
                "kid": k,
                "nonce": nonce,
                "url": url,
            }),
        };
        let protected_b64 = URL_SAFE_NO_PAD.encode(serde_json::to_string(&protected)?.as_bytes());
        let signing_input = format!("{protected_b64}.{payload_b64}");

        let sig = self
            .key_pair
            .sign(&self.rng, signing_input.as_bytes())
            .map_err(|e| anyhow::anyhow!("sign: {e:?}"))?;
        let sig_b64 = URL_SAFE_NO_PAD.encode(sig.as_ref());

        let body_json = serde_json::to_string(&json!({
            "protected": protected_b64,
            "payload":   payload_b64,
            "signature": sig_b64,
        }))?;

        let req = HyperRequest::builder()
            .method(Method::POST)
            .uri(url.parse::<Uri>()?)
            .header("Content-Type", "application/jose+json")
            .body(Body::from(body_json))?;

        let resp = self.http.request(req).await?;
        let status = resp.status();
        let headers = resp.headers().clone();
        let next_nonce = headers
            .get("replay-nonce")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_string();
        let body_bytes = hyper::body::to_bytes(resp.into_body()).await?;
        let body_str = String::from_utf8_lossy(&body_bytes).to_string();

        if !status.is_success() && status.as_u16() != 201 {
            bail!("ACME {url} → HTTP {status}: {body_str}");
        }
        Ok((body_str, headers, next_nonce))
    }
}

fn str_field<'a>(v: &'a Value, key: &str) -> Result<&'a str> {
    v[key]
        .as_str()
        .with_context(|| format!("missing field: {key}"))
}

// ─── 证书续签 ─────────────────────────────────────────────────────────────────

/// 读取缓存的 DER 证���，返回距离过期还有多少天
/// 读取失败或解析失败返回 0（触发立即续签）
pub fn cert_expires_in_days(cache_dir: &std::path::Path) -> i64 {
    let cert_path = cache_dir.join("cert.der");
    let der = match std::fs::read(cert_path) {
        Ok(d) => d,
        Err(_) => return 0,
    };

    // 用 x509-parser 解析过期时间
    use x509_parser::prelude::*;
    let (_, parsed) = match X509Certificate::from_der(&der) {
        Ok(r) => r,
        Err(_) => return 0,
    };

    let not_after = parsed.validity().not_after.timestamp();
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64;

    (not_after - now) / 86400
}

/// 启动后台续签任务
/// 每天检查一次证书剩余有效期，不足 30 天时重新申请
/// 续签成功后以 exit(0) 退出，由 systemd/supervisor 自动重启加载新证书
pub fn spawn_renewal_task(
    domain: String,
    email: String,
    cache_dir: std::path::PathBuf,
    staging: bool,
    cf_api_token: String,
) {
    tokio::spawn(async move {
        // 启动时先检查一次，之后每 24 小时检查一次
        let mut interval = tokio::time::interval(std::time::Duration::from_secs(86400));
        loop {
            interval.tick().await;

            let days = cert_expires_in_days(&cache_dir);
            if days > 30 {
                info!("ACME: certificate valid for {days} days, no renewal needed");
                continue;
            }

            info!("ACME: certificate expires in {days} days, starting renewal...");

            // 删除旧缓存，强制重新申请
            let cert_path = cache_dir.join("cert.der");
            let key_path = cache_dir.join("key.der");
            let _ = tokio::fs::remove_file(&cert_path).await;
            let _ = tokio::fs::remove_file(&key_path).await;

            match obtain_certificate(&domain, &email, &cache_dir, staging, &cf_api_token).await {
                Ok(_) => {
                    info!("ACME: certificate renewed successfully, restarting to load new cert...");
                    // 短暂等待日志落盘
                    tokio::time::sleep(std::time::Duration::from_secs(1)).await;
                    std::process::exit(0);
                }
                Err(e) => {
                    warn!("ACME: renewal failed: {e:#}, will retry in 24h");
                    // 续签失败恢复旧缓存（已删除，下次还会重试）
                }
            }
        }
    });
}
