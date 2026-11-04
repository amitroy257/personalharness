//! Personal Harness sync server: a zero-knowledge store for end-to-end
//! encrypted records.
//!
//! What the server sees: your email, a KDF salt and parameters, a hash of a
//! *login key* derived from your password on your device, a *wrapped* data key
//! it cannot unwrap, and opaque encrypted records whose ids are keyed hashes.
//! It never receives your password or the key that decrypts your data, so it
//! cannot read chats, settings or API keys — and neither can anyone who
//! steals its database.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use argon2::password_hash::{PasswordHasher, PasswordVerifier};
use argon2::Argon2;
use axum::extract::{ConnectInfo, DefaultBodyLimit, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, post};
use axum::{Json, Router};
use base64::Engine as _;
use hmac::{KeyInit, Mac};
use parking_lot::Mutex;
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

const B64: base64::engine::GeneralPurpose = base64::engine::general_purpose::STANDARD;
const SESSION_DAYS: i64 = 180;
const MAX_RECORD_BYTES: usize = 1024 * 1024;
const MAX_BATCH: usize = 500;
const FAIL_LIMIT: u32 = 10;
const FAIL_WINDOW: Duration = Duration::from_secs(15 * 60);

#[derive(Clone, Debug)]
pub struct Config {
    pub data_dir: PathBuf,
    /// When set, sign-ups must present this code. When unset, only the very
    /// first account can be created (the server owner).
    pub signup_code: Option<String>,
    pub quota_bytes: i64,
    /// Trust `Fly-Client-IP` / `X-Forwarded-For` (only behind a known proxy).
    pub trust_proxy: bool,
}

pub struct AppState {
    db: Mutex<Connection>,
    cfg: Config,
    secret: [u8; 32],
    failures: Mutex<HashMap<String, (u32, Instant)>>,
}

const SCHEMA: &str = r#"
PRAGMA journal_mode = WAL;
PRAGMA foreign_keys = ON;
PRAGMA secure_delete = ON;
CREATE TABLE IF NOT EXISTS meta (k TEXT PRIMARY KEY, v BLOB NOT NULL);
CREATE TABLE IF NOT EXISTS users (
  id INTEGER PRIMARY KEY,
  email TEXT UNIQUE NOT NULL,
  salt TEXT NOT NULL,
  kdf TEXT NOT NULL,
  auth_hash TEXT NOT NULL,
  wrapped_key TEXT NOT NULL,
  seq INTEGER NOT NULL DEFAULT 0,
  bytes INTEGER NOT NULL DEFAULT 0,
  created_at INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS sessions (
  token_hash TEXT PRIMARY KEY,
  user_id INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
  device TEXT NOT NULL DEFAULT '',
  created_at INTEGER NOT NULL,
  expires_at INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS records (
  user_id INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
  rid TEXT NOT NULL,
  seq INTEGER NOT NULL,
  blob BLOB NOT NULL,
  PRIMARY KEY (user_id, rid)
);
CREATE INDEX IF NOT EXISTS records_seq ON records(user_id, seq);
"#;

fn now_ms() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as i64).unwrap_or(0)
}

fn sha256_hex(data: &[u8]) -> String {
    Sha256::digest(data).iter().map(|b| format!("{b:02x}")).collect()
}

pub fn open(cfg: Config) -> anyhow::Result<Arc<AppState>> {
    std::fs::create_dir_all(&cfg.data_dir)?;
    let path = cfg.data_dir.join("sync.db");
    #[cfg(unix)]
    {
        use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
        let _ = std::fs::set_permissions(&cfg.data_dir, std::fs::Permissions::from_mode(0o700));
        let _ = std::fs::OpenOptions::new().create(true).append(true).mode(0o600).open(&path);
    }
    let conn = Connection::open(&path)?;
    conn.execute_batch(SCHEMA)?;
    let secret: Vec<u8> = match conn.query_row("SELECT v FROM meta WHERE k = 'secret'", [], |r| r.get(0)).optional()? {
        Some(v) => v,
        None => {
            let s: [u8; 32] = rand::random();
            conn.execute("INSERT INTO meta (k, v) VALUES ('secret', ?1)", params![s.to_vec()])?;
            s.to_vec()
        }
    };
    let mut sec = [0u8; 32];
    sec.copy_from_slice(&secret[..32]);
    Ok(Arc::new(AppState { db: Mutex::new(conn), cfg, secret: sec, failures: Mutex::new(HashMap::new()) }))
}

