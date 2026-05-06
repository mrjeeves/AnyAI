//! OpenAI-compatible HTTP server.
//!
//! Listens on a configurable host:port (default 127.0.0.1:1473), translates virtual
//! model IDs (e.g. `anyai-text`) to the currently-resolved underlying tag, and proxies
//! to Ollama at 127.0.0.1:11434. Streaming requests are forwarded byte-for-byte; the
//! `model` field in each chunk is rewritten back to the requested virtual ID so clients
//! see what they asked for.
//!
//! See README ("Serve") for endpoint semantics.

use anyhow::{anyhow, Result};
use axum::body::Body;
use axum::extract::{Query, State};
use axum::http::{HeaderMap, HeaderName, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response, Sse};
use axum::routing::{get, post};
use axum::{Json, Router};
use bytes::Bytes;
use dashmap::DashMap;
use futures_util::StreamExt;
use serde::Deserialize;
use serde_json::{json, Value};
use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::watch;
use tower_http::cors::{Any, CorsLayer};

use crate::api_models::{
    ChatCompletionRequest, CompletionRequest, EmbeddingsRequest, ModelList, ModelObject,
};

const OLLAMA_BASE: &str = "http://127.0.0.1:11434";

#[derive(Clone)]
pub struct AppState {
    pub bearer_token: Option<String>,
    pub pull_status: Arc<DashMap<String, watch::Receiver<PullStatus>>>,
}

#[derive(Debug, Clone)]
pub struct PullStatus {
    pub done: bool,
    pub error: Option<String>,
    pub last_line: String,
}

#[derive(Debug, Deserialize)]
pub struct WaitQuery {
    #[serde(default)]
    pub wait: bool,
}

// ---------------------------------------------------------------------------
// Public entry points
// ---------------------------------------------------------------------------

pub async fn serve(
    host: IpAddr,
    port: u16,
    cors_all: bool,
    bearer_token: Option<String>,
) -> Result<()> {
    let state = AppState {
        bearer_token,
        pull_status: Arc::new(DashMap::new()),
    };

    let mut router = Router::new()
        .route("/healthz", get(healthz))
        .route("/v1/models", get(list_models))
        .route("/v1/chat/completions", post(chat_completions))
        .route("/v1/completions", post(completions))
        .route("/v1/embeddings", post(embeddings))
        .route("/v1/anyai/preload", post(api_preload))
        .route("/v1/anyai/status", get(api_status))
        .with_state(state.clone());

    if cors_all {
        router = router.layer(
            CorsLayer::new()
                .allow_origin(Any)
                .allow_methods(Any)
                .allow_headers(Any),
        );
    }

    let addr = SocketAddr::new(host, port);
    let listener = tokio::net::TcpListener::bind(addr).await.map_err(|e| {
        anyhow!(
            "could not bind {addr}: {e}\n\
             (if another anyai/ollama is running, choose a different --port)"
        )
    })?;

    eprintln!("anyai serve: listening on http://{addr}");
    if cors_all {
        eprintln!("  CORS: allow-all");
    }
    if state.bearer_token.is_some() {
        eprintln!("  Auth: bearer token required");
    } else if !host.is_loopback() {
        eprintln!(
            "  WARNING: bound to non-loopback {host} without --bearer-token; \
             anyone on the network can use this AI."
        );
    }

    axum::serve(listener, router).await?;
    Ok(())
}

