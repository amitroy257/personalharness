//! `harness-sync` — run your own Personal Harness sync server.
//!
//!   harness-sync [--addr 0.0.0.0:8787] [--data ./sync-data]
//!
//! Environment:
//!   SIGNUP_CODE   invite code required to create accounts (if unset, only the
//!                 first account — yours — can be created)
//!   QUOTA_MB      per-account storage quota (default 512)
//!   TRUST_PROXY=1 use Fly-Client-IP / X-Forwarded-For for rate limiting
//!                 (only when running behind a reverse proxy)
//!
//! Put it behind HTTPS (Fly.io, Render, Caddy, nginx...). Everything stored is
//! end-to-end encrypted by the apps, but the login key travels in requests.

use std::net::SocketAddr;
use std::path::PathBuf;

use harness_sync_server::{open, router, Config};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()))
        .init();
    let mut addr: SocketAddr =
        std::env::var("ADDR").ok().and_then(|a| a.parse().ok()).unwrap_or_else(|| "0.0.0.0:8787".parse().unwrap());
    let mut data = std::env::var_os("DATA_DIR").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("sync-data"));
    let mut it = std::env::args().skip(1);
    while let Some(a) = it.next() {
        match a.as_str() {
            "--addr" => addr = it.next().and_then(|v| v.parse().ok()).expect("--addr HOST:PORT"),
            "--data" => data = it.next().map(PathBuf::from).expect("--data DIR"),
            "-h" | "--help" => {
                println!("harness-sync [--addr 0.0.0.0:8787] [--data DIR]  (env: SIGNUP_CODE, QUOTA_MB, TRUST_PROXY)");
                return Ok(());
            }
            other => anyhow::bail!("unknown argument {other}"),
        }
    }
    let cfg = Config {
        data_dir: data.clone(),
        signup_code: std::env::var("SIGNUP_CODE").ok().filter(|s| !s.trim().is_empty()),
        quota_bytes: std::env::var("QUOTA_MB").ok().and_then(|v| v.parse::<i64>().ok()).unwrap_or(512) * 1024 * 1024,
        trust_proxy: std::env::var("TRUST_PROXY").map(|v| v == "1" || v == "true").unwrap_or(false),
    };
    let signup = if cfg.signup_code.is_some() { "with sign-up code" } else { "first account only" };
    let state = open(cfg)?;
    let listener = tokio::net::TcpListener::bind(addr).await?;
    tracing::info!("harness-sync listening on http://{addr} (data: {}, sign-ups: {signup})", data.display());
    axum::serve(listener, router(state).into_make_service_with_connect_info::<SocketAddr>()).await?;
    Ok(())
}
