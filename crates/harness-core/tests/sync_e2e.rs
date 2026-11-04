//! Two devices (laptop + phone) syncing through a real sync server.
//! Also proves the server only ever stores ciphertext.

use std::net::SocketAddr;
use std::path::Path;
use std::sync::Arc;

use harness_core::commands;
use harness_core::settings::ProviderSettings;
use harness_core::sync::{self, Kdf};
use harness_core::Harness;
use serde_json::{json, Value};

const PASSWORD: &str = "correct horse battery staple";

async fn start_server(dir: &Path) -> String {
    let state = harness_sync_server::open(harness_sync_server::Config {
        data_dir: dir.to_path_buf(),
        signup_code: None,
        quota_bytes: 64 << 20,
        trust_proxy: false,
    })
    .unwrap();
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = l.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(l, harness_sync_server::router(state).into_make_service_with_connect_info::<SocketAddr>()).await.unwrap();
    });
    format!("http://{addr}")
}

fn device(dir: &Path) -> Arc<Harness> {
    let h = Harness::open(dir).unwrap();
    // cheaper key derivation for tests (still checked to be >= 8 MiB by clients)
    h.set_sync_kdf(Kdf { alg: "argon2id".into(), m_kib: 8 * 1024, t: 1, p: 1 });
    h
}

async fn sync(h: &Arc<Harness>) -> Value {
    commands::call(h, "sync_now", Value::Null).await.unwrap()
}

fn server_bytes(dir: &Path) -> String {
    let mut all = Vec::new();
    for f in std::fs::read_dir(dir).unwrap() {
        all.extend(std::fs::read(f.unwrap().path()).unwrap_or_default());
    }
    String::from_utf8_lossy(&all).to_string()
}

#[tokio::test]
async fn laptop_and_phone_stay_in_sync_and_server_sees_only_ciphertext() {
    let (sdir, ldir, pdir) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let server = start_server(sdir.path()).await;
    let laptop = device(ldir.path());
    let phone = device(pdir.path());

    // --- the laptop has data before sync is ever enabled ---------------------------
    laptop
        .update_settings(|s| {
            s.providers.insert(
                "groq".into(),
                ProviderSettings { enabled: true, api_key: "gsk-LAPTOP-SECRET-KEY-123".into(), ..Default::default() },
            );
        })
        .unwrap();
    let conv = laptop.store.create_conversation("MARKER-TITLE trip plans", "auto", "");
    laptop.store.add_message(&conv.id, "user", "MARKER-CHAT-a1b2 my private thought", "", "", "", (None, None), None, "");
    let answer = laptop.store.add_message(
        &conv.id,
        "assistant",
        "MARKER-ANSWER sure",
        "",
        "groq/x",
        "x",
        (Some(3), Some(4)),
        Some(10),
        "",
    );

    // --- sign up on the laptop: existing data is uploaded --------------------------
    let st = commands::call(&laptop, "sync_signup", json!({"server": server, "email": "Me@Example.com", "password": PASSWORD}))
        .await
        .unwrap();
    assert_eq!(st["signed_in"], true);
    assert_eq!(st["pending_changes"], 0, "{st}");

    // --- the phone: a wrong password is rejected, the right one restores everything
    let e = commands::call(
        &phone,
        "sync_login",
        json!({"server": server, "email": "me@example.com", "password": "not the password"}),
    )
    .await;
    assert!(e.unwrap_err().contains("wrong email or password"));
    commands::call(&phone, "sync_login", json!({"server": server, "email": "me@example.com", "password": PASSWORD}))
        .await
        .unwrap();
    let convs = phone.store.list_conversations(10);
    assert_eq!(convs.len(), 1);
    assert_eq!(convs[0].title, "MARKER-TITLE trip plans");
    let msgs = phone.store.messages(&conv.id);
    assert_eq!(msgs.len(), 2);
    assert_eq!(msgs[0].content, "MARKER-CHAT-a1b2 my private thought");
    assert_eq!(phone.settings().provider("groq").api_key, "gsk-LAPTOP-SECRET-KEY-123", "API keys sync too");
    assert_ne!(phone.settings().server.token, laptop.settings().server.token, "the local API token stays per device");

    // --- rate on the phone -> the laptop sees it -----------------------------------------
    commands::call(&phone, "rate_message", json!({"id": answer.id, "rating": 1})).await.unwrap();
    sync(&phone).await;
    sync(&laptop).await;
    assert_eq!(laptop.store.get_message(&answer.id).unwrap().rating, 1);

    // --- new chat on the phone -> appears on the laptop ----------------------------------
    let pc = phone.store.create_conversation("from my phone", "auto:fast", "");
    phone.store.add_message(&pc.id, "user", "typed on the bus", "", "", "", (None, None), None, "");
    sync(&phone).await;
    sync(&laptop).await;
    assert_eq!(laptop.store.list_conversations(10).len(), 2);
    assert_eq!(laptop.store.messages(&pc.id)[0].content, "typed on the bus");

    // --- settings changed on the phone -> laptop; device-specific port untouched ---------
    commands::call(&phone, "update_settings", json!({"chat": {"system_prompt": "Answer briefly."}, "server": {"port": 5555}}))
        .await
        .ok();
    sync(&phone).await;
    sync(&laptop).await;
    assert_eq!(laptop.settings().chat.system_prompt, "Answer briefly.");
    assert_ne!(laptop.settings().server.port, 5555);

    // --- delete on the laptop -> gone on the phone ------------------------------------------
    commands::call(&laptop, "delete_conversation", json!({"id": conv.id})).await.unwrap();
    sync(&laptop).await;
    sync(&phone).await;
    assert!(phone.store.get_conversation(&conv.id).is_none());
    assert!(phone.store.get_message(&answer.id).is_none());
    assert_eq!(phone.store.list_conversations(10).len(), 1);

    // --- concurrent edits: last edit wins on both devices --------------------------------
    phone.store.update_conversation(&pc.id, Some("phone title"), None, None);
    tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    laptop.store.update_conversation(&pc.id, Some("laptop title (later)"), None, None);
    sync(&phone).await;
    sync(&laptop).await;
    sync(&phone).await;
    assert_eq!(laptop.store.get_conversation(&pc.id).unwrap().title, "laptop title (later)");
    assert_eq!(phone.store.get_conversation(&pc.id).unwrap().title, "laptop title (later)");

    // --- removing a provider key on one device removes it everywhere ---------------------
    laptop
        .update_settings(|s| {
            s.providers.remove("groq");
        })
        .unwrap();
    sync(&laptop).await;
    sync(&phone).await;
    assert!(!phone.settings().providers.contains_key("groq"));

    // --- zero knowledge: nothing readable on the server ---------------------------------
    let raw = server_bytes(sdir.path());
    for secret in [
        "MARKER-CHAT",
        "MARKER-ANSWER",
        "MARKER-TITLE",
        "typed on the bus",
        "gsk-LAPTOP-SECRET",
        "Answer briefly",
        "from my phone",
        "provider:groq",
        PASSWORD,
    ] {
        assert!(!raw.contains(secret), "server storage contains plaintext: {secret}");
    }
    assert!(raw.contains("me@example.com"), "sanity: the scan does read the database");

    // --- signing out stops sync; deleting the account erases it -------------------------
    let s = commands::call(&phone, "sync_logout", Value::Null).await.unwrap();
    assert_eq!(s["signed_in"], false);
    assert!(!phone.store.tracking());
    assert_eq!(phone.store.list_conversations(10).len(), 1, "local data stays on the device");
    let e = commands::call(&laptop, "sync_delete_account", json!({"password": "wrong password!!"})).await;
    assert!(e.is_err());
    commands::call(&laptop, "sync_delete_account", json!({"password": PASSWORD})).await.unwrap();
    let e =
        commands::call(&phone, "sync_login", json!({"server": server, "email": "me@example.com", "password": PASSWORD})).await;
    assert!(e.is_err(), "account is gone");
    let _ = sync::status(&laptop);
}