/// CLI entry point for `anyai serve`.
pub async fn cmd_serve(args: &[String]) -> Result<()> {
    let mut host: IpAddr = "127.0.0.1".parse().unwrap();
    let mut port: u16 = 1473;
    let mut cors_all = false;
    let mut bearer: Option<String> = None;
    let mut auto_ollama = true;

    // Apply config defaults first.
    if let Ok(cfg) = crate::resolver::load_config_value() {
        if let Some(h) = cfg["api"]["host"].as_str() {
            if let Ok(parsed) = h.parse() {
                host = parsed;
            }
        }
        if let Some(p) = cfg["api"]["port"].as_u64() {
            port = p as u16;
        }
        if cfg["api"]["cors_allow_all"].as_bool() == Some(true) {
            cors_all = true;
        }
        if let Some(t) = cfg["api"]["bearer_token"].as_str() {
            if !t.is_empty() {
                bearer = Some(t.to_string());
            }
        }
    }

    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--host" => {
                host = args
                    .get(i + 1)
                    .ok_or_else(|| anyhow!("--host requires a value"))?
                    .parse()
                    .map_err(|e| anyhow!("invalid --host: {e}"))?;
                i += 2;
            }
            "--port" => {
                port = args
                    .get(i + 1)
                    .ok_or_else(|| anyhow!("--port requires a value"))?
                    .parse()
                    .map_err(|e| anyhow!("invalid --port: {e}"))?;
                i += 2;
            }
            "--cors-allow-all" => {
                cors_all = true;
                i += 1;
            }
            "--bearer-token" => {
                bearer = args.get(i + 1).cloned();
                i += 2;
            }
            "--no-ollama" => {
                auto_ollama = false;
                i += 1;
            }
            _ => i += 1,
        }
    }

    if auto_ollama {
        if !crate::ollama::is_installed() {
            eprintln!("Ollama not found. Installing…");
            crate::ollama::install().await?;
        }
        crate::ollama::ensure_running().await?;
    }

    // Kick off the watcher in the background so tracked modes stay current.
    let _ = crate::watcher::spawn_background();

    tokio::select! {
        res = serve(host, port, cors_all, bearer) => res,
        _ = tokio::signal::ctrl_c() => {
            eprintln!("\nShutting down…");
            Ok(())
        }
    }
}

// ---------------------------------------------------------------------------
// Handlers
// ---------------------------------------------------------------------------

async fn healthz() -> impl IntoResponse {
    let ollama_up = ollama_reachable().await;
    if ollama_up {
        (
            StatusCode::OK,
            Json(json!({"status": "ok", "ollama": true })),
        )
    } else {
        (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({"status": "degraded", "ollama": false})),
        )
    }
}

async fn list_models(State(_state): State<AppState>) -> impl IntoResponse {
    let mut data: Vec<ModelObject> = Vec::new();

    // Virtual models for each known mode.
    for mode in crate::resolver::KNOWN_MODES {
        let id = format!("{}{}", crate::resolver::VIRTUAL_PREFIX, mode);
        let resolved = crate::resolver::resolve(mode).await.ok();
        data.push(ModelObject {
            id,
            object: "model",
            owned_by: "anyai".to_string(),
            created: None,
            metadata: Some(json!({
                "mode": mode,
                "resolved_to": resolved,
            })),
        });
    }

    // Plus every raw pulled tag.
    if let Ok(pulled) = crate::ollama::list_models().await {
        for m in pulled {
            data.push(ModelObject {
                id: m.name,
                object: "model",
                owned_by: "ollama".to_string(),
                created: None,
                metadata: Some(json!({ "size_bytes": m.size })),
            });
        }
    }

    Json(ModelList {
        object: "list",
        data,
    })
}

async fn chat_completions(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<WaitQuery>,
    Json(req): Json<ChatCompletionRequest>,
) -> Response {
    if let Err(resp) = check_auth(&state, &headers) {
        return resp;
    }

    let requested = req.model.clone();
    let resolved = match crate::resolver::translate_virtual(&requested).await {
        Ok(t) => t,
        Err(e) => return error_response(StatusCode::BAD_REQUEST, "bad_model", e.to_string()),
    };

    let wait = q.wait || header_bool(&headers, "x-anyai-wait");
    if let Err(resp) = ensure_model_or_503(&state, &resolved, wait).await {
        return resp;
    }

    let mut body = serde_json::to_value(&req).unwrap_or(json!({}));
    body["model"] = json!(resolved);

    proxy_with_model_rewrite(
        "/v1/chat/completions",
        body,
        req.stream,
        Some(&requested),
        Some(&resolved),
    )
    .await
}

async fn completions(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<WaitQuery>,
    Json(req): Json<CompletionRequest>,
) -> Response {
    if let Err(resp) = check_auth(&state, &headers) {
        return resp;
    }
    let requested = req.model.clone();
    let resolved = match crate::resolver::translate_virtual(&requested).await {
        Ok(t) => t,
        Err(e) => return error_response(StatusCode::BAD_REQUEST, "bad_model", e.to_string()),
    };
    let wait = q.wait || header_bool(&headers, "x-anyai-wait");
    if let Err(resp) = ensure_model_or_503(&state, &resolved, wait).await {
        return resp;
    }
    let mut body = serde_json::to_value(&req).unwrap_or(json!({}));
    body["model"] = json!(resolved);
    proxy_with_model_rewrite(
        "/v1/completions",
        body,
        req.stream,
        Some(&requested),
        Some(&resolved),
    )
    .await
}

