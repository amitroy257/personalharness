//! Shared application state used by the Tauri app, the headless server and the
//! local API.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use parking_lot::{Mutex, RwLock};
use serde::Serialize;

use crate::catalog::{self, Endpoint, ModelEntry, ProviderCatalog};
use crate::ranking::{self, RankInput, RankedModel, View};
use crate::settings::Settings;
use crate::signals::{self, SignalUrls, Signals};
use crate::store::Store;
use crate::util::now_ms;

#[derive(Clone, Debug, Serialize)]
pub struct Cooldown {
    pub until_ms: i64,
    pub reason: String,
}

pub struct ServerHandle {
    pub port: u16,
    shutdown: Option<tokio::sync::oneshot::Sender<()>>,
}

impl Drop for ServerHandle {
    fn drop(&mut self) {
        if let Some(tx) = self.shutdown.take() {
            let _ = tx.send(());
        }
    }
}

pub struct Harness {
    pub data_dir: PathBuf,
    settings_path: PathBuf,
    settings: RwLock<Settings>,
    pub store: Store,
    catalogs: RwLock<HashMap<String, ProviderCatalog>>,
    signals: RwLock<Signals>,
    cooldowns: Mutex<HashMap<String, Cooldown>>,
    pub http: reqwest::Client,
    pub signal_urls: SignalUrls,
    server: tokio::sync::Mutex<Option<ServerHandle>>,
    server_error: Mutex<Option<String>>,
    /// Extra routes merged into the local server (headless web UI).
    extra_routes: Mutex<Option<axum::Router>>,
    signals_refresh: tokio::sync::Mutex<()>,
    /// conversation id -> stop flag for in-flight generations
    pub(crate) stops: Mutex<HashMap<String, Arc<std::sync::atomic::AtomicBool>>>,
    /// Usage aggregates for rankings, cached briefly (SQA F3: recomputing them
    /// on every `auto` request cost ~170 ms with 100k usage rows).
    stats_cache: Mutex<Option<(std::time::Instant, UsageStats)>>,
    /// True when running inside the desktop app (vs. the headless server).
    desktop: std::sync::atomic::AtomicBool,
}

type UsageStats = Arc<(HashMap<String, crate::store::FamilyStats>, HashMap<String, (i64, i64, Option<f64>)>)>;
const STATS_TTL: Duration = Duration::from_secs(15);

impl Harness {
    pub fn open(data_dir: &Path) -> anyhow::Result<Arc<Self>> {
        std::fs::create_dir_all(data_dir)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(data_dir, std::fs::Permissions::from_mode(0o700));
        }
        let settings_path = data_dir.join("settings.json");
        let settings = Settings::load(&settings_path)?;
        settings.save(&settings_path)?;
        let store = Store::open(&data_dir.join("harness.db"))?;

        let mut catalogs = HashMap::new();
        for ep in catalog::all_endpoints(&settings) {
            if let Some((json, _)) = store.kv_get(&format!("catalog:{}", ep.provider)) {
                if let Ok(c) = serde_json::from_str::<ProviderCatalog>(&json) {
                    catalogs.insert(ep.provider.clone(), c);
                }
            }
        }
        let signals = store
            .kv_get("signals")
            .and_then(|(json, _)| serde_json::from_str::<Signals>(&json).ok())
            .filter(|s| !s.arena_text.is_empty() || !s.arena_code.is_empty())
            .unwrap_or_else(Signals::baseline);

