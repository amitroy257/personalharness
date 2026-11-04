//! Local OpenAI-compatible API (`http://127.0.0.1:<port>/v1`).
//!
//! Security model: binds to loopback only, every `/v1` and `/__ui` request
//! needs the local token, and requests whose `Host` header is not a loopback
//! name are rejected (defeats DNS-rebinding from web pages). No CORS headers
//! are ever sent, so browsers cannot read responses cross-origin.

use std::convert::Infallible;
use std::sync::Arc;

use axum::body::Body;
use axum::extract::{DefaultBodyLimit, Path, Query, Request, State};
use axum::http::{header, HeaderMap, HeaderValue, StatusCode};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use bytes::Bytes;
use serde::Deserialize;
use serde_json::{json, Value};
use tokio_stream::wrappers::UnboundedReceiverStream;

use crate::harness::Harness;
use crate::ranking::{Preset, View};
use crate::router;

pub fn api_router(h: Arc<Harness>) -> Router {
    Router::new()
        .route("/health", get(health))
        .route("/v1/models", get(list_models))
        .route("/v1/chat/completions", post(chat_completions))
        .route("/v1/rankings", get(rankings))
        .with_state(h)
}

/// Management endpoints for the browser UI in headless mode.
pub fn ui_router(h: Arc<Harness>) -> Router {
    Router::new().route("/__ui/call/{name}", post(ui_call)).route("/__ui/chat", post(ui_chat)).with_state(h)
}

pub fn with_guard(app: Router, h: Arc<Harness>) -> Router {
    app.layer(middleware::from_fn_with_state(h, guard)).layer(DefaultBodyLimit::max(50 * 1024 * 1024))
}

pub fn openai_error(status: StatusCode, msg: impl Into<String>, kind: &str) -> Response {
    let body = json!({"error": {"message": msg.into(), "type": kind, "code": status.as_u16()}});
    (status, Json(body)).into_response()
}

fn host_is_loopback(host: &str) -> bool {
    let h = host.trim();
    let name = if h.starts_with('[') {
        h.split(']').next().map(|s| format!("{s}]")).unwrap_or_default()
    } else {
        h.split(':').next().unwrap_or("").to_string()
    };
    matches!(name.to_ascii_lowercase().as_str(), "127.0.0.1" | "localhost" | "[::1]")
}

fn token_from(headers: &HeaderMap) -> Option<String> {
    if let Some(v) = headers.get(header::AUTHORIZATION).and_then(|v| v.to_str().ok()) {
        let v = v.trim();
        if let Some(t) = v.strip_prefix("Bearer ").or_else(|| v.strip_prefix("bearer ")) {
            return Some(t.trim().to_string());
        }
    }
    headers.get("x-api-key").and_then(|v| v.to_str().ok()).map(|s| s.trim().to_string())
}