pub fn router(state: Arc<AppState>) -> Router {
    Router::new()
        .route(
            "/health",
            get(|| async { Json(json!({"ok": true, "name": "harness-sync", "version": env!("CARGO_PKG_VERSION")})) }),
        )
        .route("/v1/account/prelogin", post(prelogin))
        .route("/v1/account/signup", post(signup))
        .route("/v1/account/login", post(login))
        .route("/v1/account/logout", post(logout))
        .route("/v1/account", get(account).delete(delete_account))
        .route("/v1/sync/push", post(push))
        .route("/v1/sync/pull", get(pull))
        .route("/v1/account/delete", delete(delete_account))
        .layer(DefaultBodyLimit::max(24 * 1024 * 1024))
        .with_state(state)
}

// ---- errors ------------------------------------------------------------------

pub struct ApiError(StatusCode, String);

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (self.0, Json(json!({"error": self.1}))).into_response()
    }
}

fn err(code: StatusCode, msg: impl Into<String>) -> ApiError {
    ApiError(code, msg.into())
}

impl From<rusqlite::Error> for ApiError {
    fn from(e: rusqlite::Error) -> Self {
        tracing::error!("db error: {e}");
        err(StatusCode::INTERNAL_SERVER_ERROR, "storage error")
    }
}

type Res<T> = Result<T, ApiError>;

// ---- helpers ---------------------------------------------------------------------

fn normalize_email(e: &str) -> Res<String> {
    let e = e.trim().to_ascii_lowercase();
    let ok = e.len() <= 254
        && e.split_once('@').map(|(u, d)| !u.is_empty() && d.contains('.') && !d.starts_with('.')).unwrap_or(false);
    if ok {
        Ok(e)
    } else {
        Err(err(StatusCode::BAD_REQUEST, "invalid email"))
    }
}

fn b64_exact(s: &str, len: usize, what: &str) -> Res<Vec<u8>> {
    let v = B64.decode(s.trim()).map_err(|_| err(StatusCode::BAD_REQUEST, format!("{what} is not base64")))?;
    if v.len() != len {
        return Err(err(StatusCode::BAD_REQUEST, format!("{what} must be {len} bytes")));
    }
    Ok(v)
}

fn client_ip(state: &AppState, headers: &HeaderMap, peer: SocketAddr) -> String {
    if state.cfg.trust_proxy {
        for h in ["fly-client-ip", "x-real-ip", "x-forwarded-for"] {
            if let Some(v) = headers.get(h).and_then(|v| v.to_str().ok()) {
                if let Some(first) = v.split(',').next() {
                    return first.trim().to_string();
                }
            }
        }
    }
    peer.ip().to_string()
}

impl AppState {
    fn too_many_failures(&self, keys: &[String]) -> bool {
        let mut f = self.failures.lock();
        let now = Instant::now();
        f.retain(|_, (_, t)| now.duration_since(*t) < FAIL_WINDOW);
        keys.iter().any(|k| f.get(k).map(|(n, _)| *n >= FAIL_LIMIT).unwrap_or(false))
    }

    fn record_failure(&self, keys: &[String]) {
        let mut f = self.failures.lock();
        let now = Instant::now();
        for k in keys {
            let e = f.entry(k.clone()).or_insert((0, now));
            e.0 += 1;
        }
    }

    fn clear_failures(&self, keys: &[String]) {
        let mut f = self.failures.lock();
        for k in keys {
            f.remove(k);
        }
    }

    /// Deterministic fake KDF salt for unknown emails, so `prelogin` does not
    /// reveal which emails have accounts.
    fn fake_salt(&self, email: &str) -> String {
        let mut mac = hmac::Hmac::<Sha256>::new_from_slice(&self.secret).expect("hmac key");
        mac.update(email.as_bytes());
        B64.encode(&mac.finalize().into_bytes()[..16])
    }

    fn session_user(&self, headers: &HeaderMap) -> Res<i64> {
        let token = headers
            .get("authorization")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.strip_prefix("Bearer "))
            .ok_or_else(|| err(StatusCode::UNAUTHORIZED, "not signed in"))?;
        let db = self.db.lock();
        let row: Option<(i64, i64)> = db
            .query_row(
                "SELECT user_id, expires_at FROM sessions WHERE token_hash = ?1",
                params![sha256_hex(token.trim().as_bytes())],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;
        match row {
            Some((uid, exp)) if exp > now_ms() => Ok(uid),
            _ => Err(err(StatusCode::UNAUTHORIZED, "session expired, please sign in again")),
        }
    }

