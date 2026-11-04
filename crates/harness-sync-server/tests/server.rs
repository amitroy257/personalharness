//! Security and behaviour tests for the sync server, over real HTTP.

use std::net::SocketAddr;

use base64::Engine as _;
use harness_sync_server::{open, router, Config};
use serde_json::{json, Value};

const B64: base64::engine::GeneralPurpose = base64::engine::general_purpose::STANDARD;

async fn start(signup_code: Option<&str>, quota_bytes: i64) -> (String, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let state = open(Config {
        data_dir: dir.path().to_path_buf(),
        signup_code: signup_code.map(String::from),
        quota_bytes,
        trust_proxy: false,
    })
    .unwrap();
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = l.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(l, router(state).into_make_service_with_connect_info::<SocketAddr>()).await.unwrap();
    });
    (format!("http://{addr}"), dir)
}

fn http() -> reqwest::Client {
    reqwest::Client::builder().no_proxy().build().unwrap()
}

fn signup_body(email: &str, auth: u8, code: &str) -> Value {
    json!({
        "email": email,
        "auth_key": B64.encode([auth; 32]),
        "salt": B64.encode([7u8; 16]),
        "kdf": {"alg": "argon2id", "m_kib": 8192, "t": 1, "p": 1},
        "wrapped_key": B64.encode([9u8; 72]),
        "signup_code": code,
        "device": "test",
    })
}

async fn post(c: &reqwest::Client, url: String, body: Value, token: Option<&str>) -> (u16, Value) {
    let mut r = c.post(url).json(&body);
    if let Some(t) = token {
        r = r.bearer_auth(t);
    }
    let resp = r.send().await.unwrap();
    let code = resp.status().as_u16();
    (code, resp.json().await.unwrap_or(Value::Null))
}

#[tokio::test]
async fn signup_policy_owner_only_then_invite_code() {
    let (base, _d) = start(None, 1 << 20).await;
    let c = http();
    let (code, v) = post(&c, format!("{base}/v1/account/signup"), signup_body("Owner@Example.com", 1, ""), None).await;
    assert_eq!(code, 200, "{v}");
    assert_eq!(v["email"], "owner@example.com");
    let (code, _) = post(&c, format!("{base}/v1/account/signup"), signup_body("stranger@example.com", 2, ""), None).await;
    assert_eq!(code, 403, "without SIGNUP_CODE only the first account may be created");

    let (base, _d2) = start(Some("family-123"), 1 << 20).await;
    let (code, _) = post(&c, format!("{base}/v1/account/signup"), signup_body("a@example.com", 1, "wrong"), None).await;
    assert_eq!(code, 403);
    let (code, _) = post(&c, format!("{base}/v1/account/signup"), signup_body("a@example.com", 1, "family-123"), None).await;
    assert_eq!(code, 200);
    let (code, _) = post(&c, format!("{base}/v1/account/signup"), signup_body("A@example.com", 1, "family-123"), None).await;
    assert_eq!(code, 409, "duplicate email (case-insensitive)");
}

#[tokio::test]
async fn login_lockout_and_no_email_enumeration() {
    let (base, _d) = start(None, 1 << 20).await;
    let c = http();
    post(&c, format!("{base}/v1/account/signup"), signup_body("me@example.com", 1, ""), None).await;

    // prelogin answers the same way for unknown emails (deterministic fake salt)
    let (_, known) = post(&c, format!("{base}/v1/account/prelogin"), json!({"email": "me@example.com"}), None).await;
    let (_, unknown1) = post(&c, format!("{base}/v1/account/prelogin"), json!({"email": "nobody@example.com"}), None).await;
    let (_, unknown2) = post(&c, format!("{base}/v1/account/prelogin"), json!({"email": "nobody@example.com"}), None).await;
    assert_eq!(known["salt"], B64.encode([7u8; 16]));
    assert_eq!(unknown1, unknown2, "fake answer is stable");
    assert!(unknown1["salt"].is_string() && unknown1["kdf"]["alg"] == "argon2id");

    // wrong key: 401 with the same message as unknown email
    let (code, v1) =
        post(&c, format!("{base}/v1/account/login"), json!({"email": "me@example.com", "auth_key": B64.encode([2u8; 32])}), None)
            .await;
    let (code2, v2) =
        post(&c, format!("{base}/v1/account/login"), json!({"email": "x@example.com", "auth_key": B64.encode([2u8; 32])}), None)
            .await;
    assert_eq!((code, code2), (401, 401));
    assert_eq!(v1, v2, "identical errors do not reveal which emails exist");

    // brute force: locked after 10 failures, even with the right key
    for _ in 0..10 {
        post(&c, format!("{base}/v1/account/login"), json!({"email": "me@example.com", "auth_key": B64.encode([3u8; 32])}), None)
            .await;
    }
    let (code, _) =
        post(&c, format!("{base}/v1/account/login"), json!({"email": "me@example.com", "auth_key": B64.encode([1u8; 32])}), None)
            .await;
    assert_eq!(code, 429);
}