fn ct_eq(a: &str, b: &str) -> bool {
    a.len() == b.len() && a.bytes().zip(b.bytes()).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

async fn guard(State(h): State<Arc<Harness>>, req: Request, next: Next) -> Response {
    let host = req.headers().get(header::HOST).and_then(|v| v.to_str().ok()).unwrap_or("");
    if !host_is_loopback(host) {
        return openai_error(StatusCode::FORBIDDEN, "Host not allowed", "forbidden");
    }
    let path = req.uri().path();
    if path.starts_with("/v1") || path.starts_with("/__ui/") {
        let expected = h.settings().server.token;
        let ok = token_from(req.headers()).map(|t| ct_eq(&t, &expected)).unwrap_or(false);
        if !ok {
            return openai_error(
                StatusCode::UNAUTHORIZED,
                "Missing or wrong API key. Use the token shown in Personal Harness → Local API.",
                "invalid_api_key",
            );
        }
    }
    next.run(req).await
}

async fn health() -> Json<Value> {
    Json(json!({"ok": true, "name": "personal-harness", "version": env!("CARGO_PKG_VERSION")}))
}

async fn list_models(State(h): State<Arc<Harness>>) -> Json<Value> {
    let ranked = h.rankings(View::Recommended(Preset::General));
    let mut data: Vec<Value> = vec![];
    for (id, desc) in [
        ("auto", "Best available model right now (general), with automatic fallback"),
        ("auto:coding", "Best available model for coding"),
        ("auto:reasoning", "Best available model for reasoning / math"),
        ("auto:fast", "Fastest good model"),
    ] {
        data.push(json!({"id": id, "object": "model", "created": 0, "owned_by": "harness", "description": desc}));
    }
    for m in &ranked {
        for r in &m.routes {
            data.push(json!({
                "id": r.api_id,
                "object": "model",
                "created": m.released.unwrap_or(0),
                "owned_by": r.provider,
                "context_length": r.context_length,
                "harness": {"family": m.family, "rank": m.rank, "score": (m.score * 10.0).round() / 10.0, "badges": m.badges},
            }));
        }
    }
    Json(json!({"object": "list", "data": data}))
}

#[derive(Deserialize)]
struct RankQuery {
    view: Option<String>,
    preset: Option<String>,
    limit: Option<usize>,
}

async fn rankings(State(h): State<Arc<Harness>>, Query(q): Query<RankQuery>) -> Json<Value> {
    let view = View::parse(q.view.as_deref().unwrap_or("recommended"), q.preset.as_deref().unwrap_or("general"));
    let mut items = h.rankings(view);
    items.truncate(q.limit.unwrap_or(50));
    Json(json!({"data": items}))
}

fn harness_headers(resp: &mut Response, api_id: &str, provider: &str, attempts: usize) {
    let hs = resp.headers_mut();
    if let Ok(v) = HeaderValue::from_str(api_id) {
        hs.insert("x-harness-model", v);
    }
    if let Ok(v) = HeaderValue::from_str(provider) {
        hs.insert("x-harness-provider", v);
    }
    hs.insert("x-harness-attempts", HeaderValue::from(attempts as u64 + 1));
}

async fn chat_completions(State(h): State<Arc<Harness>>, body: Option<Json<Value>>) -> Response {
    let Some(Json(body)) = body else {
        return openai_error(StatusCode::BAD_REQUEST, "Body must be JSON", "invalid_request_error");
    };
    if !body.get("messages").map(|m| m.is_array()).unwrap_or(false) {
        return openai_error(StatusCode::BAD_REQUEST, "'messages' must be an array", "invalid_request_error");
    }
    let model = body.get("model").and_then(|m| m.as_str()).unwrap_or("auto").to_string();
    let fallbacks: Vec<String> = body
        .get("models")
        .and_then(|m| m.as_array())
        .map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect())
        .unwrap_or_default();
    let stream = body.get("stream").and_then(|s| s.as_bool()).unwrap_or(false);

    let cands = match router::resolve(&h, &model, &fallbacks) {
        Ok(c) => c,
        Err(e) => return openai_error(StatusCode::NOT_FOUND, e, "model_not_found"),
    };
    let routed = match router::execute(&h, cands, &body, stream, "api", |_| {}).await {
        Ok(r) => r,
        Err(e) => {
            let mut resp =
                openai_error(StatusCode::from_u16(e.status).unwrap_or(StatusCode::BAD_GATEWAY), e.message, "upstream_error");
            resp.headers_mut().insert("x-harness-attempts", HeaderValue::from(e.attempts.len() as u64));
            return resp;
        }
    };
    let router::Routed { candidate, response, attempts, started } = routed;
    let api_id = candidate.entry.api_id.clone();
    let provider = candidate.entry.provider.clone();

    if stream {
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel::<Result<Bytes, Infallible>>();
        let h2 = h.clone();
        tokio::spawn(async move {
            let out = crate::relay::pump(response, started, |b| tx.send(Ok(b)).is_ok(), |_| true).await;
            h2.store.record_usage(&out.usage_record(&candidate, "api"));
        });
        let mut resp = Response::new(Body::from_stream(UnboundedReceiverStream::new(rx)));
        resp.headers_mut().insert(header::CONTENT_TYPE, HeaderValue::from_static("text/event-stream"));
        resp.headers_mut().insert(header::CACHE_CONTROL, HeaderValue::from_static("no-cache"));
        harness_headers(&mut resp, &api_id, &provider, attempts.len());
        return resp;
    }

    let bytes = match response.bytes().await {
        Ok(b) => b,
        Err(e) => {
            return openai_error(StatusCode::BAD_GATEWAY, format!("Reading provider response failed: {e}"), "upstream_error")
        }
    };
    let duration_ms = started.elapsed().as_millis() as i64;
    let mut resp = match serde_json::from_slice::<Value>(&bytes) {
        Ok(mut v) => {
            let usage = v.get("usage").cloned().unwrap_or(Value::Null);
            let content = v.pointer("/choices/0/message/content").and_then(|c| c.as_str()).unwrap_or("").to_string();
            let out = crate::relay::Outcome {
                content,
                prompt_tokens: usage.get("prompt_tokens").and_then(|t| t.as_i64()),
                completion_tokens: usage.get("completion_tokens").and_then(|t| t.as_i64()),
                ttft_ms: Some(duration_ms),
                duration_ms,
                ..Default::default()
            };
            h.store.record_usage(&out.usage_record(&candidate, "api"));
            if let Some(o) = v.as_object_mut() {
                o.insert("model".into(), Value::String(api_id.clone()));
                o.insert("provider".into(), Value::String(provider.clone()));
            }
            Json(v).into_response()
        }
        Err(_) => (StatusCode::OK, [(header::CONTENT_TYPE, "application/json")], bytes).into_response(),
    };
    harness_headers(&mut resp, &api_id, &provider, attempts.len());
    resp
}

