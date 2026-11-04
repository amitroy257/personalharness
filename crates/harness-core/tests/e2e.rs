//! End-to-end: mock providers <- harness router <- local OpenAI-compatible API.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use axum::body::Body;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use harness_core::commands::{self, ChatEvent};
use harness_core::settings::ProviderSettings;
use harness_core::Harness;
use serde_json::{json, Value};

#[derive(Default)]
struct Mock {
    calls: AtomicUsize,
    /// Every header value any provider received, for leak checks.
    seen_headers: std::sync::Mutex<Vec<String>>,
}

async fn models(Path(p): Path<String>) -> Json<Value> {
    let ids: Vec<&str> = match p.as_str() {
        "a" => vec!["limited-70b", "solo-8b", "whisper-large-v3"],
        _ => vec!["limited-70b"],
    };
    Json(json!({"object": "list", "data": ids.iter().map(|i| json!({"id": i, "context_window": 32768})).collect::<Vec<_>>()}))
}

async fn chat(State(m): State<Arc<Mock>>, Path(p): Path<String>, headers: HeaderMap, Json(body): Json<Value>) -> Response {
    m.calls.fetch_add(1, Ordering::SeqCst);
    // Key isolation: each provider only ever receives its own key.
    let expected = if p == "a" { "Bearer key-groq" } else { "Bearer key-cerebras" };
    assert_eq!(headers.get("authorization").unwrap().to_str().unwrap(), expected);
    m.seen_headers
        .lock()
        .unwrap()
        .extend(headers.iter().map(|(k, v)| format!("{k}: {}", v.to_str().unwrap_or(""))));
    assert!(body.get("models").is_none(), "OpenRouter-only fields are stripped");
    let model = body["model"].as_str().unwrap().to_string();
    if p == "a" && model == "limited-70b" {
        return (
            StatusCode::TOO_MANY_REQUESTS,
            [("retry-after", "30")],
            Json(json!({"error": {"message": "Rate limit reached"}})),
        )
            .into_response();
    }
    let reply = format!("hello from {p}/{model}");
    if body["stream"].as_bool() == Some(true) {
        let (a, b) = reply.split_at(5);
        let sse = format!(
            "data: {}\n\ndata: {}\n\ndata: {}\n\ndata: [DONE]\n\n",
            json!({"choices":[{"index":0,"delta":{"role":"assistant","content":a}}]}),
            json!({"choices":[{"index":0,"delta":{"content":b}}]}),
            json!({"choices":[],"usage":{"prompt_tokens":5,"completion_tokens":4}}),
        );
        return Response::builder().header("content-type", "text/event-stream").body(Body::from(sse)).unwrap();
    }
    Json(json!({
        "id": "c1", "object": "chat.completion", "model": model,
        "choices": [{"index": 0, "message": {"role": "assistant", "content": reply}, "finish_reason": "stop"}],
        "usage": {"prompt_tokens": 5, "completion_tokens": 4}
    }))
    .into_response()
}

async fn start_mock() -> (String, Arc<Mock>) {
    let m = Arc::new(Mock::default());
    let app = Router::new().route("/{p}/models", get(models)).route("/{p}/chat/completions", post(chat)).with_state(m.clone());
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = l.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(l, app).await.unwrap() });
    (format!("http://{addr}"), m)
}

fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port()
}