#[tokio::test]
async fn sync_requires_auth_and_round_trips() {
    let (base, dir) = start(None, 1 << 20).await;
    let c = http();
    let (_, s) = post(&c, format!("{base}/v1/account/signup"), signup_body("me@example.com", 1, ""), None).await;
    let token = s["token"].as_str().unwrap().to_string();

    // every sync endpoint needs a valid session
    assert_eq!(c.get(format!("{base}/v1/sync/pull")).send().await.unwrap().status(), 401);
    assert_eq!(c.get(format!("{base}/v1/sync/pull")).bearer_auth("forged").send().await.unwrap().status(), 401);
    let (code, _) = post(&c, format!("{base}/v1/sync/push"), json!({"records": []}), None).await;
    assert_eq!(code, 401);

    // push / pull, overwrite keeps one row per record id
    let blob = |b: u8| B64.encode([b; 64]);
    let (code, v) = post(
        &c,
        format!("{base}/v1/sync/push"),
        json!({"records": [{"rid": "r1", "blob": blob(1)}, {"rid": "r2", "blob": blob(2)}]}),
        Some(&token),
    )
    .await;
    assert_eq!(code, 200, "{v}");
    assert_eq!(v["latest_seq"], 2);
    post(&c, format!("{base}/v1/sync/push"), json!({"records": [{"rid": "r1", "blob": blob(3)}]}), Some(&token)).await;
    let v: Value = c.get(format!("{base}/v1/sync/pull?since=0")).bearer_auth(&token).send().await.unwrap().json().await.unwrap();
    let recs = v["records"].as_array().unwrap();
    assert_eq!(recs.len(), 2);
    assert_eq!(recs[1]["rid"], "r1");
    assert_eq!(recs[1]["seq"], 3);
    assert_eq!(recs[1]["blob"], blob(3));
    let v: Value = c.get(format!("{base}/v1/sync/pull?since=2")).bearer_auth(&token).send().await.unwrap().json().await.unwrap();
    assert_eq!(v["records"].as_array().unwrap().len(), 1);

    // input validation
    let (code, _) =
        post(&c, format!("{base}/v1/sync/push"), json!({"records": [{"rid": "../x", "blob": blob(1)}]}), Some(&token)).await;
    assert_eq!(code, 400);

    // logout revokes the session
    post(&c, format!("{base}/v1/account/logout"), json!({}), Some(&token)).await;
    assert_eq!(c.get(format!("{base}/v1/sync/pull")).bearer_auth(&token).send().await.unwrap().status(), 401);

    // login again, then delete the account: all data is erased
    let (code, l) =
        post(&c, format!("{base}/v1/account/login"), json!({"email": "me@example.com", "auth_key": B64.encode([1u8; 32])}), None)
            .await;
    assert_eq!(code, 200);
    assert_eq!(l["wrapped_key"], B64.encode([9u8; 72]));
    let t2 = l["token"].as_str().unwrap().to_string();
    let r = c
        .delete(format!("{base}/v1/account"))
        .bearer_auth(&t2)
        .json(&json!({"auth_key": B64.encode([2u8; 32])}))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 401, "deleting needs the password again");
    let r = c
        .delete(format!("{base}/v1/account"))
        .bearer_auth(&t2)
        .json(&json!({"auth_key": B64.encode([1u8; 32])}))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 200);
    let (code, _) =
        post(&c, format!("{base}/v1/account/login"), json!({"email": "me@example.com", "auth_key": B64.encode([1u8; 32])}), None)
            .await;
    assert_eq!(code, 401);
    let raw: Vec<u8> =
        std::fs::read_dir(dir.path()).unwrap().flat_map(|f| std::fs::read(f.unwrap().path()).unwrap_or_default()).collect();
    let marker = [3u8; 64];
    assert!(!raw.windows(64).any(|w| w == marker), "deleted record bytes still on disk");
}

#[tokio::test]
async fn quota_and_size_limits() {
    let (base, _d) = start(None, 1000).await;
    let c = http();
    let (_, s) = post(&c, format!("{base}/v1/account/signup"), signup_body("me@example.com", 1, ""), None).await;
    let token = s["token"].as_str().unwrap();
    let (code, _) =
        post(&c, format!("{base}/v1/sync/push"), json!({"records": [{"rid": "a", "blob": B64.encode([1u8; 600])}]}), Some(token))
            .await;
    assert_eq!(code, 200);
    let (code, _) =
        post(&c, format!("{base}/v1/sync/push"), json!({"records": [{"rid": "b", "blob": B64.encode([1u8; 600])}]}), Some(token))
            .await;
    assert_eq!(code, 507, "quota enforced");
    let (code, _) =
        post(&c, format!("{base}/v1/sync/push"), json!({"records": [{"rid": "a", "blob": B64.encode([1u8; 900])}]}), Some(token))
            .await;
    assert_eq!(code, 200, "replacing a record only counts the difference");
}