// ---- headless UI bridge ---------------------------------------------------------

async fn ui_call(State(h): State<Arc<Harness>>, Path(name): Path<String>, body: Option<Json<Value>>) -> Response {
    let args = body.map(|Json(v)| v).unwrap_or(Value::Null);
    match crate::commands::call(&h, &name, args).await {
        Ok(v) => Json(v).into_response(),
        Err(e) => (StatusCode::BAD_REQUEST, Json(json!({"error": e}))).into_response(),
    }
}

async fn ui_chat(State(h): State<Arc<Harness>>, Json(args): Json<Value>) -> Response {
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel::<crate::commands::ChatEvent>();
    tokio::spawn(async move {
        crate::commands::send_message(&h, args, tx).await;
    });
    let stream = UnboundedReceiverStream::new(rx);
    let stream = futures_util::StreamExt::map(stream, |ev| {
        let line = format!("data: {}\n\n", serde_json::to_string(&ev).unwrap_or_default());
        Ok::<Bytes, Infallible>(Bytes::from(line))
    });
    let mut resp = Response::new(Body::from_stream(stream));
    resp.headers_mut().insert(header::CONTENT_TYPE, HeaderValue::from_static("text/event-stream"));
    resp.headers_mut().insert(header::CACHE_CONTROL, HeaderValue::from_static("no-cache"));
    resp
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loopback_hosts() {
        assert!(host_is_loopback("127.0.0.1:4141"));
        assert!(host_is_loopback("localhost"));
        assert!(host_is_loopback("[::1]:4141"));
        assert!(!host_is_loopback("evil.com"));
        assert!(!host_is_loopback("127.0.0.1.evil.com:4141"));
        assert!(!host_is_loopback(""));
    }

    #[test]
    fn constant_time_eq() {
        assert!(ct_eq("abc", "abc"));
        assert!(!ct_eq("abc", "abd"));
        assert!(!ct_eq("abc", "abcd"));
    }
}
