//! Web UI 鉴权
//!
//! - 首次启动若配置里 `password_hash` 为空，生成随机密码并写回配置文件
//! - 登录成功后签发一个随机 token，保存在内存里（重启后失效，需要重新登录）
//! - 用 HttpOnly Cookie 承载 token，中间件校验

use std::collections::HashMap;
use std::sync::Arc;

use anyhow::Result;
use argon2::password_hash::rand_core::{OsRng, RngCore};
use argon2::password_hash::{PasswordHash, PasswordHasher, PasswordVerifier, SaltString};
use argon2::Argon2;
use axum::extract::State;
use axum::http::{header, Request, StatusCode};
use axum::middleware::Next;
use axum::response::Response;
use tokio::sync::RwLock;

use crate::runtime::AppRuntime;

pub const SESSION_COOKIE_NAME: &str = "tunx_session";
const TOKEN_BYTES: usize = 32;

/// 内存态的登录 token 集合：token → 签发时间（unix 秒）
/// 简单起见不做过期淘汰的后台任务，只在校验时惰性检查 TTL
#[derive(Default)]
pub struct SessionStore {
    tokens: RwLock<HashMap<String, i64>>,
}

const TOKEN_TTL_SECS: i64 = 7 * 24 * 3600; // 7 天

impl SessionStore {
    pub fn new() -> Self {
        Self::default()
    }

    pub async fn issue(&self) -> String {
        let token = generate_token();
        let now = unix_now();
        self.tokens.write().await.insert(token.clone(), now);
        token
    }

    pub async fn is_valid(&self, token: &str) -> bool {
        let now = unix_now();
        let map = self.tokens.read().await;
        match map.get(token) {
            Some(issued_at) => now - *issued_at < TOKEN_TTL_SECS,
            None => false,
        }
    }

    pub async fn revoke(&self, token: &str) {
        self.tokens.write().await.remove(token);
    }
}

fn unix_now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

fn generate_token() -> String {
    let mut bytes = [0u8; TOKEN_BYTES];
    OsRng.fill_bytes(&mut bytes);
    hex_encode(&bytes)
}

fn hex_encode(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// 生成一个人类可读的随机密码（首次启动用）：16 字节 → base32 风格，去掉易混淆字符
fn generate_random_password() -> String {
    const ALPHABET: &[u8] = b"ABCDEFGHJKLMNPQRSTUVWXYZ23456789"; // 去掉 0/O/1/I
    let mut bytes = [0u8; 20];
    OsRng.fill_bytes(&mut bytes);
    bytes
        .iter()
        .map(|b| ALPHABET[(*b as usize) % ALPHABET.len()] as char)
        .collect()
}

pub fn hash_password(plain: &str) -> Result<String> {
    let salt = SaltString::generate(&mut OsRng);
    let argon2 = Argon2::default();
    let hash = argon2
        .hash_password(plain.as_bytes(), &salt)
        .map_err(|e| anyhow::anyhow!("hash password: {e}"))?;
    Ok(hash.to_string())
}

pub fn verify_password(plain: &str, hash: &str) -> bool {
    let parsed = match PasswordHash::new(hash) {
        Ok(h) => h,
        Err(_) => return false,
    };
    Argon2::default()
        .verify_password(plain.as_bytes(), &parsed)
        .is_ok()
}

/// 首次启动：若配置里没有 password_hash，生成随机密码，写回配置文件，
/// 并把明文密码打印在日志里（仅这一次，之后无法从日志找回）
pub async fn ensure_web_credentials(app: &Arc<AppRuntime>) -> Result<()> {
    let mut cfg = app.snapshot_config().await;
    if !cfg.web.password_hash.is_empty() {
        return Ok(());
    }

    let plain = generate_random_password();
    let hash = hash_password(&plain)?;
    cfg.web.password_hash = hash;
    cfg.save_to_file(&app.config_path_str())?;
    {
        let mut guard = app.config.write().await;
        guard.web.password_hash = cfg.web.password_hash.clone();
    }

    tracing::warn!(
        "══════════════════════════════════════════════════════════════\n\
         首次启动，已生成 Web UI 登录密码（仅此一次打印，请妥善保存）:\n\
           用户名: {}\n\
           密　码: {}\n\
         配置文件: {}\n\
         ══════════════════════════════════════════════════════════════",
        cfg.web.username,
        plain,
        app.config_path_str()
    );

    Ok(())
}

// ─── axum 中间件：校验 Cookie 里的 token ────────────────────────────────────

pub async fn require_auth<B>(
    State(state): State<crate::web::WebState>,
    req: Request<B>,
    next: Next<B>,
) -> Result<Response, StatusCode>
where
    B: Send + 'static,
{
    let token = extract_token(&req);
    match token {
        Some(t) if state.sessions.is_valid(&t).await => Ok(next.run(req).await),
        _ => Err(StatusCode::UNAUTHORIZED),
    }
}

fn extract_token<B>(req: &Request<B>) -> Option<String> {
    let cookie_header = req.headers().get(header::COOKIE)?.to_str().ok()?;
    for part in cookie_header.split(';') {
        let part = part.trim();
        if let Some(v) = part.strip_prefix(&format!("{SESSION_COOKIE_NAME}=")) {
            return Some(v.to_string());
        }
    }
    None
}