async fn embeddings(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<EmbeddingsRequest>,
) -> Response {
    if let Err(resp) = check_auth(&state, &headers) {
        return resp;
    }
    let requested = req.model.clone();
    let resolved = match crate::resolver::translate_virtual(&requested).await {
        Ok(t) => t,
        Err(e) => return error_response(StatusCode::BAD_REQUEST, "bad_model", e.to_string()),
    };
    if let Err(resp) = ensure_model_or_503(&state, &resolved, false).await {
        return resp;
    }
    let mut body = serde_json::to_value(&req).unwrap_or(json!({}));
    body["model"] = json!(resolved);
    proxy_with_model_rewrite(
        "/v1/embeddings",
        body,
        false,
        Some(&requested),
        Some(&resolved),
    )
    .await
}

#[derive(Deserialize)]
struct PreloadBody {
    modes: Vec<String>,
    #[serde(default)]
    track: bool,
    #[serde(default = "default_warm")]
    warm: bool,
}
fn default_warm() -> bool {
    true
}

async fn api_preload(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<PreloadBody>,
) -> Response {
    if let Err(resp) = check_auth(&state, &headers) {
        return resp;
    }
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel::<
        Result<axum::response::sse::Event, std::convert::Infallible>,
    >();

    tokio::spawn(async move {
        let result = crate::preload::preload(&body.modes, body.track, body.warm, |evt| {
            let payload = serde_json::to_string(&evt).unwrap_or_else(|_| "{}".to_string());
            let _ = tx.send(Ok(axum::response::sse::Event::default().data(payload)));
        })
        .await;
        if let Err(e) = result {
            let payload = json!({"status": "error", "detail": e.to_string()}).to_string();
            let _ = tx.send(Ok(axum::response::sse::Event::default().data(payload)));
        }
        let _ = tx.send(Ok(axum::response::sse::Event::default()
            .event("done")
            .data("{}")));
    });

    let stream = tokio_stream::wrappers::UnboundedReceiverStream::new(rx);
    Sse::new(stream)
        .keep_alive(axum::response::sse::KeepAlive::new().interval(Duration::from_secs(15)))
        .into_response()
}

async fn api_status(State(_state): State<AppState>) -> impl IntoResponse {
    let modes = crate::resolver::tracked_modes().unwrap_or_default();
    let mut tracked = serde_json::Map::new();
    for m in &modes {
        let resolved = crate::resolver::resolve(m).await.ok();
        let pulled = match &resolved {
            Some(t) => crate::ollama::has_model(t).await.unwrap_or(false),
            None => false,
        };
        tracked.insert(
            m.clone(),
            json!({
                "resolved_to": resolved,
                "pulled": pulled,
            }),
        );
    }
    let ollama_up = ollama_reachable().await;
    Json(json!({
        "ollama": ollama_up,
        "tracked": tracked,
    }))
}

// ---------------------------------------------------------------------------
// Pull-on-demand
// ---------------------------------------------------------------------------

async fn ensure_model_or_503(
    state: &AppState,
    tag: &str,
    wait: bool,
) -> std::result::Result<(), Response> {
    if crate::ollama::has_model(tag).await.unwrap_or(false) {
        return Ok(());
    }
    let rx = ensure_pull_started(state, tag);
    if !wait {
        let snap = rx.borrow().clone();
        let mut resp = (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({
                "error": {
                    "message": match &snap.error {
                        Some(e) => format!("pull failed for {tag}: {e}"),
                        None => format!("model {tag} is being pulled"),
                    },
                    "type": "anyai_error",
                    "code": if snap.error.is_some() { "pull_failed" } else { "warming_up" },
                    "model": tag,
                    "progress": snap.last_line,
                }
            })),
        )
            .into_response();
        resp.headers_mut()
            .insert("retry-after", HeaderValue::from_static("10"));
        return Err(resp);
    }

    // wait=true: stream pull progress as SSE keep-alives, then proceed.
    let (tx, rx_stream) = tokio::sync::mpsc::unbounded_channel::<
        std::result::Result<axum::response::sse::Event, std::convert::Infallible>,
    >();
    let mut watcher = rx;
    let tag_owned = tag.to_string();
    tokio::spawn(async move {
        loop {
            if watcher.changed().await.is_err() {
                break;
            }
            let snap = watcher.borrow().clone();
            let payload =
                json!({"model": &tag_owned, "line": snap.last_line, "done": snap.done}).to_string();
            let _ = tx.send(Ok(axum::response::sse::Event::default().data(payload)));
            if snap.done {
                break;
            }
        }
    });
    // Block this request until the pull completes.
    let mut local = state.pull_status.get(tag).map(|v| v.value().clone());
    if let Some(mut w) = local.take() {
        loop {
            if w.borrow().done {
                break;
            }
            if w.changed().await.is_err() {
                break;
            }
        }
    }
    drop(rx_stream);
    Ok(())
}

