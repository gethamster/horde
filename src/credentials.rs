//! Per-invocation loopback credential broker for API-backed harnesses.
//! The harness receives a scoped token; the upstream provider key stays in the daemon.
use crate::config::ExecutorConfig;
use anyhow::{Context, Result, bail};
use axum::{
    Router,
    body::{Body, Bytes},
    extract::{DefaultBodyLimit, State},
    http::{HeaderMap, Method, StatusCode, Uri},
    response::{IntoResponse, Response},
    routing::any,
};
use serde_json::Value;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
struct Backend {
    observer: Option<std::path::PathBuf>,
    config: ExecutorConfig,
    client: reqwest::Client,
    base: String,
    key: String,
    token: String,
    kind: String,
    model: Option<String>,
    active: Arc<AtomicBool>,
}
pub struct Broker {
    pub base_url: String,
    handle: tokio::task::JoinHandle<()>,
    active: Arc<AtomicBool>,
}
impl Drop for Broker {
    fn drop(&mut self) {
        self.active.store(false, Ordering::Release);
        self.handle.abort();
    }
}
impl Broker {
    pub async fn start(config: &ExecutorConfig, token: &str, timeout: u64) -> Result<Self> {
        let key = crate::config::credential(&config.api_key_env)
            .with_context(|| format!("missing {} in daemon environment", config.api_key_env))?;
        Self::with_key(config, token, key, timeout).await
    }
    pub async fn with_key(
        config: &ExecutorConfig,
        token: &str,
        key: String,
        timeout: u64,
    ) -> Result<Self> {
        Self::with_observer(config, token, key, timeout, None).await
    }
    pub async fn start_observed(
        config: &ExecutorConfig,
        token: &str,
        timeout: u64,
        root: &std::path::Path,
    ) -> Result<Self> {
        let key = crate::config::credential(&config.api_key_env)
            .with_context(|| format!("missing {} in daemon environment", config.api_key_env))?;
        Self::with_observer(config, token, key, timeout, Some(root.to_owned())).await
    }
    async fn with_observer(
        config: &ExecutorConfig,
        token: &str,
        key: String,
        timeout: u64,
        observer: Option<std::path::PathBuf>,
    ) -> Result<Self> {
        if !["codex", "claude"].contains(&config.kind.as_str()) {
            bail!("credential broker supports codex and claude");
        }
        let url = reqwest::Url::parse(&config.base_url)?;
        if url.scheme() != "https"
            && !(url.scheme() == "http"
                && [Some("127.0.0.1"), Some("localhost"), Some("::1")].contains(&url.host_str()))
        {
            bail!("provider keys require HTTPS except on loopback test endpoints");
        }
        if url.query().is_some()
            || url.fragment().is_some()
            || !url.username().is_empty()
            || url.password().is_some()
        {
            bail!("provider base URL cannot include credentials, query, or fragment");
        }
        let active = Arc::new(AtomicBool::new(true));
        let state = Arc::new(Backend {
            observer,
            config: config.clone(),
            client: reqwest::Client::builder()
                .redirect(reqwest::redirect::Policy::none())
                .timeout(std::time::Duration::from_secs(timeout))
                .build()?,
            base: config.base_url.trim_end_matches('/').into(),
            key,
            token: token.into(),
            kind: config.kind.clone(),
            model: config.model.clone(),
            active: active.clone(),
        });
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
        let base_url = format!("http://{}/v1", listener.local_addr()?);
        let app = Router::new()
            .route("/v1/{*path}", any(forward))
            .layer(DefaultBodyLimit::max(16 * 1024 * 1024))
            .with_state(state);
        let handle = tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        Ok(Self {
            base_url,
            handle,
            active,
        })
    }
}
async fn forward(
    State(state): State<Arc<Backend>>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let supplied = headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.strip_prefix("Bearer "))
        .or_else(|| headers.get("x-api-key").and_then(|v| v.to_str().ok()));
    if !state.active.load(Ordering::Acquire) || supplied != Some(state.token.as_str()) {
        return (StatusCode::UNAUTHORIZED, "invalid invocation credential").into_response();
    }
    let path = uri.path().strip_prefix("/v1/").unwrap_or("");
    let allowed = match state.kind.as_str() {
        "codex" => {
            method == Method::POST && ["responses", "responses/compact"].contains(&path)
                || method == Method::GET && path == "models"
        }
        "claude" => method == Method::POST && ["messages", "messages/count_tokens"].contains(&path),
        _ => false,
    };
    if !allowed {
        return (StatusCode::FORBIDDEN, "endpoint outside invocation scope").into_response();
    }
    if method == Method::POST {
        let payload: Value = match serde_json::from_slice(&body) {
            Ok(v) => v,
            Err(_) => {
                return (StatusCode::BAD_REQUEST, "invalid model request JSON").into_response();
            }
        };
        if let Some(model) = &state.model
            && payload["model"] != *model
        {
            return (StatusCode::FORBIDDEN, "model outside invocation scope").into_response();
        }
    }
    let mut url = match reqwest::Url::parse(&format!("{}/{path}", state.base)) {
        Ok(url) => url,
        Err(_) => return (StatusCode::BAD_GATEWAY, "invalid upstream URL").into_response(),
    };
    url.set_query(uri.query());
    let mut request = state
        .client
        .request(method, url)
        .header("content-type", "application/json")
        .body(body);
    if state.kind == "claude" {
        request = request.header("x-api-key", &state.key);
        for name in ["anthropic-version", "anthropic-beta"] {
            if let Some(value) = headers.get(name) {
                request = request.header(name, value);
            }
        }
    } else {
        request = request.bearer_auth(&state.key);
    }
    let response = match request.send().await {
        Ok(r) => r,
        Err(_) => return (StatusCode::BAD_GATEWAY, "provider connection failed").into_response(),
    };
    if let Some(root) = &state.observer
        && let Ok(db) = crate::store::Store::open(root)
    {
        let _ = crate::capacity::ingest_headers(
            &db,
            &state.config,
            response.headers(),
            response.status().as_u16(),
        );
    }
    let mut builder = Response::builder().status(response.status());
    for name in ["content-type", "retry-after", "request-id", "x-request-id"] {
        if let Some(value) = response.headers().get(name) {
            builder = builder.header(name, value);
        }
    }
    builder
        .body(Body::from_stream(response.bytes_stream()))
        .unwrap_or_else(|_| (StatusCode::BAD_GATEWAY, "invalid provider response").into_response())
}
