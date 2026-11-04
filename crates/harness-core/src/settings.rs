//! User settings, persisted as `settings.json` (mode 0600) inside the app's
//! private data directory. API keys never leave this file except to be sent
//! to the provider they belong to.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::{secret, util};

#[derive(Clone, Debug, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct ProviderSettings {
    pub enabled: bool,
    pub api_key: String,
    pub account_id: String,
    /// Override for the provider's base URL (advanced / testing).
    pub base_url: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct CustomProvider {
    pub id: String,
    pub name: String,
    pub base_url: String,
    pub api_key: String,
    pub enabled: bool,
    /// Optional fixed model list; when empty we call `GET {base_url}/models`.
    pub models: Vec<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct ServerSettings {
    pub enabled: bool,
    pub port: u16,
    pub token: String,
}

impl Default for ServerSettings {
    fn default() -> Self {
        Self { enabled: true, port: 4141, token: String::new() }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct RoutingSettings {
    /// Maximum upstream attempts per request (across fallbacks).
    pub max_attempts: u32,
    /// Default cooldown after a rate-limit when the provider gives no hint.
    pub cooldown_secs: u64,
    pub request_timeout_secs: u64,
    /// For an explicit `provider/model`, also try the same model on other providers.
    pub same_model_fallback: bool,
}

impl Default for RoutingSettings {
    fn default() -> Self {
        Self { max_attempts: 4, cooldown_secs: 60, request_timeout_secs: 300, same_model_fallback: true }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct RankingSettings {
    /// Download public leaderboard/trending data. Off = rankings use only the
    /// bundled snapshot plus your own usage.
    pub public_data: bool,
    pub refresh_hours: u64,
    /// 0..1 — how much your own experience can move a model's score.
    pub personal_weight: f64,
    pub source_arena: bool,
    pub source_huggingface: bool,
    pub source_openrouter_models: bool,
    /// Optional: enables OpenRouter's daily token-usage rankings (Data API).
    pub openrouter_key: String,
    /// Optional: enables Artificial Analysis intelligence/coding indexes.
    pub artificial_analysis_key: String,
}

impl Default for RankingSettings {
    fn default() -> Self {
        Self {
            public_data: true,
            refresh_hours: 12,
            personal_weight: 0.35,
            source_arena: true,
            source_huggingface: true,
            source_openrouter_models: true,
            openrouter_key: String::new(),
            artificial_analysis_key: String::new(),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct ChatSettings {
    pub default_model: String,
    pub system_prompt: String,
    pub temperature: Option<f64>,
}

impl Default for ChatSettings {
    fn default() -> Self {
        Self { default_model: "auto".into(), system_prompt: String::new(), temperature: None }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct Settings {
    pub providers: BTreeMap<String, ProviderSettings>,
    pub custom_providers: Vec<CustomProvider>,
    pub server: ServerSettings,
    pub routing: RoutingSettings,
    pub ranking: RankingSettings,
    pub chat: ChatSettings,
    /// `provider/model` ids hidden from routing and rankings.
    pub hidden_models: BTreeSet<String>,
}

impl Settings {
    pub fn load(path: &Path) -> anyhow::Result<Self> {
        let mut s: Settings = match std::fs::read(path) {
            Ok(bytes) => serde_json::from_slice(&bytes)?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Settings::default(),
            Err(e) => return Err(e.into()),
        };
        s.map_secrets(secret::unprotect);
        if s.server.token.is_empty() {
            s.server.token = util::new_token();
        }
        Ok(s)
    }

    /// Writes with owner-only permissions; on Windows secrets are DPAPI-encrypted.
    pub fn save(&self, path: &Path) -> anyhow::Result<()> {
        let mut on_disk = self.clone();
        on_disk.map_secrets(secret::protect);
        let bytes = serde_json::to_vec_pretty(&on_disk)?;
        util::write_private(path, &bytes)
    }

    /// Apply `f` to every secret: provider keys, ranking keys, local API token.
    fn map_secrets(&mut self, f: impl Fn(&str) -> String) {
        for p in self.providers.values_mut() {
            p.api_key = f(&p.api_key);
        }
        for c in self.custom_providers.iter_mut() {
            c.api_key = f(&c.api_key);
        }
        self.ranking.openrouter_key = f(&self.ranking.openrouter_key);
        self.ranking.artificial_analysis_key = f(&self.ranking.artificial_analysis_key);
        self.server.token = f(&self.server.token);
    }

    pub fn provider(&self, id: &str) -> ProviderSettings {
        self.providers.get(id).cloned().unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_and_defaults() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("settings.json");
        let mut s = Settings::load(&p).unwrap();
        assert_eq!(s.server.port, 4141);
        assert!(s.server.token.starts_with("ph-"));
        s.providers.insert("groq".into(), ProviderSettings { enabled: true, api_key: "k".into(), ..Default::default() });
        s.save(&p).unwrap();
        let t = Settings::load(&p).unwrap();
        assert_eq!(t.provider("groq").api_key, "k");
        let raw = std::fs::read_to_string(&p).unwrap();
        if cfg!(windows) {
            assert!(!raw.contains(&s.server.token), "token must be DPAPI-encrypted on disk");
            assert!(raw.contains("dpapi:"));
        } else {
            assert!(raw.contains(&s.server.token));
        }
        assert_eq!(t.server.token, s.server.token);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&p).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600);
        }
    }

    #[test]
    fn tolerates_partial_json() {
        let s: Settings = serde_json::from_str(r#"{"server":{"port":5000}}"#).unwrap();
        assert_eq!(s.server.port, 5000);
        assert!(s.server.enabled);
        assert_eq!(s.routing.max_attempts, 4);
    }
}