#[tokio::test]
async fn routes_falls_back_streams_and_records() {
    let (mock, calls) = start_mock().await;
    let dir = tempfile::tempdir().unwrap();
    let h = Harness::open(dir.path()).unwrap();
    let port = free_port();
    h.update_settings(|s| {
        s.server.port = port;
        s.ranking.public_data = false;
        for (id, p) in [("groq", "a"), ("cerebras", "b")] {
            s.providers.insert(
                id.into(),
                ProviderSettings {
                    enabled: true,
                    api_key: format!("key-{id}"),
                    base_url: Some(format!("{mock}/{p}")),
                    ..Default::default()
                },
            );
        }
    })
    .unwrap();
    let res = h.refresh_all_providers().await;
    assert!(res.iter().all(|(_, r)| r.is_ok()), "{res:?}");
    let entries = h.entries(false);
    assert_eq!(entries.len(), 3, "whisper filtered out: {entries:?}");

    h.start_server().await.unwrap();
    let token = h.settings().server.token;
    let base = format!("http://127.0.0.1:{port}");
    let c = reqwest::Client::builder().no_proxy().build().unwrap();

    // auth + host checks
    assert_eq!(c.get(format!("{base}/v1/models")).send().await.unwrap().status(), 401);
    assert_eq!(
        c.get(format!("{base}/v1/models")).bearer_auth(&token).header("Host", "evil.example").send().await.unwrap().status(),
        403
    );
    assert_eq!(c.get(format!("{base}/health")).send().await.unwrap().status(), 200);

    // models list contains virtual + real ids
    let v: Value = c.get(format!("{base}/v1/models")).bearer_auth(&token).send().await.unwrap().json().await.unwrap();
    let ids: Vec<&str> = v["data"].as_array().unwrap().iter().map(|m| m["id"].as_str().unwrap()).collect();
    assert!(ids.contains(&"auto") && ids.contains(&"groq/solo-8b") && ids.contains(&"cerebras/limited-70b"), "{ids:?}");

    // explicit model rate-limited on groq -> same model on cerebras
    let r = c
        .post(format!("{base}/v1/chat/completions"))
        .bearer_auth(&token)
        .header("x-client-secret", "client-only-s3cr3t")
        .header("cookie", "session=client-cookie")
        .json(&json!({"model": "groq/limited-70b", "messages": [{"role": "user", "content": "hi"}], "models": ["groq/solo-8b"]}))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 200);
    assert_eq!(r.headers()["x-harness-model"], "cerebras/limited-70b");
    assert_eq!(r.headers()["x-harness-attempts"], "2");
    let v: Value = r.json().await.unwrap();
    assert_eq!(v["model"], "cerebras/limited-70b");
    assert_eq!(v["choices"][0]["message"]["content"], "hello from b/limited-70b");
    let before = calls.calls.load(Ordering::SeqCst);

    // groq route is now cooling down: the next request goes straight to cerebras
    let r = c
        .post(format!("{base}/v1/chat/completions"))
        .bearer_auth(&token)
        .json(&json!({"model": "limited-70b", "messages": [{"role": "user", "content": "again"}]}))
        .send()
        .await
        .unwrap();
    assert_eq!(r.headers()["x-harness-model"], "cerebras/limited-70b");
    assert_eq!(calls.calls.load(Ordering::SeqCst), before + 1);
    assert!(h.cooldowns().contains_key("groq/limited-70b"));

    // streaming via auto
    let r = c
        .post(format!("{base}/v1/chat/completions"))
        .bearer_auth(&token)
        .json(&json!({"model": "auto:fast", "stream": true, "messages": [{"role": "user", "content": "stream"}]}))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 200);
    assert!(r.headers()["content-type"].to_str().unwrap().contains("event-stream"));
    let text = r.text().await.unwrap();
    assert!(text.contains("hello") && text.trim_end().ends_with("data: [DONE]"), "{text}");

    // unknown model
    let r = c
        .post(format!("{base}/v1/chat/completions"))
        .bearer_auth(&token)
        .json(&json!({"model": "nope/does-not-exist", "messages": []}))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 404);

    // rankings endpoint
    let v: Value =
        c.get(format!("{base}/v1/rankings?view=strongest")).bearer_auth(&token).send().await.unwrap().json().await.unwrap();
    assert_eq!(v["data"].as_array().unwrap().len(), 2, "two model families");

    // usage was recorded locally (metadata only)
    let stats = h.store.family_stats(1);
    assert!(stats["limited-70b"].attempts >= 3);
    assert!(stats["limited-70b"].rate_limited >= 1);

    // chat command used by the app UI
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    commands::send_message(&h, json!({"content": "Hi there", "model": "groq/solo-8b"}), tx).await;
    let mut events = vec![];
    while let Ok(ev) = rx.try_recv() {
        events.push(ev);
    }
    let text: String =
        events.iter().filter_map(|e| if let ChatEvent::Delta { content } = e { Some(content.as_str()) } else { None }).collect();
    assert_eq!(text, "hello from a/solo-8b");
    let done =
        events.iter().find_map(|e| if let ChatEvent::Done { message } = e { Some(message.clone()) } else { None }).unwrap();
    assert_eq!(done.api_id, "groq/solo-8b");
    assert_eq!(done.completion_tokens, Some(4));
    let conv_id = done.conversation_id.clone();
    let v = commands::call(&h, "get_conversation", json!({"id": conv_id})).await.unwrap();
    assert_eq!(v["messages"].as_array().unwrap().len(), 2);
    assert_eq!(v["conversation"]["title"], "Hi there");
    let v = commands::call(&h, "rate_message", json!({"id": done.id, "rating": 1})).await.unwrap();
    assert_eq!(v["ok"], true);
    let mine = commands::call(&h, "rankings", json!({"view": "mine"})).await.unwrap();
    assert!(!mine["items"].as_array().unwrap().is_empty());

    // Nothing from the client side leaks upstream: not the local token, not
    // the client's own headers or cookies.
    let seen = calls.seen_headers.lock().unwrap().join("\n");
    assert!(!seen.contains(&token), "local API token was forwarded to a provider");
    assert!(!seen.contains("client-only-s3cr3t") && !seen.contains("client-cookie"), "client headers forwarded:\n{seen}");

    // privacy self-check (headless on Linux/macOS/Windows CI): nothing may fail
    let sc = commands::call(&h, "privacy_selfcheck", Value::Null).await.unwrap();
    assert_eq!(sc["failed"], 0, "{sc:#}");
    let ids: Vec<&str> = sc["checks"].as_array().unwrap().iter().map(|c| c["id"].as_str().unwrap()).collect();
    for want in ["api_requires_token", "api_host_check", "api_no_cors", "api_loopback_only"] {
        assert!(ids.contains(&want), "{want} missing: {ids:?}");
    }
    let v: Value = c.get(format!("{base}/v1/selfcheck")).bearer_auth(&token).send().await.unwrap().json().await.unwrap();
    assert_eq!(v["failed"], 0);

    // providers view never exposes full keys
    let p = commands::call(&h, "list_providers", Value::Null).await.unwrap();
    let s = p.to_string();
    assert!(!s.contains("key-groq") && s.contains("…groq"));
}