        let http = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(15))
            .pool_idle_timeout(Duration::from_secs(90))
            .build()?;

        Ok(Arc::new(Self {
            data_dir: data_dir.to_path_buf(),
            settings_path,
            settings: RwLock::new(settings),
            store,
            catalogs: RwLock::new(catalogs),
            signals: RwLock::new(signals),
            cooldowns: Mutex::new(HashMap::new()),
            http,
            signal_urls: SignalUrls::default(),
            server: tokio::sync::Mutex::new(None),
            server_error: Mutex::new(None),
            extra_routes: Mutex::new(None),
            signals_refresh: tokio::sync::Mutex::new(()),
            stops: Mutex::new(HashMap::new()),
            stats_cache: Mutex::new(None),
            desktop: std::sync::atomic::AtomicBool::new(false),
        }))
    }

    /// Called by the desktop app so the privacy self-check knows the OS
    /// sandbox is expected (headless mode is not sandboxed).
    pub fn set_desktop(&self, v: bool) {
        self.desktop.store(v, std::sync::atomic::Ordering::Relaxed);
    }

    pub fn is_desktop(&self) -> bool {
        self.desktop.load(std::sync::atomic::Ordering::Relaxed)
    }

    // ---- settings ------------------------------------------------------------

    pub fn settings(&self) -> Settings {
        self.settings.read().clone()
    }

    pub fn update_settings<R>(&self, f: impl FnOnce(&mut Settings) -> R) -> anyhow::Result<R> {
        let mut s = self.settings.write();
        let r = f(&mut s);
        s.save(&self.settings_path)?;
        Ok(r)
    }

    // ---- catalogs --------------------------------------------------------------

    pub fn endpoint(&self, id: &str) -> Option<Endpoint> {
        catalog::endpoint(&self.settings.read(), id)
    }

    pub fn catalog(&self, provider: &str) -> Option<ProviderCatalog> {
        self.catalogs.read().get(provider).cloned()
    }

    /// Models of usable providers. `include_hidden` for the Models page.
    pub fn entries(&self, include_hidden: bool) -> Vec<ModelEntry> {
        let s = self.settings.read();
        let cats = self.catalogs.read();
        let mut out = Vec::new();
        for ep in catalog::all_endpoints(&s) {
            if !ep.usable() {
                continue;
            }
            if let Some(c) = cats.get(&ep.provider) {
                out.extend(c.models.iter().filter(|m| include_hidden || !s.hidden_models.contains(&m.api_id)).cloned());
            }
        }
        out
    }

    pub async fn refresh_provider(&self, id: &str) -> anyhow::Result<usize> {
        let Some(ep) = self.endpoint(id) else { anyhow::bail!("unknown provider '{id}'") };
        if !ep.usable() {
            self.catalogs.write().remove(id);
            anyhow::bail!("provider '{}' is disabled or missing an API key / account id", ep.name);
        }
        let res = catalog::fetch_models(&self.http, &ep).await;
        let mut cat = self.catalog(id).unwrap_or_else(|| ProviderCatalog { provider: id.to_string(), ..Default::default() });
        cat.fetched_at = now_ms();
        let out = match res {
            Ok(models) => {
                let n = models.len();
                cat.models = models;
                cat.error = None;
                Ok(n)
            }
            Err(e) => {
                cat.error = Some(e.to_string());
                Err(e)
            }
        };
        if let Ok(json) = serde_json::to_string(&cat) {
            self.store.kv_set(&format!("catalog:{id}"), &json);
        }
        self.catalogs.write().insert(id.to_string(), cat);
        out
    }

    pub async fn refresh_all_providers(&self) -> Vec<(String, Result<usize, String>)> {
        let ids: Vec<String> =
            catalog::all_endpoints(&self.settings.read()).into_iter().filter(|e| e.usable()).map(|e| e.provider).collect();
        let futs = ids.iter().map(|id| async move { (id.clone(), self.refresh_provider(id).await.map_err(|e| e.to_string())) });
        futures_util::future::join_all(futs).await
    }

    // ---- signals & rankings ------------------------------------------------------

    pub fn signals(&self) -> Signals {
        self.signals.read().clone()
    }

    pub async fn refresh_signals(&self, force: bool) -> Signals {
        let _guard = self.signals_refresh.lock().await;
        let cfg = self.settings.read().ranking.clone();
        let prev = self.signals();
        let stale = now_ms() - prev.fetched_at > (cfg.refresh_hours.max(1) as i64) * 3_600_000;
        if !force && !stale {
            return prev;
        }
        let fresh = signals::refresh(&self.http, &cfg, &prev, &self.signal_urls).await;
        if let Ok(json) = serde_json::to_string(&fresh) {
            self.store.kv_set("signals", &json);
        }
        *self.signals.write() = fresh.clone();
        fresh
    }

    fn usage_stats(&self) -> UsageStats {
        let mut cache = self.stats_cache.lock();
        if let Some((at, stats)) = cache.as_ref() {
            if at.elapsed() < STATS_TTL {
                return stats.clone();
            }
        }
        let stats: UsageStats = Arc::new((self.store.family_stats(30), self.store.route_stats(30)));
        *cache = Some((std::time::Instant::now(), stats.clone()));
        stats
    }

    /// Drop cached usage aggregates (after ratings or deletions, so the UI
    /// reflects them immediately).
    pub fn invalidate_stats(&self) {
        *self.stats_cache.lock() = None;
    }

    pub fn rankings(&self, view: View) -> Vec<RankedModel> {
        let entries = self.entries(false);
        let signals = self.signals.read();
        let stats = self.usage_stats();
        let (fam, routes) = (&stats.0, &stats.1);
        let cds = self.cooldown_map();
        let s = self.settings.read();
        let providers: HashMap<String, (String, f64)> =
            catalog::all_endpoints(&s).into_iter().map(|e| (e.provider.clone(), (e.name.clone(), e.speed_prior))).collect();
        let input = RankInput {
            entries: &entries,
            signals: &signals,
            family_stats: fam,
            route_stats: routes,
            cooldowns: &cds,
            providers: &providers,
            personal_weight: s.ranking.personal_weight,
            now_ms: now_ms(),
        };
        ranking::rank(&input, view)
    }

    // ---- cooldowns ----------------------------------------------------------------

    pub fn set_cooldown(&self, api_id: &str, dur: Duration, reason: &str) {
        let until = now_ms() + dur.as_millis() as i64;
        self.cooldowns.lock().insert(api_id.to_string(), Cooldown { until_ms: until, reason: reason.to_string() });
    }

    pub fn clear_cooldown(&self, api_id: &str) {
        self.cooldowns.lock().remove(api_id);
    }

    pub fn cooldown_map(&self) -> HashMap<String, i64> {
        let now = now_ms();
        let mut c = self.cooldowns.lock();
        c.retain(|_, v| v.until_ms > now);
        c.iter().map(|(k, v)| (k.clone(), v.until_ms)).collect()
    }

    pub fn cooldowns(&self) -> HashMap<String, Cooldown> {
        let now = now_ms();
        let mut c = self.cooldowns.lock();
        c.retain(|_, v| v.until_ms > now);
        c.clone()
    }

    // ---- local API server ------------------------------------------------------------

    pub fn set_extra_routes(&self, r: axum::Router) {
        *self.extra_routes.lock() = Some(r);
    }

    pub async fn start_server(self: &Arc<Self>) -> anyhow::Result<()> {
        let mut guard = self.server.lock().await;
        *guard = None; // drop -> shuts down any previous instance
        let cfg = self.settings.read().server.clone();
        if !cfg.enabled {
            *self.server_error.lock() = None;
            return Ok(());
        }
        let mut app = crate::server::api_router(self.clone());
        if let Some(extra) = self.extra_routes.lock().clone() {
            app = app.merge(extra);
        }
        let app = crate::server::with_guard(app, self.clone());
        let addr = std::net::SocketAddr::from(([127, 0, 0, 1], cfg.port));
        let listener = match tokio::net::TcpListener::bind(addr).await {
            Ok(l) => l,
            Err(e) => {
                let msg = format!("Could not listen on 127.0.0.1:{}: {e}", cfg.port);
                *self.server_error.lock() = Some(msg.clone());
                anyhow::bail!(msg);
            }
        };
        let (tx, rx) = tokio::sync::oneshot::channel::<()>();
        tokio::spawn(async move {
            let _ = axum::serve(listener, app)
                .with_graceful_shutdown(async move {
                    let _ = rx.await;
                })
                .await;
        });
        *self.server_error.lock() = None;
        *guard = Some(ServerHandle { port: cfg.port, shutdown: Some(tx) });
        tracing::info!("local API listening on http://{addr}/v1");
        Ok(())
    }

    pub async fn server_status(&self) -> serde_json::Value {
        let g = self.server.lock().await;
        let s = self.settings.read();
        serde_json::json!({
            "enabled": s.server.enabled,
            "running": g.is_some(),
            "port": g.as_ref().map(|h| h.port).unwrap_or(s.server.port),
            "base_url": format!("http://127.0.0.1:{}/v1", g.as_ref().map(|h| h.port).unwrap_or(s.server.port)),
            "error": self.server_error.lock().clone(),
        })
    }

    // ---- background work -------------------------------------------------------------

    /// Periodically refresh stale model lists and public signals.
    pub fn spawn_background(self: &Arc<Self>) {
        let h = self.clone();
        tokio::spawn(async move {
            loop {
                let stale_after = 6 * 3_600_000;
                let now = now_ms();
                let ids: Vec<String> = catalog::all_endpoints(&h.settings())
                    .into_iter()
                    .filter(|e| e.usable())
                    .filter(|e| {
                        h.catalog(&e.provider).map(|c| now - c.fetched_at > stale_after || c.models.is_empty()).unwrap_or(true)
                    })
                    .map(|e| e.provider)
                    .collect();
                for id in ids {
                    if let Err(e) = h.refresh_provider(&id).await {
                        tracing::warn!("model refresh for {id} failed: {e}");
                    }
                }
                h.refresh_signals(false).await;
                tokio::time::sleep(Duration::from_secs(30 * 60)).await;
            }
        });
    }

    pub fn privacy_info(&self) -> serde_json::Value {
        let s = self.settings.read();
        let mut dest: Vec<serde_json::Value> = catalog::all_endpoints(&s)
            .into_iter()
            .filter(|e| e.usable())
            .map(|e| serde_json::json!({"host": host_of(&e.base_url), "why": format!("{}: your prompts (only when routed there) and model-list requests", e.name)}))
            .collect();
        if s.ranking.public_data {
            let u = &self.signal_urls;
            if s.ranking.source_arena {
                dest.push(
                    serde_json::json!({"host": host_of(&u.arena_base), "why": "LMArena leaderboard snapshots (anonymous GET)"}),
                );
            }
            if s.ranking.source_huggingface {
                dest.push(serde_json::json!({"host": host_of(&u.hf_api), "why": "Trending/download counts (anonymous GET)"}));
            }
            if s.ranking.source_openrouter_models || !s.ranking.openrouter_key.is_empty() {
                dest.push(serde_json::json!({"host": host_of(&u.or_models), "why": "Model release dates; token rankings if you added a key (GET)"}));
            }
            if !s.ranking.artificial_analysis_key.is_empty() {
                dest.push(serde_json::json!({"host": host_of(&u.aa_models), "why": "Benchmark indexes with your key (GET)"}));
            }
        }
        let keys_note = if cfg!(windows) {
            "Provider keys and local API token (encrypted with Windows DPAPI for your user account), preferences"
        } else {
            "Provider keys, preferences, local API token (owner-only permissions)"
        };
        serde_json::json!({
            "data_dir": self.data_dir.display().to_string(),
            "platform": std::env::consts::OS,
            "protection": protection_note(),
            "files": [
                {"name": "settings.json", "what": keys_note},
                {"name": "harness.db", "what": "Chats, per-request metadata (model, status, timing, token counts), ratings, caches"},
            ],
            "network": dest,
            "server": format!("Local API listens on 127.0.0.1:{} only, requires your token, rejects foreign Host headers", s.server.port),
            "telemetry": "None. No analytics, crash reporting or update pings.",
        })
    }
}

/// How this platform keeps the data private, in one sentence for the UI.
pub fn protection_note() -> &'static str {
    if cfg!(target_os = "macos") {
        // macOS sets this variable for processes running inside the App Sandbox.
        if std::env::var_os("APP_SANDBOX_CONTAINER_ID").is_some() {
            "macOS App Sandbox: the app can only read and write its own private container, not your files."
        } else {
            "Stored in your user Library with owner-only permissions (headless mode is not sandboxed)."
        }
    } else if cfg!(windows) {
        "Stored in your Windows profile (%LOCALAPPDATA%, never roamed to other machines); API keys are encrypted with Windows DPAPI so only your Windows account can read them."
    } else {
        "Stored in your home directory with owner-only permissions."
    }
}

fn host_of(url: &str) -> String {
    url.split("://").nth(1).unwrap_or(url).split('/').next().unwrap_or("").to_string()
}