    fn new_session(&self, db: &Connection, uid: i64, device: &str) -> Res<String> {
        let raw: [u8; 32] = rand::random();
        let token = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(raw);
        let now = now_ms();
        db.execute(
            "INSERT INTO sessions (token_hash, user_id, device, created_at, expires_at) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                sha256_hex(token.as_bytes()),
                uid,
                device.chars().take(80).collect::<String>(),
                now,
                now + SESSION_DAYS * 86_400_000
            ],
        )?;
        Ok(token)
    }
}

fn hash_auth_key(auth_key: &[u8]) -> Res<String> {
    Argon2::default()
        .hash_password(auth_key)
        .map(|h| h.to_string())
        .map_err(|_| err(StatusCode::INTERNAL_SERVER_ERROR, "hashing failed"))
}

fn verify_auth_key(auth_key: &[u8], hash: &str) -> bool {
    Argon2::default().verify_password(auth_key, hash).is_ok()
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Kdf {
    pub alg: String,
    pub m_kib: u32,
    pub t: u32,
    pub p: u32,
}

impl Kdf {
    fn validate(&self) -> Res<()> {
        if self.alg != "argon2id"
            || self.m_kib < 1024
            || self.m_kib > 1024 * 1024
            || self.t == 0
            || self.t > 20
            || self.p == 0
            || self.p > 8
        {
            return Err(err(StatusCode::BAD_REQUEST, "unsupported KDF parameters"));
        }
        Ok(())
    }

    fn default_client() -> Self {
        Kdf { alg: "argon2id".into(), m_kib: 65536, t: 3, p: 1 }
    }
}

// ---- account -----------------------------------------------------------------------

#[derive(Deserialize)]
struct PreloginReq {
    email: String,
}

async fn prelogin(State(s): State<Arc<AppState>>, Json(req): Json<PreloginReq>) -> Res<Json<Value>> {
    let email = normalize_email(&req.email)?;
    let row: Option<(String, String)> =
        s.db.lock()
            .query_row("SELECT salt, kdf FROM users WHERE email = ?1", params![email], |r| Ok((r.get(0)?, r.get(1)?)))
            .optional()?;
    let (salt, kdf) = match row {
        Some((salt, kdf)) => (salt, serde_json::from_str::<Kdf>(&kdf).unwrap_or_else(|_| Kdf::default_client())),
        None => (s.fake_salt(&email), Kdf::default_client()),
    };
    Ok(Json(json!({"salt": salt, "kdf": kdf})))
}

#[derive(Deserialize)]
struct SignupReq {
    email: String,
    auth_key: String,
    salt: String,
    kdf: Kdf,
    wrapped_key: String,
    #[serde(default)]
    signup_code: String,
    #[serde(default)]
    device: String,
}

async fn signup(
    State(s): State<Arc<AppState>>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Json(req): Json<SignupReq>,
) -> Res<Json<Value>> {
    let ip_key = format!("signup-ip:{}", client_ip(&s, &headers, peer));
    if s.too_many_failures(std::slice::from_ref(&ip_key)) {
        return Err(err(StatusCode::TOO_MANY_REQUESTS, "too many attempts, try again later"));
    }
    let email = normalize_email(&req.email)?;
    let auth_key = b64_exact(&req.auth_key, 32, "auth_key")?;
    b64_exact(&req.salt, 16, "salt")?;
    req.kdf.validate()?;
    let wrapped = B64.decode(req.wrapped_key.trim()).map_err(|_| err(StatusCode::BAD_REQUEST, "wrapped_key is not base64"))?;
    if wrapped.len() < 40 || wrapped.len() > 256 {
        return Err(err(StatusCode::BAD_REQUEST, "wrapped_key has an unexpected size"));
    }
    {
        let db = s.db.lock();
        let users: i64 = db.query_row("SELECT COUNT(*) FROM users", [], |r| r.get(0))?;
        match &s.cfg.signup_code {
            Some(code) if !code.is_empty() => {
                if !constant_eq(code.as_bytes(), req.signup_code.trim().as_bytes()) {
                    drop(db);
                    s.record_failure(std::slice::from_ref(&ip_key));
                    return Err(err(StatusCode::FORBIDDEN, "wrong sign-up code"));
                }
            }
            _ if users > 0 => {
                return Err(err(
                    StatusCode::FORBIDDEN,
                    "sign-ups are closed on this server (the owner can set SIGNUP_CODE to invite others)",
                ))
            }
            _ => {}
        }
        let exists: Option<i64> =
            db.query_row("SELECT id FROM users WHERE email = ?1", params![email], |r| r.get(0)).optional()?;
        if exists.is_some() {
            return Err(err(StatusCode::CONFLICT, "an account with this email already exists"));
        }
    }
    let auth_hash = hash_auth_key(&auth_key)?;
    let db = s.db.lock();
    db.execute(
        "INSERT INTO users (email, salt, kdf, auth_hash, wrapped_key, created_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![email, req.salt.trim(), serde_json::to_string(&req.kdf).unwrap(), auth_hash, req.wrapped_key.trim(), now_ms()],
    )?;
    let uid = db.last_insert_rowid();
    let token = s.new_session(&db, uid, &req.device)?;
    tracing::info!("account created");
    Ok(Json(json!({"token": token, "email": email})))
}

fn constant_eq(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

#[derive(Deserialize)]
struct LoginReq {
    email: String,
    auth_key: String,
    #[serde(default)]
    device: String,
}

async fn login(
    State(s): State<Arc<AppState>>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Json(req): Json<LoginReq>,
) -> Res<Json<Value>> {
    let email = normalize_email(&req.email)?;
    let keys = vec![format!("ip:{}", client_ip(&s, &headers, peer)), format!("email:{email}")];
    if s.too_many_failures(&keys) {
        return Err(err(StatusCode::TOO_MANY_REQUESTS, "too many failed sign-in attempts, try again in 15 minutes"));
    }
    let auth_key = b64_exact(&req.auth_key, 32, "auth_key")?;
    let row: Option<(i64, String, String, String, String)> =
        s.db.lock()
            .query_row("SELECT id, auth_hash, wrapped_key, salt, kdf FROM users WHERE email = ?1", params![email], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?))
            })
            .optional()?;
    let ok = match &row {
        Some((_, hash, ..)) => verify_auth_key(&auth_key, hash),
        None => {
            // same work as a real check, so timing does not reveal unknown emails
            let _ = hash_auth_key(&auth_key);
            false
        }
    };
    if !ok {
        s.record_failure(&keys);
        return Err(err(StatusCode::UNAUTHORIZED, "wrong email or password"));
    }
    s.clear_failures(&keys);
    let (uid, _, wrapped, salt, kdf) = row.unwrap();
    let db = s.db.lock();
    let token = s.new_session(&db, uid, &req.device)?;
    db.execute("DELETE FROM sessions WHERE expires_at < ?1", params![now_ms()])?;
    Ok(Json(
        json!({"token": token, "email": email, "wrapped_key": wrapped, "salt": salt, "kdf": serde_json::from_str::<Value>(&kdf).unwrap_or(Value::Null)}),
    ))
}