#[tokio::test]
async fn sync_state_secrets_are_protected_and_settings_defaults_do_not_override() {
    let (sdir, adir, bdir) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let server = start_server(sdir.path()).await;
    let a = device(adir.path());
    a.update_settings(|s| s.chat.default_model = "auto:coding".into()).unwrap();
    commands::call(&a, "sync_signup", json!({"server": server, "email": "x@example.com", "password": PASSWORD})).await.unwrap();

    // a fresh device with default settings must not overwrite the account's settings
    let b = device(bdir.path());
    commands::call(&b, "sync_login", json!({"server": server, "email": "x@example.com", "password": PASSWORD})).await.unwrap();
    assert_eq!(b.settings().chat.default_model, "auto:coding");
    sync(&a).await;
    assert_eq!(a.settings().chat.default_model, "auto:coding");

    // the sync key and session token on disk: protected on Windows, owner-only elsewhere
    let raw = std::fs::read_to_string(adir.path().join("sync.json")).unwrap();
    if cfg!(windows) {
        assert!(raw.contains("dpapi:"));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(std::fs::metadata(adir.path().join("sync.json")).unwrap().permissions().mode() & 0o777, 0o600);
    }
    // weak passwords are refused before anything is sent
    let c = device(tempfile::tempdir().unwrap().path());
    let e = commands::call(&c, "sync_signup", json!({"server": server, "email": "y@example.com", "password": "short"})).await;
    assert!(e.unwrap_err().contains("at least 10"));
    // plain http to a public host is refused
    let e = commands::call(
        &c,
        "sync_login",
        json!({"server": "http://sync.example.com", "email": "y@example.com", "password": PASSWORD}),
    )
    .await;
    assert!(e.unwrap_err().contains("https"));
}
