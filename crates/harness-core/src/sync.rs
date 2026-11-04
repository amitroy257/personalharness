//! End-to-end encrypted sync between your devices (laptop, phone, ...).
//!
//! Keys (all derived and used on the device; the server never sees them):
//!
//! ```text
//! password ──Argon2id(salt, 64 MiB)──► master
//!   master ──HKDF "ph-auth-v1"──► auth_key   (sent to the server, which stores only an Argon2 hash of it)
//!   master ──HKDF "ph-enc-v1" ──► wrap_key   (never leaves the device)
//! data_key = random 32 bytes, stored on the server only as XChaCha20-Poly1305(wrap_key, data_key)
//! record   = XChaCha20-Poly1305(data_key, nonce, json, aad = record id)
//! record id = HMAC-SHA256(data_key, kind:id)  (the server cannot even tell chats from settings)
//! ```
//!
//! Conflicts resolve per record by "last edit wins" on `updated_at`;
//! deletions travel as tombstones. Forgetting the password makes the synced
//! copy unrecoverable by design (local data on each device is unaffected).

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use base64::Engine as _;
use chacha20poly1305::aead::{Aead, KeyInit, Payload};
use chacha20poly1305::{XChaCha20Poly1305, XNonce};
use hmac::Mac;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::Sha256;

use crate::harness::Harness;
use crate::store::{Conversation, Message, K_CONV, K_MSG, K_SET};
use crate::util::now_ms;

const B64: base64::engine::GeneralPurpose = base64::engine::general_purpose::STANDARD;
const BATCH: i64 = 200;

// ---- crypto ------------------------------------------------------------------------

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Kdf {
    pub alg: String,
    pub m_kib: u32,
    pub t: u32,
    pub p: u32,
}

impl Default for Kdf {
    fn default() -> Self {
        // OWASP-recommended class of parameters; ~0.5 s on a laptop, ~1 s on a phone
        Kdf { alg: "argon2id".into(), m_kib: 64 * 1024, t: 3, p: 1 }
    }
}

pub struct Keys {
    pub auth_key: [u8; 32],
    pub wrap_key: [u8; 32],
}

pub fn derive_keys(password: &str, salt: &[u8], kdf: &Kdf) -> Result<Keys, String> {
    if kdf.alg != "argon2id" {
        return Err(format!("unsupported key derivation {}", kdf.alg));
    }
    let params = argon2::Params::new(kdf.m_kib, kdf.t, kdf.p, Some(32)).map_err(|e| e.to_string())?;
    let a = argon2::Argon2::new(argon2::Algorithm::Argon2id, argon2::Version::V0x13, params);
    let mut master = [0u8; 32];
    a.hash_password_into(password.as_bytes(), salt, &mut master).map_err(|e| e.to_string())?;
    let hk = hkdf::Hkdf::<Sha256>::new(None, &master);
    let mut auth_key = [0u8; 32];
    let mut wrap_key = [0u8; 32];
    hk.expand(b"ph-auth-v1", &mut auth_key).map_err(|e| e.to_string())?;
    hk.expand(b"ph-enc-v1", &mut wrap_key).map_err(|e| e.to_string())?;
    Ok(Keys { auth_key, wrap_key })
}

fn aead_seal(key: &[u8; 32], aad: &[u8], plaintext: &[u8]) -> Vec<u8> {
    let cipher = XChaCha20Poly1305::new_from_slice(key).expect("32-byte key");
    let nonce_bytes: [u8; 24] = rand::random();
    let nonce = XNonce::from(nonce_bytes);
    let ct = cipher.encrypt(&nonce, Payload { msg: plaintext, aad }).expect("encryption cannot fail");
    let mut out = nonce_bytes.to_vec();
    out.extend_from_slice(&ct);
    out
}

fn aead_open(key: &[u8; 32], aad: &[u8], blob: &[u8]) -> Result<Vec<u8>, String> {
    if blob.len() < 24 + 16 {
        return Err("record too short".into());
    }
    let cipher = XChaCha20Poly1305::new_from_slice(key).expect("32-byte key");
    let mut n = [0u8; 24];
    n.copy_from_slice(&blob[..24]);
    cipher
        .decrypt(&XNonce::from(n), Payload { msg: &blob[24..], aad })
        .map_err(|_| "could not decrypt (wrong key or tampered data)".to_string())
}