async fn logout(State(s): State<Arc<AppState>>, headers: HeaderMap) -> Res<Json<Value>> {
    s.session_user(&headers)?;
    let token = headers.get("authorization").and_then(|v| v.to_str().ok()).and_then(|v| v.strip_prefix("Bearer ")).unwrap_or("");
    s.db.lock().execute("DELETE FROM sessions WHERE token_hash = ?1", params![sha256_hex(token.trim().as_bytes())])?;
    Ok(Json(json!({"ok": true})))
}

async fn account(State(s): State<Arc<AppState>>, headers: HeaderMap) -> Res<Json<Value>> {
    let uid = s.session_user(&headers)?;
    let db = s.db.lock();
    let (email, bytes, seq, created): (String, i64, i64, i64) =
        db.query_row("SELECT email, bytes, seq, created_at FROM users WHERE id = ?1", params![uid], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
        })?;
    let records: i64 = db.query_row("SELECT COUNT(*) FROM records WHERE user_id = ?1", params![uid], |r| r.get(0))?;
    let sessions: i64 =
        db.query_row("SELECT COUNT(*) FROM sessions WHERE user_id = ?1 AND expires_at > ?2", params![uid, now_ms()], |r| {
            r.get(0)
        })?;
    Ok(Json(json!({
        "email": email, "created_at": created, "records": records, "bytes": bytes, "seq": seq,
        "quota_bytes": s.cfg.quota_bytes, "devices_signed_in": sessions,
    })))
}

#[derive(Deserialize)]
struct DeleteReq {
    auth_key: String,
}

async fn delete_account(State(s): State<Arc<AppState>>, headers: HeaderMap, Json(req): Json<DeleteReq>) -> Res<Json<Value>> {
    let uid = s.session_user(&headers)?;
    let auth_key = b64_exact(&req.auth_key, 32, "auth_key")?;
    let hash: String = s.db.lock().query_row("SELECT auth_hash FROM users WHERE id = ?1", params![uid], |r| r.get(0))?;
    if !verify_auth_key(&auth_key, &hash) {
        return Err(err(StatusCode::UNAUTHORIZED, "wrong password"));
    }
    let db = s.db.lock();
    db.execute("DELETE FROM records WHERE user_id = ?1", params![uid])?;
    db.execute("DELETE FROM sessions WHERE user_id = ?1", params![uid])?;
    db.execute("DELETE FROM users WHERE id = ?1", params![uid])?;
    let _ = db.execute_batch("PRAGMA wal_checkpoint(TRUNCATE);");
    tracing::info!("account deleted");
    Ok(Json(json!({"ok": true})))
}

