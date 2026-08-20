//! Web 管理界面：axum HTTP 服务 + 内嵌 Vue 前端静态资源
//!
//! 监听地址来自 `[web].listen`（默认 0.0.0.0:1080），可通过配置修改。
//! 所有 `/api/*`（除 `/api/auth/login`）都要求登录态。

pub mod api;
pub mod auth;

use std::sync::Arc;

use anyhow::{Context, Result};
use axum::body::{boxed, Body};
use axum::http::{header, StatusCode, Uri};
use axum::middleware;
use axum::response::Response;
use axum::routing::{get, post};
use axum::Router;
use rust_embed::RustEmbed;
use tracing::info;

use crate::runtime::AppRuntime;

#[derive(RustEmbed)]
#[folder = "../../web-ui/dist"]
struct Assets;

#[derive(Clone)]
pub struct WebState {
    pub app: Arc<AppRuntime>,
    pub sessions: Arc<auth::SessionStore>,
}

pub async fn serve(app: Arc<AppRuntime>) -> Result<()> {
    let listen = app.snapshot_config().await.web.listen.clone();
    let state = WebState {
        app,
        sessions: Arc::new(auth::SessionStore::new()),
    };

    let protected = Router::new()
        .route("/api/auth/logout", post(api::logout))
        .route("/api/status", get(api::get_status))
        .route(
            "/api/config",
            get(api::get_config).put(api::put_config),
        )
        .route("/api/sessions", get(api::get_sessions))
        .route("/api/client-metrics", get(api::get_client_metrics))
        .route_layer(middleware::from_fn_with_state(
            state.clone(),
            auth::require_auth,
        ));

    let public = Router::new().route("/api/auth/login", post(api::login));

    let app_router = Router::new()
        .merge(public)
        .merge(protected)
        .fallback(static_handler)
        .with_state(state);

    let addr: std::net::SocketAddr = listen
        .parse()
        .with_context(|| format!("invalid [web].listen address: {listen}"))?;

    info!("Web UI listening on http://{addr}");

    axum::Server::bind(&addr)
        .serve(app_router.into_make_service())
        .await
        .context("web server error")?;

    Ok(())
}

// ─── 内嵌静态资源 ─────────────────────────────────────────────────────────────

async fn static_handler(uri: Uri) -> Response {
    let path = uri.path().trim_start_matches('/');
    let path = if path.is_empty() { "index.html" } else { path };

    match Assets::get(path) {
        Some(content) => {
            let mime = mime_guess::from_path(path).first_or_octet_stream();
            let mut resp = Response::new(boxed(Body::from(content.data.into_owned())));
            resp.headers_mut().insert(
                header::CONTENT_TYPE,
                header::HeaderValue::from_str(mime.as_ref()).unwrap(),
            );
            resp
        }
        None => {
            // SPA 路由：找不到静态文件时回退到 index.html，交给前端路由处理
            match Assets::get("index.html") {
                Some(content) => {
                    let mut resp =
                        Response::new(boxed(Body::from(content.data.into_owned())));
                    resp.headers_mut().insert(
                        header::CONTENT_TYPE,
                        header::HeaderValue::from_static("text/html; charset=utf-8"),
                    );
                    resp
                }
                None => {
                    let mut resp = Response::new(boxed(Body::from(
                        "web UI not built (web-ui/dist missing)",
                    )));
                    *resp.status_mut() = StatusCode::NOT_FOUND;
                    resp
                }
            }
        }
    }
}