pub fn wrap_data_key(wrap_key: &[u8; 32], data_key: &[u8; 32]) -> String {
    B64.encode(aead_seal(wrap_key, b"ph-data-key-v1", data_key))
}

pub fn unwrap_data_key(wrap_key: &[u8; 32], wrapped: &str) -> Result<[u8; 32], String> {
    let blob = B64.decode(wrapped.trim()).map_err(|_| "wrapped key is not base64".to_string())?;
    let k = aead_open(wrap_key, b"ph-data-key-v1", &blob)?;
    k.try_into().map_err(|_| "wrapped key has the wrong length".to_string())
}

pub fn record_id(data_key: &[u8; 32], kind: &str, id: &str) -> String {
    let mut mac = <hmac::Hmac<Sha256> as hmac::KeyInit>::new_from_slice(data_key).expect("hmac key");
    mac.update(b"ph-rid-v1:");
    mac.update(kind.as_bytes());
    mac.update(b":");
    mac.update(id.as_bytes());
    mac.finalize().into_bytes()[..20].iter().map(|b| format!("{b:02x}")).collect()
}

pub fn seal_record(data_key: &[u8; 32], rid: &str, payload: &RecordPayload) -> String {
    B64.encode(aead_seal(data_key, rid.as_bytes(), &serde_json::to_vec(payload).expect("json")))
}

