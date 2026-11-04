//! Model discovery: ask each configured provider which models your key can use.

use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::normalize::{base_name, canon};
use crate::providers::{self, is_chat_model, ModelsSource};
use crate::settings::Settings;

pub const USER_AGENT: &str = concat!("PersonalHarness/", env!("CARGO_PKG_VERSION"));

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct ModelEntry {
    pub provider: String,
    /// Id sent upstream, e.g. `llama-3.3-70b-versatile`.
    pub model: String,
    /// `provider/model`, what clients use with the local API.
    pub api_id: String,
    pub name: String,
    /// Canonical key shared by the same model on different providers.
    pub family: String,
    pub context_length: Option<u64>,
    /// Unix seconds, when the provider reports it.
    pub created: Option<i64>,
}

impl ModelEntry {
    pub fn new(provider: &str, model: &str, context_length: Option<u64>, created: Option<i64>) -> Self {
        let model = model.trim().to_string();
        let name = base_name(&model).to_string();
        Self {
            provider: provider.to_string(),
            api_id: format!("{provider}/{model}"),
            family: canon(&model).primary,
            name: if name.is_empty() { model.clone() } else { name },
            model,
            context_length,
            created,
        }
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct ProviderCatalog {
    pub provider: String,
    pub fetched_at: i64,
    pub models: Vec<ModelEntry>,
    pub error: Option<String>,
}

/// Everything needed to talk to one provider (built-in or custom).
#[derive(Clone, Debug)]
pub struct Endpoint {
    pub provider: String,
    pub name: String,
    pub base_url: String,
    pub api_key: String,
    pub source: ModelsSource,
    pub static_models: Vec<String>,
    pub extra_headers: Vec<(String, String)>,
    pub speed_prior: f64,
    pub enabled: bool,
    /// Built-in providers need a key; custom ones (e.g. local Ollama) may not.
    pub requires_key: bool,
}

impl Endpoint {
    pub fn usable(&self) -> bool {
        self.enabled
            && (!self.requires_key || !self.api_key.trim().is_empty())
            && !self.base_url.trim().is_empty()
            && !self.base_url.contains("{account_id}")
    }

    pub fn chat_url(&self) -> String {
        format!("{}/chat/completions", self.base_url.trim_end_matches('/'))
    }
}

pub fn endpoint(settings: &Settings, id: &str) -> Option<Endpoint> {
    if let Some(def) = providers::builtin(id) {
        let ps = settings.provider(id);
        let mut base = ps.base_url.clone().filter(|b| !b.trim().is_empty()).unwrap_or_else(|| def.base_url.to_string());
        if def.needs_account_id && !ps.account_id.trim().is_empty() {
            base = base.replace("{account_id}", ps.account_id.trim());
        }
        return Some(Endpoint {
            provider: def.id.to_string(),
            name: def.name.to_string(),
            base_url: base,
            api_key: ps.api_key.trim().to_string(),
            source: def.models,
            static_models: def.static_models.iter().map(|s| s.to_string()).collect(),
            extra_headers: def.extra_headers.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect(),
            speed_prior: def.speed_prior,
            enabled: ps.enabled,
            requires_key: true,
        });
    }
    settings.custom_providers.iter().find(|c| c.id == id).map(|c| Endpoint {
        provider: c.id.clone(),
        name: c.name.clone(),
        base_url: c.base_url.trim().trim_end_matches('/').to_string(),
        api_key: c.api_key.trim().to_string(),
        source: if c.models.is_empty() { ModelsSource::OpenAi } else { ModelsSource::Static },
        static_models: c.models.clone(),
        extra_headers: vec![],
        speed_prior: 55.0,
        enabled: c.enabled,
        requires_key: false,
    })
}

/// All providers (built-in first, then custom), usable or not.
pub fn all_endpoints(settings: &Settings) -> Vec<Endpoint> {
    providers::BUILTIN
        .iter()
        .map(|d| d.id.to_string())
        .chain(settings.custom_providers.iter().map(|c| c.id.clone()))
        .filter_map(|id| endpoint(settings, &id))
        .collect()
}

fn auth(req: reqwest::RequestBuilder, ep: &Endpoint) -> reqwest::RequestBuilder {
    let mut req = req.header("User-Agent", USER_AGENT);
    if !ep.api_key.is_empty() {
        req = req.bearer_auth(&ep.api_key);
    }
    for (k, v) in &ep.extra_headers {
        req = req.header(k, v);
    }
    req
}

pub async fn fetch_models(http: &reqwest::Client, ep: &Endpoint) -> anyhow::Result<Vec<ModelEntry>> {
    let mut out = match ep.source {
        ModelsSource::Static => ep.static_models.iter().map(|m| ModelEntry::new(&ep.provider, m, None, None)).collect::<Vec<_>>(),
        ModelsSource::OpenAi => {
            let url = format!("{}/models", ep.base_url.trim_end_matches('/'));
            let v = get_json(auth(http.get(&url), ep)).await?;
            parse_openai_models(&ep.provider, &v)
        }
        ModelsSource::GitHubCatalog => {
            let base = ep.base_url.trim_end_matches('/');
            let root = base.strip_suffix("/inference").unwrap_or(base);
            let url = format!("{root}/catalog/models");
            let v = get_json(auth(http.get(&url), ep).header("Accept", "application/vnd.github+json")).await?;
            parse_github_catalog(&ep.provider, &v)
        }
        ModelsSource::CloudflareSearch => {
            let base = ep.base_url.trim_end_matches('/');
            let root = base.strip_suffix("/v1").unwrap_or(base);
            let url = format!("{root}/models/search?task=Text%20Generation&per_page=200");
            let v = get_json(auth(http.get(&url), ep)).await?;
            parse_cloudflare_models(&ep.provider, &v)
        }
    };
    out.sort_by(|a, b| a.model.cmp(&b.model));
    out.dedup_by(|a, b| a.model == b.model);
    Ok(out)
}

async fn get_json(req: reqwest::RequestBuilder) -> anyhow::Result<Value> {
    let resp = req.timeout(Duration::from_secs(25)).send().await?;
    let status = resp.status();
    let text = resp.text().await?;
    if !status.is_success() {
        anyhow::bail!("HTTP {}: {}", status.as_u16(), crate::util::truncate_chars(text.trim(), 300));
    }
    Ok(serde_json::from_str(&text)?)
}

fn u64_field(v: &Value, keys: &[&str]) -> Option<u64> {
    keys.iter().find_map(|k| {
        let x = v.get(*k)?;
        x.as_u64().or_else(|| x.as_f64().map(|f| f as u64)).or_else(|| x.as_str()?.parse().ok())
    })
}

pub fn parse_openai_models(provider: &str, v: &Value) -> Vec<ModelEntry> {
    let items = v.get("data").and_then(|d| d.as_array()).or_else(|| v.as_array()).cloned().unwrap_or_default();
    let mut seen_names = std::collections::HashSet::new();
    let mut out = Vec::new();
    // Prefer `*-latest` aliases first so Mistral-style duplicates collapse onto them.
    let mut items = items;
    items.sort_by_key(|m| !m.get("id").and_then(|i| i.as_str()).unwrap_or("").ends_with("-latest"));
    for m in items {
        let Some(id) = m.get("id").and_then(|i| i.as_str()) else { continue };
        if m.get("active").and_then(|a| a.as_bool()) == Some(false) {
            continue;
        }
        if let Some(caps) = m.get("capabilities") {
            if caps.get("completion_chat").and_then(|c| c.as_bool()) == Some(false) {
                continue;
            }
        }
        if m.get("deprecation").map(|d| !d.is_null()).unwrap_or(false) {
            continue;
        }
        if !is_chat_model(id) {
            continue;
        }
        // Mistral lists aliases as separate rows sharing `name`.
        if let Some(name) = m.get("name").and_then(|n| n.as_str()) {
            if provider == "mistral" && !seen_names.insert(name.to_string()) {
                continue;
            }
        }
        let model_id = id.strip_prefix("models/").unwrap_or(id);
        let ctx = u64_field(&m, &["context_window", "context_length", "max_context_length", "max_model_len", "max_input_tokens"]);
        let created = m.get("created").and_then(|c| c.as_i64()).filter(|c| *c > 1_500_000_000);
        out.push(ModelEntry::new(provider, model_id, ctx, created));
    }
    out
}

pub fn parse_github_catalog(provider: &str, v: &Value) -> Vec<ModelEntry> {
    let items = v.as_array().cloned().or_else(|| v.get("data").and_then(|d| d.as_array()).cloned()).unwrap_or_default();
    items
        .iter()
        .filter_map(|m| {
            let id = m.get("id").and_then(|i| i.as_str())?;
            if let Some(outs) = m.get("supported_output_modalities").and_then(|o| o.as_array()) {
                if !outs.iter().any(|o| o.as_str() == Some("text")) {
                    return None;
                }
            }
            if !is_chat_model(id) {
                return None;
            }
            let ctx = m.get("limits").and_then(|l| u64_field(l, &["max_input_tokens"]));
            Some(ModelEntry::new(provider, id, ctx, None))
        })
        .collect()
}

pub fn parse_cloudflare_models(provider: &str, v: &Value) -> Vec<ModelEntry> {
    let items = v.get("result").and_then(|r| r.as_array()).cloned().unwrap_or_default();
    items
        .iter()
        .filter_map(|m| {
            let name = m.get("name").and_then(|i| i.as_str())?;
            let task = m.get("task").and_then(|t| t.get("name")).and_then(|t| t.as_str()).unwrap_or("Text Generation");
            if task != "Text Generation" || !is_chat_model(name) || name.contains("lora") {
                return None;
            }
            let ctx = m.get("properties").and_then(|p| p.as_array()).and_then(|ps| {
                ps.iter().find(|p| p.get("property_id").and_then(|i| i.as_str()) == Some("context_window")).and_then(|p| {
                    let v = p.get("value")?;
                    v.as_u64().or_else(|| v.as_str()?.parse().ok())
                })
            });
            Some(ModelEntry::new(provider, name, ctx, None))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parses_groq_style() {
        let v = json!({"object":"list","data":[
            {"id":"llama-3.3-70b-versatile","object":"model","created":1733447754,"owned_by":"Meta","active":true,"context_window":131072},
            {"id":"whisper-large-v3","active":true,"context_window":448},
            {"id":"old-model","active":false}
        ]});
        let m = parse_openai_models("groq", &v);
        assert_eq!(m.len(), 1);
        assert_eq!(m[0].api_id, "groq/llama-3.3-70b-versatile");
        assert_eq!(m[0].family, "llama-3.3-70b");
        assert_eq!(m[0].context_length, Some(131072));
        assert_eq!(m[0].created, Some(1733447754));
    }

    #[test]
    fn parses_gemini_and_mistral_style() {
        let g = json!({"data":[{"id":"models/gemini-3.6-flash"},{"id":"models/text-embedding-004"}]});
        let m = parse_openai_models("gemini", &g);
        assert_eq!(m.len(), 1);
        assert_eq!(m[0].model, "gemini-3.6-flash");

        let mi = json!({"data":[
            {"id":"mistral-large-2512","name":"mistral-large-2512","capabilities":{"completion_chat":true},"max_context_length":131072},
            {"id":"mistral-large-latest","name":"mistral-large-2512","capabilities":{"completion_chat":true}},
            {"id":"mistral-embed","name":"mistral-embed","capabilities":{"completion_chat":false}}
        ]});
        let m = parse_openai_models("mistral", &mi);
        assert_eq!(m.len(), 1);
        assert_eq!(m[0].model, "mistral-large-latest");
    }

    #[test]
    fn parses_github_and_cloudflare() {
        let gh = json!([
            {"id":"openai/gpt-4.1","supported_output_modalities":["text"],"limits":{"max_input_tokens":1048576}},
            {"id":"openai/text-embedding-3-large","supported_output_modalities":["embeddings"]}
        ]);
        let m = parse_github_catalog("github", &gh);
        assert_eq!(m.len(), 1);
        assert_eq!(m[0].api_id, "github/openai/gpt-4.1");
        assert_eq!(m[0].family, "gpt-4.1");
        assert_eq!(m[0].context_length, Some(1048576));

        let cf = json!({"success":true,"result":[
            {"name":"@cf/meta/llama-3.3-70b-instruct-fp8-fast","task":{"name":"Text Generation"},"properties":[{"property_id":"context_window","value":"24000"}]},
            {"name":"@cf/baai/bge-m3","task":{"name":"Text Embeddings"}}
        ]});
        let m = parse_cloudflare_models("cloudflare", &cf);
        assert_eq!(m.len(), 1);
        assert_eq!(m[0].family, "llama-3.3-70b");
        assert_eq!(m[0].context_length, Some(24000));
    }

    #[test]
    fn endpoint_resolution() {
        let mut s = Settings::default();
        s.providers.insert(
            "cloudflare".into(),
            crate::settings::ProviderSettings { enabled: true, api_key: "k".into(), account_id: "abc".into(), base_url: None },
        );
        let ep = endpoint(&s, "cloudflare").unwrap();
        assert!(ep.usable());
        assert_eq!(ep.chat_url(), "https://api.cloudflare.com/client/v4/accounts/abc/ai/v1/chat/completions");
        let ep = endpoint(&s, "groq").unwrap();
        assert!(!ep.usable());
        s.custom_providers.push(crate::settings::CustomProvider {
            id: "ollama".into(),
            name: "Ollama".into(),
            base_url: "http://localhost:11434/v1/".into(),
            enabled: true,
            ..Default::default()
        });
        let ep = endpoint(&s, "ollama").unwrap();
        assert!(ep.usable(), "keyless custom provider is usable");
        assert_eq!(ep.chat_url(), "http://localhost:11434/v1/chat/completions");
        assert_eq!(all_endpoints(&s).len(), crate::providers::BUILTIN.len() + 1);
    }
}