fn ensure_pull_started(state: &AppState, tag: &str) -> watch::Receiver<PullStatus> {
    if let Some(existing) = state.pull_status.get(tag) {
        return existing.value().clone();
    }
    let (tx, rx) = watch::channel(PullStatus {
        done: false,
        error: None,
        last_line: "starting".into(),
    });
    state.pull_status.insert(tag.to_string(), rx.clone());

    let tag_owned = tag.to_string();
    let map = state.pull_status.clone();
    tokio::spawn(async move {
        let res = crate::ollama::pull_with(&tag_owned, |line| {
            let _ = tx.send(PullStatus {
                done: false,
                error: None,
                last_line: line.to_string(),
            });
        })
        .await;
        let final_status = match res {
            Ok(()) => PullStatus {
                done: true,
                error: None,
                last_line: "complete".into(),
            },
            Err(e) => PullStatus {
                done: true,
                error: Some(e.to_string()),
                last_line: format!("error: {e}"),
            },
        };
        let _ = tx.send(final_status);
        // Leave entry in map briefly so concurrent readers see `done`; reap after a bit.
        tokio::time::sleep(Duration::from_secs(30)).await;
        map.remove(&tag_owned);
    });
    rx
}

// ---------------------------------------------------------------------------
// Proxy
// ---------------------------------------------------------------------------

async fn proxy_with_model_rewrite(
    path: &str,
    body: Value,
    stream: bool,
    requested_id: Option<&str>,
    resolved_id: Option<&str>,
) -> Response {
    let url = format!("{OLLAMA_BASE}{path}");
    let client = match reqwest_client() {
        Ok(c) => c,
        Err(e) => {
            return error_response(StatusCode::INTERNAL_SERVER_ERROR, "client", e.to_string())
        }
    };
    let upstream = match client.post(&url).json(&body).send().await {
        Ok(r) => r,
        Err(e) => {
            return error_response(
                StatusCode::BAD_GATEWAY,
                "ollama_unreachable",
                format!("could not reach ollama at {url}: {e}"),
            )
        }
    };

    let status = upstream.status();
    let upstream_headers = upstream.headers().clone();
    let resolved_header = resolved_id.unwrap_or("").to_string();

    if stream {
        let bytes_stream = upstream.bytes_stream();
        let req_owned = requested_id.map(str::to_string);
        let res_owned = resolved_id.map(str::to_string);
        let rewritten = bytes_stream.map(move |chunk| {
            chunk.map(|b| rewrite_stream_chunk(b, req_owned.as_deref(), res_owned.as_deref()))
        });
        let body = Body::from_stream(rewritten);
        let mut resp = Response::builder()
            .status(status)
            .body(body)
            .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response());
        copy_relevant_headers(&upstream_headers, resp.headers_mut());
        if !resolved_header.is_empty() {
            if let Ok(v) = HeaderValue::from_str(&resolved_header) {
                resp.headers_mut()
                    .insert(HeaderName::from_static("x-anyai-resolved-model"), v);
            }
        }
        return resp;
    }

    let bytes = match upstream.bytes().await {
        Ok(b) => b,
        Err(e) => {
            return error_response(
                StatusCode::BAD_GATEWAY,
                "ollama_read",
                format!("error reading ollama response: {e}"),
            )
        }
    };
    let rewritten = rewrite_json_body(&bytes, requested_id, resolved_id);
    let mut resp = Response::builder()
        .status(status)
        .body(Body::from(rewritten))
        .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response());
    copy_relevant_headers(&upstream_headers, resp.headers_mut());
    if !resolved_header.is_empty() {
        if let Ok(v) = HeaderValue::from_str(&resolved_header) {
            resp.headers_mut()
                .insert(HeaderName::from_static("x-anyai-resolved-model"), v);
        }
    }
    resp
}