pub fn open_record(data_key: &[u8; 32], rid: &str, blob: &str) -> Result<RecordPayload, String> {
    let raw = B64.decode(blob.trim()).map_err(|_| "record is not base64".to_string())?;
    let plain = aead_open(data_key, rid.as_bytes(), &raw)?;
    serde_json::from_slice(&plain).map_err(|e| format!("bad record: {e}"))
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RecordPayload {
    pub v: u8,
    pub kind: String,
    pub id: String,
    pub updated_at: i64,
    pub deleted: bool,
    #[serde(default)]
    pub data: Value,
}

// ---- local sync state (sync.json, secrets protected like settings) -------------------

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct SyncState {
    pub server: String,
    pub email: String,
    pub token: String,
    pub data_key: String,
    pub last_seq: i64,
    pub last_sync_at: i64,
    pub last_error: Option<String>,
    pub device: String,
}

impl SyncState {
    pub fn signed_in(&self) -> bool {
        !self.token.is_empty() && !self.data_key.is_empty()
    }

    fn key(&self) -> Result<[u8; 32], String> {
        let v = B64.decode(&self.data_key).map_err(|_| "corrupt sync key".to_string())?;
        v.try_into().map_err(|_| "corrupt sync key".to_string())
    }

    pub fn load(path: &std::path::Path) -> Self {
        let mut s: SyncState = std::fs::read(path).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default();
        s.token = crate::secret::unprotect(&s.token);
        s.data_key = crate::secret::unprotect(&s.data_key);
        s
    }

    pub fn save(&self, path: &std::path::Path) -> Result<(), String> {
        let mut disk = self.clone();
        disk.token = crate::secret::protect(&disk.token);
        disk.data_key = crate::secret::protect(&disk.data_key);
        crate::util::write_private(path, &serde_json::to_vec_pretty(&disk).unwrap()).map_err(|e| e.to_string())
    }
}

/// Only HTTPS, except for local/private network addresses (home server, LAN, Tailscale).
pub fn check_server_url(url: &str) -> Result<String, String> {
    let u = url.trim().trim_end_matches('/').to_string();
    let (scheme, rest) = u.split_once("://").ok_or("server URL must start with https://")?;
    let host = rest.split(['/', ':']).next().unwrap_or("").to_ascii_lowercase();
    if host.is_empty() {
        return Err("server URL has no host".into());
    }
    match scheme {
        "https" => Ok(u),
        "http" => {
            let private = host == "localhost"
                || host.ends_with(".local")
                || host.parse::<std::net::IpAddr>().map(|ip| match ip {
                    std::net::IpAddr::V4(v4) => {
                        v4.is_loopback() || v4.is_private() || (v4.octets()[0] == 100 && (64..128).contains(&v4.octets()[1]))
                    }
                    std::net::IpAddr::V6(v6) => v6.is_loopback(),
                }) == Ok(true);
            if private {
                Ok(u)
            } else {
                Err("use https:// for servers on the internet (plain http is only allowed on your local network)".into())
            }
        }
        _ => Err("server URL must start with https://".into()),
    }
}

// ---- server API --------------------------------------------------------------------

async fn api(
    h: &Harness,
    method: reqwest::Method,
    server: &str,
    path: &str,
    token: Option<&str>,
    body: Option<Value>,
) -> Result<Value, String> {
    let mut req = h
        .http
        .request(method, format!("{server}{path}"))
        .header("User-Agent", crate::catalog::USER_AGENT)
        .timeout(Duration::from_secs(60));
    if let Some(t) = token {
        req = req.bearer_auth(t);
    }
    if let Some(b) = body {
        req = req.json(&b);
    }
    let resp = req.send().await.map_err(|e| format!("cannot reach the sync server: {e}"))?;
    let status = resp.status();
    let v: Value = resp.json().await.unwrap_or(Value::Null);
    if !status.is_success() {
        let msg =
            v.get("error").and_then(|e| e.as_str()).map(String::from).unwrap_or_else(|| format!("HTTP {}", status.as_u16()));
        return Err(msg);
    }
    Ok(v)
}

fn device_name() -> String {
    let host = std::env::var("COMPUTERNAME").or_else(|_| std::env::var("HOSTNAME")).unwrap_or_default();
    let os = match std::env::consts::OS {
        "macos" => "Mac",
        "windows" => "Windows PC",
        "android" => "Android",
        "ios" => "iPhone",
        other => other,
    };
    if host.is_empty() {
        os.to_string()
    } else {
        format!("{os} ({host})")
    }
}

// ---- account flows -----------------------------------------------------------------

pub async fn signup(h: &Arc<Harness>, server: &str, email: &str, password: &str, code: &str) -> Result<Value, String> {
    let server = check_server_url(server)?;
    if password.chars().count() < 10 {
        return Err("use a password of at least 10 characters (it protects all your synced data)".into());
    }
    let salt: [u8; 16] = rand::random();
    let kdf = h.sync_kdf();
    let keys = derive_keys(password, &salt, &kdf)?;
    let data_key: [u8; 32] = rand::random();
    let device = device_name();
    let v = api(
        h,
        reqwest::Method::POST,
        &server,
        "/v1/account/signup",
        None,
        Some(json!({
            "email": email.trim(),
            "auth_key": B64.encode(keys.auth_key),
            "salt": B64.encode(salt),
            "kdf": kdf,
            "wrapped_key": wrap_data_key(&keys.wrap_key, &data_key),
            "signup_code": code.trim(),
            "device": device,
        })),
    )
    .await?;
    let st = SyncState {
        server,
        email: v["email"].as_str().unwrap_or(email).to_string(),
        token: v["token"].as_str().unwrap_or_default().to_string(),
        data_key: B64.encode(data_key),
        device,
        ..Default::default()
    };
    start_session(h, st).await
}

pub async fn login(h: &Arc<Harness>, server: &str, email: &str, password: &str) -> Result<Value, String> {
    let server = check_server_url(server)?;
    let pre = api(h, reqwest::Method::POST, &server, "/v1/account/prelogin", None, Some(json!({"email": email.trim()}))).await?;
    let salt = B64.decode(pre["salt"].as_str().unwrap_or_default()).map_err(|_| "server sent a bad salt".to_string())?;
    let kdf: Kdf = serde_json::from_value(pre["kdf"].clone()).map_err(|_| "server sent bad KDF parameters".to_string())?;
    if kdf.m_kib < 8 * 1024 || kdf.t == 0 {
        // a malicious server could weaken the KDF to speed up guessing
        return Err("server asked for unsafe key-derivation settings".into());
    }
    let keys = derive_keys(password, &salt, &kdf)?;
    let device = device_name();
    let v = api(
        h,
        reqwest::Method::POST,
        &server,
        "/v1/account/login",
        None,
        Some(json!({"email": email.trim(), "auth_key": B64.encode(keys.auth_key), "device": device})),
    )
    .await?;
    let data_key = unwrap_data_key(&keys.wrap_key, v["wrapped_key"].as_str().unwrap_or_default())?;
    let st = SyncState {
        server,
        email: v["email"].as_str().unwrap_or(email).to_string(),
        token: v["token"].as_str().unwrap_or_default().to_string(),
        data_key: B64.encode(data_key),
        device,
        ..Default::default()
    };
    start_session(h, st).await
}

async fn start_session(h: &Arc<Harness>, st: SyncState) -> Result<Value, String> {
    h.set_sync_state(Some(st))?;
    h.store.set_tracking(true);
    // merge what this device already has into the account
    h.store.mark_all_dirty();
    h.mark_all_settings_dirty();
    sync_now(h).await?;
    Ok(status(h))
}

pub async fn logout(h: &Arc<Harness>) -> Result<Value, String> {
    if let Some(st) = h.sync_state() {
        let _ = api(h, reqwest::Method::POST, &st.server, "/v1/account/logout", Some(&st.token), Some(json!({}))).await;
    }
    h.store.set_tracking(false);
    h.store.clear_sync_state();
    h.set_sync_state(None)?;
    Ok(status(h))
}

pub async fn delete_account(h: &Arc<Harness>, password: &str) -> Result<Value, String> {
    let st = h.sync_state().ok_or("not signed in")?;
    let pre = api(h, reqwest::Method::POST, &st.server, "/v1/account/prelogin", None, Some(json!({"email": st.email}))).await?;
    let salt = B64.decode(pre["salt"].as_str().unwrap_or_default()).map_err(|_| "bad salt".to_string())?;
    let kdf: Kdf = serde_json::from_value(pre["kdf"].clone()).map_err(|_| "bad KDF".to_string())?;
    let keys = derive_keys(password, &salt, &kdf)?;
    api(
        h,
        reqwest::Method::DELETE,
        &st.server,
        "/v1/account",
        Some(&st.token),
        Some(json!({"auth_key": B64.encode(keys.auth_key)})),
    )
    .await?;
    h.store.set_tracking(false);
    h.store.clear_sync_state();
    h.set_sync_state(None)?;
    Ok(status(h))
}

pub fn status(h: &Harness) -> Value {
    match h.sync_state() {
        Some(st) => json!({
            "signed_in": true,
            "email": st.email,
            "server": st.server,
            "device": st.device,
            "last_sync_at": st.last_sync_at,
            "last_error": st.last_error,
            "pending_changes": h.store.dirty_count(),
        }),
        None => json!({"signed_in": false}),
    }
}

// ---- sync cycle ----------------------------------------------------------------------

#[derive(Debug, Default, Serialize)]
pub struct Report {
    pub pulled: usize,
    pub applied: usize,
    pub pushed: usize,
}

pub async fn sync_now(h: &Arc<Harness>) -> Result<Report, String> {
    let _guard = h.sync_lock.lock().await;
    let result = sync_inner(h).await;
    h.refresh_synced_catalogs().await;
    if let Some(mut st) = h.sync_state() {
        match &result {
            Ok(_) => {
                st.last_sync_at = now_ms();
                st.last_error = None;
            }
            Err(e) => st.last_error = Some(e.clone()),
        }
        let _ = h.set_sync_state(Some(st));
    }
    result
}

async fn sync_inner(h: &Arc<Harness>) -> Result<Report, String> {
    let mut st = h.sync_state().ok_or("not signed in")?;
    let key = st.key()?;
    let mut report = Report::default();

    // 1. pull everything new from the server
    loop {
        let v = api(
            h,
            reqwest::Method::GET,
            &st.server,
            &format!("/v1/sync/pull?since={}&limit=500", st.last_seq),
            Some(&st.token),
            None,
        )
        .await?;
        let recs = v["records"].as_array().cloned().unwrap_or_default();
        let mut payloads = Vec::new();
        let mut max_seq = st.last_seq;
        for r in &recs {
            let rid = r["rid"].as_str().unwrap_or_default();
            max_seq = max_seq.max(r["seq"].as_i64().unwrap_or(0));
            match open_record(&key, rid, r["blob"].as_str().unwrap_or_default()) {
                // the id inside must hash to the outer id: blocks a server swapping records around
                Ok(p) if record_id(&key, &p.kind, &p.id) == rid => payloads.push(p),
                Ok(_) => tracing::warn!("sync: record id mismatch, ignored"),
                Err(e) => tracing::warn!("sync: skipped undecryptable record: {e}"),
            }
        }
        report.pulled += recs.len();
        // settings, then conversations, then messages
        payloads.sort_by_key(|p| match p.kind.as_str() {
            K_SET => 0,
            K_CONV => 1,
            _ => 2,
        });
        for p in payloads {
            if apply(h, &p) {
                report.applied += 1;
            }
        }
        st.last_seq = max_seq;
        h.set_sync_state(Some(st.clone()))?;
        if !v["more"].as_bool().unwrap_or(false) {
            break;
        }
    }

    // 2. push local changes
    loop {
        let dirty = h.store.take_dirty(BATCH);
        if dirty.is_empty() {
            break;
        }
        let mut records = Vec::with_capacity(dirty.len());
        for (kind, id, _) in &dirty {
            if let Some(p) = local_payload(h, kind, id) {
                let rid = record_id(&key, kind, id);
                records.push(json!({"rid": rid, "blob": seal_record(&key, &rid, &p)}));
            }
        }
        if !records.is_empty() {
            let n = records.len() as i64;
            let v =
                api(h, reqwest::Method::POST, &st.server, "/v1/sync/push", Some(&st.token), Some(json!({"records": records})))
                    .await?;
            report.pushed += n as usize;
            // skip re-downloading our own records when nobody else pushed in between
            if v["first_seq"].as_i64() == Some(st.last_seq + 1) {
                st.last_seq = v["latest_seq"].as_i64().unwrap_or(st.last_seq);
                h.set_sync_state(Some(st.clone()))?;
            }
        }
        h.store.clear_dirty(&dirty);
        if (dirty.len() as i64) < BATCH {
            break;
        }
    }
    Ok(report)
}

/// The current local version of a record (or a tombstone).
fn local_payload(h: &Harness, kind: &str, id: &str) -> Option<RecordPayload> {
    let tomb =
        |at: i64| RecordPayload { v: 1, kind: kind.into(), id: id.into(), updated_at: at, deleted: true, data: Value::Null };
    match kind {
        K_CONV => match h.store.get_conversation(id) {
            Some(c) => Some(RecordPayload {
                v: 1,
                kind: kind.into(),
                id: id.into(),
                updated_at: c.updated_at,
                deleted: false,
                data: json!(c),
            }),
            None => h.store.tombstone_at(kind, id).map(tomb),
        },
        K_MSG => match h.store.get_message(id) {
            Some(m) => Some(RecordPayload {
                v: 1,
                kind: kind.into(),
                id: id.into(),
                updated_at: m.updated_at,
                deleted: false,
                data: json!(m),
            }),
            None => h.store.tombstone_at(kind, id).map(tomb),
        },
        K_SET => match h.settings_section(id) {
            Some((data, at)) => {
                Some(RecordPayload { v: 1, kind: kind.into(), id: id.into(), updated_at: at, deleted: false, data })
            }
            None => h.store.tombstone_at(kind, id).map(tomb),
        },
        _ => None,
    }
}

/// Apply one remote record with last-edit-wins. Returns true if local data changed.
fn apply(h: &Harness, p: &RecordPayload) -> bool {
    if !matches!(p.kind.as_str(), K_CONV | K_MSG | K_SET) {
        return false;
    }
    if let Some(local) = local_payload(h, &p.kind, &p.id) {
        if local.updated_at > p.updated_at {
            // ours is newer: make sure the server ends up with it
            h.store.mark_changed(&p.kind, &p.id);
            return false;
        }
        if local.updated_at == p.updated_at {
            if local.deleted == p.deleted && local.data == p.data {
                return false; // identical (typically our own echo)
            }
            // same timestamp, different content: deterministic tie-break so
            // every device converges on the same version
            let rank = |r: &RecordPayload| (r.deleted, serde_json::to_string(&r.data).unwrap_or_default());
            if rank(&local) >= rank(p) {
                h.store.mark_changed(&p.kind, &p.id);
                return false;
            }
        }
        if p.deleted && local.deleted {
            return false;
        }
    }
    if p.deleted {
        match p.kind.as_str() {
            K_SET => {
                h.delete_settings_section_remote(&p.id, p.updated_at);
                h.store.delete_remote(K_SET, &p.id, p.updated_at);
            }
            _ => h.store.delete_remote(&p.kind, &p.id, p.updated_at),
        }
        return true;
    }
    match p.kind.as_str() {
        K_CONV => match serde_json::from_value::<Conversation>(p.data.clone()) {
            Ok(c) if c.id == p.id => h.store.upsert_conversation_remote(&c),
            _ => return false,
        },
        K_MSG => match serde_json::from_value::<Message>(p.data.clone()) {
            Ok(m) if m.id == p.id => h.store.upsert_message_remote(&m),
            _ => return false,
        },
        _ => {
            if !h.apply_settings_section_remote(&p.id, &p.data, p.updated_at) {
                return false;
            }
        }
    }
    true
}

/// Settings split into independently-merged sections. Device-specific parts
/// (local API port and token) are never synced.
pub fn settings_sections(s: &crate::settings::Settings) -> BTreeMap<String, Value> {
    let mut m = BTreeMap::new();
    for (id, p) in &s.providers {
        m.insert(format!("provider:{id}"), json!(p));
    }
    for c in &s.custom_providers {
        m.insert(format!("custom:{}", c.id), json!(c));
    }
    m.insert("ranking".into(), json!(s.ranking));
    m.insert("chat".into(), json!(s.chat));
    m.insert("routing".into(), json!(s.routing));
    m.insert("hidden".into(), json!(s.hidden_models));
    m
}

/// Background loop: sync after local changes (debounced) and every minute.
pub fn spawn_loop(h: &Arc<Harness>) {
    let h = h.clone();
    tokio::spawn(async move {
        loop {
            tokio::select! {
                _ = h.sync_notify.notified() => tokio::time::sleep(Duration::from_millis(1500)).await,
                _ = tokio::time::sleep(Duration::from_secs(60)) => {}
            }
            if h.sync_state().map(|s| s.signed_in()).unwrap_or(false) {
                if let Err(e) = sync_now(&h).await {
                    tracing::warn!("sync failed: {e}");
                }
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fast() -> Kdf {
        Kdf { alg: "argon2id".into(), m_kib: 8 * 1024, t: 1, p: 1 }
    }

    #[test]
    fn keys_are_deterministic_and_separate() {
        let a = derive_keys("correct horse battery", &[1u8; 16], &fast()).unwrap();
        let b = derive_keys("correct horse battery", &[1u8; 16], &fast()).unwrap();
        let c = derive_keys("correct horse battery!", &[1u8; 16], &fast()).unwrap();
        assert_eq!(a.auth_key, b.auth_key);
        assert_ne!(a.auth_key, a.wrap_key, "login key and encryption key are independent");
        assert_ne!(a.auth_key, c.auth_key);
    }

    #[test]
    fn wrap_and_records_round_trip_and_detect_tampering() {
        let k = derive_keys("pw-123456789", &[2u8; 16], &fast()).unwrap();
        let dk: [u8; 32] = rand::random();
        let w = wrap_data_key(&k.wrap_key, &dk);
        assert_eq!(unwrap_data_key(&k.wrap_key, &w).unwrap(), dk);
        let wrong = derive_keys("wrong-password", &[2u8; 16], &fast()).unwrap();
        assert!(unwrap_data_key(&wrong.wrap_key, &w).is_err());

        let p = RecordPayload {
            v: 1,
            kind: "msg".into(),
            id: "m1".into(),
            updated_at: 5,
            deleted: false,
            data: json!({"content": "secret"}),
        };
        let rid = record_id(&dk, "msg", "m1");
        assert_eq!(rid.len(), 40);
        assert_ne!(rid, record_id(&dk, "conv", "m1"));
        let blob = seal_record(&dk, &rid, &p);
        assert!(!String::from_utf8_lossy(&B64.decode(&blob).unwrap()).contains("secret"));
        assert_eq!(open_record(&dk, &rid, &blob).unwrap().data["content"], "secret");
        // a record moved to another id (by a malicious server) fails authentication
        assert!(open_record(&dk, &record_id(&dk, "msg", "m2"), &blob).is_err());
        let mut raw = B64.decode(&blob).unwrap();
        let last = raw.len() - 1;
        raw[last] ^= 1;
        assert!(open_record(&dk, &rid, &B64.encode(raw)).is_err());
    }

    #[test]
    fn server_urls() {
        assert!(check_server_url("https://sync.example.com/").is_ok());
        assert!(check_server_url("http://192.168.1.20:8787").is_ok());
        assert!(check_server_url("http://localhost:8787").is_ok());
        assert!(check_server_url("http://100.101.1.2:8787").is_ok(), "tailscale");
        assert!(check_server_url("http://sync.example.com").is_err());
        assert!(check_server_url("ftp://x").is_err());
    }
}
