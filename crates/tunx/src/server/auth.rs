//! 方案 A 认证模块
//!
//! session_id = base64url( random_bytes(16) )
//!            + "."
//!            + base64url( HMAC-SHA256(token, random_bytes) )
//!
//! 格式：<nonce_b64>.<hmac_b64>
//!
//! 服务端收到后可以不查表直接验证：
//!   HMAC-SHA256(token, nonce_b64) == hmac_b64
//!
//! 好处：
//!   - session_id 不可伪造（不知道 token 就无法构造合法的 HMAC）
//!   - 不需要额外的 session 存储校验字段
//!   - token 轮换后旧 session_id 自动失效

use anyhow::{bail, Result};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use ring::hmac;
use ring::rand::{SecureRandom, SystemRandom};

/// 生成带 HMAC 签名的 session_id
///
/// token 为空时退化为纯随机 ID（不启用 HMAC 保护）
pub fn generate_session_id(token: &str) -> String {
    let rng = SystemRandom::new();
    let mut nonce = [0u8; 16];
    rng.fill(&mut nonce).expect("rng fill");
    let nonce_b64 = URL_SAFE_NO_PAD.encode(nonce);

    if token.is_empty() {
        // 无 token 模式：纯随机 ID
        return nonce_b64;
    }

    let mac = hmac_sign(token, &nonce_b64);
    format!("{nonce_b64}.{mac}")
}

/// 验证 session_id 签名
///
/// - token 为空：始终通过（兼容无认证模式）
/// - token 非空：校验 HMAC，失败则拒绝
pub fn verify_session_id(session_id: &str, token: &str) -> Result<()> {
    if token.is_empty() {
        return Ok(());
    }

    let (nonce_b64, mac) = session_id
        .split_once('.')
        .ok_or_else(|| anyhow::anyhow!("malformed session_id: missing '.'"))?;

    let expected = hmac_sign(token, nonce_b64);

    // 恒定时间比较，防止时序攻击
    if !constant_time_eq(mac.as_bytes(), expected.as_bytes()) {
        bail!("session_id signature mismatch");
    }
    Ok(())
}

fn hmac_sign(token: &str, data: &str) -> String {
    let key = hmac::Key::new(hmac::HMAC_SHA256, token.as_bytes());
    let sig = hmac::sign(&key, data.as_bytes());
    URL_SAFE_NO_PAD.encode(sig.as_ref())
}

/// 恒定时间字节比较（防止时序泄漏）
fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

// ─── session TTL 检查 ─────────────────────────────────────────────────────────

use std::time::{Duration, Instant};

/// session 建立时间戳，用于 TTL 判断
#[allow(dead_code)]
pub struct SessionTimer {
    created_at: Instant,
    /// 必须在此时间内建立 ControlStream，否则视为超时
    control_stream_deadline: Instant,
}

impl SessionTimer {
    /// deadline_secs：Login 后多少秒内必须建立 ControlStream
    pub fn new(deadline_secs: u64) -> Self {
        let now = Instant::now();
        Self {
            created_at: now,
            control_stream_deadline: now + Duration::from_secs(deadline_secs),
        }
    }

    /// ControlStream 是否已超时
    pub fn is_expired(&self) -> bool {
        Instant::now() > self.control_stream_deadline
    }

    #[allow(dead_code)]
    pub fn age(&self) -> Duration {
        self.created_at.elapsed()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_generate_and_verify() {
        let token = "my-secret-token";
        let sid = generate_session_id(token);
        assert!(sid.contains('.'), "should have dot separator");
        assert!(verify_session_id(&sid, token).is_ok());
    }

    #[test]
    fn test_tampered_sid_rejected() {
        let token = "my-secret-token";
        let sid = generate_session_id(token);
        // 篡改最后一个字符
        let mut tampered = sid.clone();
        let last = tampered.pop().unwrap();
        tampered.push(if last == 'A' { 'B' } else { 'A' });
        assert!(verify_session_id(&tampered, token).is_err());
    }

    #[test]
    fn test_wrong_token_rejected() {
        let sid = generate_session_id("correct-token");
        assert!(verify_session_id(&sid, "wrong-token").is_err());
    }

    #[test]
    fn test_empty_token_always_passes() {
        // 无 token 模式不校验
        let sid = generate_session_id("");
        assert!(verify_session_id(&sid, "").is_ok());
        // 纯随机 ID 也通过（没有点）
        assert!(verify_session_id("anyrandomstring", "").is_ok());
    }

    #[test]
    fn test_each_sid_is_unique() {
        let token = "tok";
        let a = generate_session_id(token);
        let b = generate_session_id(token);
        assert_ne!(a, b, "nonce should be random each time");
    }

    #[test]
    fn test_ttl_not_expired() {
        let timer = SessionTimer::new(30);
        assert!(!timer.is_expired());
    }
}