fn copy_relevant_headers(src: &HeaderMap, dst: &mut HeaderMap) {
    const PASS: &[&str] = &["content-type", "cache-control"];
    for (k, v) in src {
        if PASS.iter().any(|p| k.as_str().eq_ignore_ascii_case(p)) {
            dst.insert(k, v.clone());
        }
    }
}

fn rewrite_json_body(bytes: &[u8], requested: Option<&str>, resolved: Option<&str>) -> Bytes {
    let (Some(req), Some(res)) = (requested, resolved) else {
        return Bytes::copy_from_slice(bytes);
    };
    if req == res {
        return Bytes::copy_from_slice(bytes);
    }
    let mut value: Value = match serde_json::from_slice(bytes) {
        Ok(v) => v,
        Err(_) => return Bytes::copy_from_slice(bytes),
    };
    rewrite_model_field(&mut value, res, req);
    Bytes::from(value.to_string())
}

fn rewrite_stream_chunk(chunk: Bytes, requested: Option<&str>, resolved: Option<&str>) -> Bytes {
    let (Some(req), Some(res)) = (requested, resolved) else {
        return chunk;
    };
    if req == res {
        return chunk;
    }
    let s = match std::str::from_utf8(&chunk) {
        Ok(s) => s,
        Err(_) => return chunk,
    };
    let mut out = String::with_capacity(s.len());
    for line in s.split_inclusive('\n') {
        let trimmed = line
            .trim_start_matches("data: ")
            .trim_end_matches(['\n', '\r']);
        if trimmed.is_empty() || trimmed == "[DONE]" {
            out.push_str(line);
            continue;
        }
        match serde_json::from_str::<Value>(trimmed) {
            Ok(mut v) => {
                rewrite_model_field(&mut v, res, req);
                let serialised = v.to_string();
                if line.starts_with("data: ") {
                    out.push_str("data: ");
                }
                out.push_str(&serialised);
                if line.ends_with('\n') {
                    out.push('\n');
                }
            }
            Err(_) => out.push_str(line),
        }
    }
    Bytes::from(out)
}

fn rewrite_model_field(v: &mut Value, from: &str, to: &str) {
    if let Some(obj) = v.as_object_mut() {
        if let Some(model) = obj.get_mut("model") {
            if model.as_str() == Some(from) {
                *model = json!(to);
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

#[allow(clippy::result_large_err)] // Response is the natural error type for axum handlers.
fn check_auth(state: &AppState, headers: &HeaderMap) -> std::result::Result<(), Response> {
    let Some(expected) = state.bearer_token.as_deref() else {
        return Ok(());
    };
    let Some(authz) = headers.get("authorization").and_then(|v| v.to_str().ok()) else {
        return Err(error_response(
            StatusCode::UNAUTHORIZED,
            "auth_required",
            "Authorization: Bearer <token> required",
        ));
    };
    let token = authz.trim_start_matches("Bearer ").trim();
    if token == expected {
        Ok(())
    } else {
        Err(error_response(
            StatusCode::UNAUTHORIZED,
            "bad_token",
            "invalid bearer token",
        ))
    }
}

fn header_bool(headers: &HeaderMap, key: &str) -> bool {
    headers
        .get(key)
        .and_then(|v| v.to_str().ok())
        .map(|s| matches!(s.to_ascii_lowercase().as_str(), "1" | "true" | "yes"))
        .unwrap_or(false)
}

fn error_response(status: StatusCode, code: &str, msg: impl Into<String>) -> Response {
    (
        status,
        Json(json!({
            "error": {
                "message": msg.into(),
                "type": "anyai_error",
                "code": code,
            }
        })),
    )
        .into_response()
}

fn reqwest_client() -> Result<reqwest::Client> {
    Ok(reqwest::Client::builder()
        .pool_idle_timeout(Duration::from_secs(30))
        .build()?)
}

async fn ollama_reachable() -> bool {
    let client = match reqwest_client() {
        Ok(c) => c,
        Err(_) => return false,
    };
    matches!(
        tokio::time::timeout(
            Duration::from_secs(2),
            client.get(format!("{OLLAMA_BASE}/")).send()
        )
        .await,
        Ok(Ok(r)) if r.status().is_success() || r.status() == StatusCode::NOT_FOUND
    )
}