// ---- sync --------------------------------------------------------------------------

#[derive(Deserialize)]
struct PushRecord {
    rid: String,
    blob: String,
}

#[derive(Deserialize)]
struct PushReq {
    records: Vec<PushRecord>,
}

fn valid_rid(r: &str) -> bool {
    !r.is_empty() && r.len() <= 128 && r.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

async fn push(State(s): State<Arc<AppState>>, headers: HeaderMap, Json(req): Json<PushReq>) -> Res<Json<Value>> {
    let uid = s.session_user(&headers)?;
    if req.records.len() > MAX_BATCH {
        return Err(err(StatusCode::PAYLOAD_TOO_LARGE, format!("at most {MAX_BATCH} records per push")));
    }
    let mut decoded = Vec::with_capacity(req.records.len());
    for r in &req.records {
        if !valid_rid(&r.rid) {
            return Err(err(StatusCode::BAD_REQUEST, "invalid record id"));
        }
        let blob = B64.decode(r.blob.trim()).map_err(|_| err(StatusCode::BAD_REQUEST, "record is not base64"))?;
        if blob.len() > MAX_RECORD_BYTES {
            return Err(err(StatusCode::PAYLOAD_TOO_LARGE, "record too large"));
        }
        decoded.push((r.rid.as_str(), blob));
    }
    let mut db = s.db.lock();
    let tx = db.transaction()?;
    let (mut seq, mut bytes): (i64, i64) =
        tx.query_row("SELECT seq, bytes FROM users WHERE id = ?1", params![uid], |r| Ok((r.get(0)?, r.get(1)?)))?;
    let first = seq + 1;
    for (rid, blob) in &decoded {
        let old: Option<i64> = tx
            .query_row("SELECT length(blob) FROM records WHERE user_id = ?1 AND rid = ?2", params![uid, rid], |r| r.get(0))
            .optional()?;
        bytes += blob.len() as i64 - old.unwrap_or(0);
        if bytes > s.cfg.quota_bytes {
            return Err(err(StatusCode::INSUFFICIENT_STORAGE, "storage quota exceeded"));
        }
        seq += 1;
        tx.execute(
            "INSERT INTO records (user_id, rid, seq, blob) VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(user_id, rid) DO UPDATE SET seq = excluded.seq, blob = excluded.blob",
            params![uid, rid, seq, blob],
        )?;
    }
    tx.execute("UPDATE users SET seq = ?2, bytes = ?3 WHERE id = ?1", params![uid, seq, bytes])?;
    tx.commit()?;
    Ok(Json(json!({"accepted": decoded.len(), "first_seq": first, "latest_seq": seq})))
}

#[derive(Deserialize)]
struct PullQuery {
    #[serde(default)]
    since: i64,
    limit: Option<i64>,
}

async fn pull(State(s): State<Arc<AppState>>, headers: HeaderMap, Query(q): Query<PullQuery>) -> Res<Json<Value>> {
    let uid = s.session_user(&headers)?;
    let limit = q.limit.unwrap_or(500).clamp(1, 1000);
    let db = s.db.lock();
    let latest: i64 = db.query_row("SELECT seq FROM users WHERE id = ?1", params![uid], |r| r.get(0))?;
    let mut stmt = db.prepare("SELECT rid, seq, blob FROM records WHERE user_id = ?1 AND seq > ?2 ORDER BY seq LIMIT ?3")?;
    let rows: Vec<Value> = stmt
        .query_map(params![uid, q.since, limit], |r| {
            let blob: Vec<u8> = r.get(2)?;
            Ok(json!({"rid": r.get::<_, String>(0)?, "seq": r.get::<_, i64>(1)?, "blob": B64.encode(blob)}))
        })?
        .collect::<Result<_, _>>()?;
    let more = rows.len() as i64 == limit;
    Ok(Json(json!({"records": rows, "latest_seq": latest, "more": more})))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn emails() {
        assert_eq!(normalize_email(" A@B.com ").ok().unwrap(), "a@b.com");
        assert!(normalize_email("nope").is_err());
        assert!(normalize_email("a@b").is_err());
    }

    #[test]
    fn rids() {
        assert!(valid_rid("abcDEF0123-_"));
        assert!(!valid_rid(""));
        assert!(!valid_rid("../etc"));
    }
}
