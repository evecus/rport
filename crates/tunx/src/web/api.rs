//! `/api/*` 路由处理函数

use std::sync::atomic::Ordering;

use axum::extract::State;
use axum::http::{header, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::{Deserialize, Serialize};

use tunx_common::config::TunxConfig;

use crate::web::auth::{self, SESSION_COOKIE_NAME};
use crate::web::WebState;

// ─── 登录 / 登出 ─────────────────────────────────────────────────────────────

#[derive(Deserialize)]
pub struct LoginBody {
    username: String,
    password: String,
}

pub async fn login(
    State(state): State<WebState>,
    Json(body): Json<LoginBody>,
) -> Result<Response, StatusCode> {
    let cfg = state.app.snapshot_config().await;
    if body.username != cfg.web.username || !auth::verify_password(&body.password, &cfg.web.password_hash) {
        return Err(StatusCode::UNAUTHORIZED);
    }

    let token = state.sessions.issue().await;
    let cookie = format!(
        "{SESSION_COOKIE_NAME}={token}; Path=/; HttpOnly; SameSite=Strict; Max-Age={}",
        7 * 24 * 3600
    );
    let mut resp = Json(serde_json::json!({ "ok": true })).into_response();
    resp.headers_mut()
        .insert(header::SET_COOKIE, cookie.parse().unwrap());
    Ok(resp)
}

pub async fn logout(
    State(state): State<WebState>,
    headers: axum::http::HeaderMap,
) -> Response {
    if let Some(token) = extract_token_from_headers(&headers) {
        state.sessions.revoke(&token).await;
    }
    let cookie = format!("{SESSION_COOKIE_NAME}=; Path=/; HttpOnly; Max-Age=0");
    let mut resp = Json(serde_json::json!({ "ok": true })).into_response();
    resp.headers_mut()
        .insert(header::SET_COOKIE, cookie.parse().unwrap());
    resp
}

fn extract_token_from_headers(headers: &axum::http::HeaderMap) -> Option<String> {
    let cookie_header = headers.get(header::COOKIE)?.to_str().ok()?;
    for part in cookie_header.split(';') {
        let part = part.trim();
        if let Some(v) = part.strip_prefix(&format!("{SESSION_COOKIE_NAME}=")) {
            return Some(v.to_string());
        }
    }
    None
}

// ─── 配置读写 ─────────────────────────────────────────────────────────────────

pub async fn get_config(State(state): State<WebState>) -> Json<serde_json::Value> {
    let mut cfg = state.app.snapshot_config().await;
    let is_runnable = cfg.is_runnable();
    cfg.web.password_hash = String::new(); // 不下发哈希
    let mut v = serde_json::to_value(&cfg).unwrap_or_default();
    if let Some(obj) = v.as_object_mut() {
        obj.insert("is_runnable".into(), serde_json::json!(is_runnable));
    }
    Json(v)
}

#[derive(Deserialize)]
pub struct SaveConfigBody {
    #[serde(flatten)]
    pub config: TunxConfig,
    /// 若前端提交了新明文密码，用这个字段传（可选）；
    /// 不填则保留原有 password_hash 不变
    pub new_password: Option<String>,
}

pub async fn put_config(
    State(state): State<WebState>,
    Json(body): Json<SaveConfigBody>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let mut new_cfg = body.config;

    // password_hash 处理：前端提交的配置里这个字段永远是空字符串（get_config 抹掉了），
    // 所以这里要么用 new_password 生成新哈希，要么复用旧哈希，不能直接采用前端传来的空值
    let old_cfg = state.app.snapshot_config().await;
    match body.new_password {
        Some(pw) if !pw.is_empty() => {
            let hash = auth::hash_password(&pw)
                .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
            new_cfg.web.password_hash = hash;
        }
        _ => {
            new_cfg.web.password_hash = old_cfg.web.password_hash.clone();
        }
    }
    if new_cfg.web.username.trim().is_empty() {
        new_cfg.web.username = old_cfg.web.username.clone();
    }

    state
        .app
        .save_and_restart(new_cfg)
        .await
        .map_err(|e| (StatusCode::BAD_REQUEST, format!("{e:#}")))?;

    let cfg = state.app.snapshot_config().await;
    Ok(Json(serde_json::json!({
        "ok": true,
        "is_runnable": cfg.is_runnable(),
    })))
}

// ─── 运行状态 ─────────────────────────────────────────────────────────────────

#[derive(Serialize)]
pub struct StatusResponse {
    version: &'static str,
    mode: &'static str,
    running: bool,
    is_runnable: bool,
    config_path: String,
}

pub async fn get_status(State(state): State<WebState>) -> Json<StatusResponse> {
    let cfg = state.app.snapshot_config().await;
    let running = state.app.is_running().await;
    Json(StatusResponse {
        version: env!("CARGO_PKG_VERSION"),
        mode: match cfg.mode {
            tunx_common::config::Mode::Server => "server",
            tunx_common::config::Mode::Client => "client",
        },
        running,
        is_runnable: cfg.is_runnable(),
        config_path: state.app.config_path_str(),
    })
}

// ─── Server 模式：session / proxy 列表 ────────────────────────────────────────

#[derive(Serialize)]
pub struct ProxyInfo {
    name: String,
    proxy_type: &'static str,
    local_addr: String,
    remote_port: u16,
    bytes_up: u64,
    bytes_down: u64,
    active_conns: u64,
    total_conns: u64,
}

#[derive(Serialize)]
pub struct SessionInfo {
    session_id: String,
    client_id: String,
    last_pong_secs_ago: i64,
    connected: bool,
    proxies: Vec<ProxyInfo>,
}

pub async fn get_sessions(State(state): State<WebState>) -> Json<Vec<SessionInfo>> {
    let Some(app_state) = state.app.server_state().await else {
        return Json(Vec::new());
    };

    let sessions_map = app_state.sessions.lock().await;
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);

    let mut out = Vec::with_capacity(sessions_map.len());
    for (session_id, session) in sessions_map.iter() {
        let last_pong = session.last_pong.load(Ordering::Relaxed);
        let last_pong_secs_ago = if last_pong > 0 { now - last_pong } else { -1 };
        let connected = session
            .server_tx
            .try_read()
            .map(|g| g.is_some())
            .unwrap_or(false);

        let proxies_map = session.proxies.lock().await;
        let mut proxies = Vec::with_capacity(proxies_map.len());
        for (name, handle) in proxies_map.iter() {
            let key = crate::server::metrics_key(session_id, name);
            let snap = app_state.metrics.get_or_create(&key).await.snapshot();
            proxies.push(ProxyInfo {
                name: name.clone(),
                proxy_type: handle.proxy_type,
                local_addr: handle.local_addr.clone(),
                remote_port: handle.remote_port,
                bytes_up: snap.bytes_up,
                bytes_down: snap.bytes_down,
                active_conns: snap.active_conns,
                total_conns: snap.total_conns,
            });
        }

        out.push(SessionInfo {
            session_id: session_id.clone(),
            client_id: session.client_id.clone(),
            last_pong_secs_ago,
            connected,
            proxies,
        });
    }

    Json(out)
}

// ─── Client 模式：本地代理流量统计 ─────────────────────────────────────────────

#[derive(Serialize)]
pub struct ClientProxyInfo {
    name: String,
    bytes_up: u64,
    bytes_down: u64,
    active_conns: u64,
    total_conns: u64,
}

pub async fn get_client_metrics(State(state): State<WebState>) -> Json<Vec<ClientProxyInfo>> {
    let cfg = state.app.snapshot_config().await;
    let Some(client_cfg) = cfg.client.as_ref() else {
        return Json(Vec::new());
    };

    let mut out = Vec::with_capacity(client_cfg.proxies.len());
    for p in &client_cfg.proxies {
        let name = p.name.clone();
        let snap = state.app.metrics.get_or_create(&name).await.snapshot();
        out.push(ClientProxyInfo {
            name,
            bytes_up: snap.bytes_up,
            bytes_down: snap.bytes_down,
            active_conns: snap.active_conns,
            total_conns: snap.total_conns,
        });
    }
    Json(out)
}
