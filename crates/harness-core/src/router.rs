//! OpenRouter-style routing: resolve a requested model (or `auto`) into an
//! ordered list of provider routes, then try them until one answers. Rate
//! limits put a route on cooldown so the next request skips it.

use std::time::{Duration, Instant};

use serde::Serialize;
use serde_json::Value;

use crate::catalog::{Endpoint, ModelEntry, USER_AGENT};
use crate::harness::Harness;
use crate::normalize::{canon, Matcher};
use crate::ranking::{Preset, View};
use crate::store::UsageRecord;

#[derive(Clone, Debug)]
pub struct Candidate {
    pub entry: ModelEntry,
    pub endpoint: Endpoint,
}

#[derive(Clone, Debug, Serialize)]
pub struct Attempt {
    pub api_id: String,
    pub status: Option<u16>,
    pub error: String,
}

pub struct Routed {
    pub candidate: Candidate,
    pub response: reqwest::Response,
    pub attempts: Vec<Attempt>,
    pub started: Instant,
}

#[derive(Debug, Serialize)]
pub struct RouteError {
    pub status: u16,
    pub message: String,
    pub attempts: Vec<Attempt>,
}

/// `auto`, `auto:coding`, `harness/auto`, `openrouter/auto` -> preset.
pub fn auto_preset(model: &str) -> Option<Preset> {
    let m = model.trim().to_ascii_lowercase();
    let m = m.strip_prefix("harness/").or_else(|| m.strip_prefix("openrouter/")).unwrap_or(&m);
    if m == "auto" {
        return Some(Preset::General);
    }
    let rest = m.strip_prefix("auto:").or_else(|| m.strip_prefix("auto-"))?;
    Preset::parse(rest)
}

/// Resolve one requested model string into candidates (unbounded).
fn resolve_one(h: &Harness, spec: &str, entries: &[ModelEntry], same_model_fallback: bool) -> Vec<Candidate> {
    let spec = spec.trim();
    let to_cand =
        |e: &ModelEntry| h.endpoint(&e.provider).filter(|ep| ep.usable()).map(|ep| Candidate { entry: e.clone(), endpoint: ep });

    if let Some(preset) = auto_preset(spec) {
        return h
            .rankings(View::Recommended(preset))
            .into_iter()
            .flat_map(|m| m.routes.into_iter().map(|r| r.api_id))
            .filter_map(|id| entries.iter().find(|e| e.api_id == id))
            .filter_map(to_cand)
            .collect();
    }

    // exact `provider/model`
    if let Some(e) = entries.iter().find(|e| e.api_id == spec) {
        let mut out: Vec<Candidate> = to_cand(e).into_iter().collect();
        if same_model_fallback {
            out.extend(entries.iter().filter(|o| o.family == e.family && o.api_id != e.api_id).filter_map(to_cand));
        }
        return out;
    }

    // `provider/model` not in the cached list: pass straight through
    if let Some((prov, model)) = spec.split_once('/') {
        if let Some(ep) = h.endpoint(prov).filter(|ep| ep.usable()) {
            let e = ModelEntry::new(prov, model, None, None);
            let mut out = vec![Candidate { entry: e.clone(), endpoint: ep }];
            if same_model_fallback {
                out.extend(entries.iter().filter(|o| o.family == e.family && o.api_id != e.api_id).filter_map(to_cand));
            }
            return out;
        }
    }

    // bare model name: same model on any provider
    let c = canon(spec);
    let mut out: Vec<Candidate> = entries.iter().filter(|e| e.family == c.primary).filter_map(to_cand).collect();
    if out.is_empty() {
        let m = Matcher::new(entries.iter().map(|e| e.model.as_str()));
        let fams: Vec<&str> = m.find(&c).into_iter().map(|i| entries[i].family.as_str()).collect();
        out = entries.iter().filter(|e| fams.contains(&e.family.as_str())).filter_map(to_cand).collect();
    }
    out
}

/// Ordered, de-duplicated candidates for a request. Routes cooling down go last.
pub fn resolve(h: &Harness, model: &str, fallbacks: &[String]) -> Result<Vec<Candidate>, String> {
    let s = h.settings();
    let entries = h.entries(false);
    let mut specs: Vec<String> = Vec::new();
    if !model.trim().is_empty() {
        specs.push(model.to_string());
    }
    specs.extend(fallbacks.iter().filter(|f| !f.trim().is_empty()).cloned());
    if specs.is_empty() {
        specs.push("auto".into());
    }
    let mut seen = std::collections::HashSet::new();
    let mut cands: Vec<Candidate> = Vec::new();
    for spec in &specs {
        for c in resolve_one(h, spec, &entries, s.routing.same_model_fallback) {
            if seen.insert(c.entry.api_id.clone()) {
                cands.push(c);
            }
        }
    }
    if cands.is_empty() {
        let configured = crate::catalog::all_endpoints(&s).iter().any(|e| e.usable());
        return Err(if !configured {
            "No provider is configured yet. Add a free API key in Personal Harness → Providers.".into()
        } else {
            format!("Unknown model '{}'. GET /v1/models lists what you can use, or use 'auto'.", specs.join(", "))
        });
    }
    let cooling = h.cooldown_map();
    let (ready, cool): (Vec<_>, Vec<_>) = cands.into_iter().partition(|c| !cooling.contains_key(&c.entry.api_id));
    let mut out = ready;
    out.extend(cool);
    out.truncate(s.routing.max_attempts.max(1) as usize);
    Ok(out)
}

/// Keys that are OpenRouter-specific or that we set ourselves.
const STRIP_KEYS: &[&str] = &["models", "provider", "route", "transforms", "plugins", "usage"];

pub fn prepare_body(body: &Value, model: &str, stream: bool) -> Value {
    let mut b = body.clone();
    if let Some(o) = b.as_object_mut() {
        for k in STRIP_KEYS {
            o.remove(*k);
        }
        o.insert("model".into(), Value::String(model.to_string()));
        if stream {
            o.insert("stream".into(), Value::Bool(true));
        } else {
            o.remove("stream");
            o.remove("stream_options");
        }
    }
    b
}

/// `Retry-After: 12`, `x-ratelimit-reset-requests: 2m59.5s`, `1.2s`, `350ms`.
pub fn parse_retry(headers: &reqwest::header::HeaderMap) -> Option<Duration> {
    let get = |k: &str| headers.get(k).and_then(|v| v.to_str().ok()).map(|s| s.trim().to_string());
    if let Some(v) = get("retry-after") {
        if let Ok(secs) = v.parse::<f64>() {
            return Some(Duration::from_secs_f64(secs.max(0.0)));
        }
    }
    for k in ["x-ratelimit-reset-requests", "x-ratelimit-reset-tokens", "x-ratelimit-reset"] {
        if let Some(v) = get(k) {
            if let Some(d) = parse_duration(&v) {
                return Some(d);
            }
        }
    }
    None
}

pub fn parse_duration(v: &str) -> Option<Duration> {
    let v = v.trim();
    if let Some(ms) = v.strip_suffix("ms") {
        return ms.parse::<f64>().ok().map(|m| Duration::from_secs_f64(m / 1000.0));
    }
    if let Ok(secs) = v.parse::<f64>() {
        return Some(Duration::from_secs_f64(secs));
    }
    // "2m59.56s", "1h2m3s", "7.66s", "3m"
    let mut total = 0.0;
    let mut num = String::new();
    let mut any = false;
    for ch in v.chars() {
        if ch.is_ascii_digit() || ch == '.' {
            num.push(ch);
        } else {
            let n: f64 = num.parse().ok()?;
            num.clear();
            any = true;
            total += match ch {
                'h' => n * 3600.0,
                'm' => n * 60.0,
                's' => n,
                _ => return None,
            };
        }
    }
    if !num.is_empty() {
        return None;
    }
    any.then(|| Duration::from_secs_f64(total))
}

fn cooldown_for(status: u16, retry: Option<Duration>, default_secs: u64) -> Option<(Duration, &'static str)> {
    let clamp = |d: Duration| d.clamp(Duration::from_secs(5), Duration::from_secs(3600));
    match status {
        429 => Some((clamp(retry.unwrap_or(Duration::from_secs(default_secs))), "rate limited")),
        401 | 403 => Some((Duration::from_secs(600), "auth/permission error")),
        402 => Some((Duration::from_secs(3600), "quota exhausted / payment required")),
        404 => Some((Duration::from_secs(600), "model not found")),
        500..=599 => Some((clamp(retry.unwrap_or(Duration::from_secs(20))), "provider error")),
        _ => None,
    }
}

fn error_text(body: &str) -> String {
    if let Ok(v) = serde_json::from_str::<Value>(body) {
        let msg = v
            .pointer("/error/message")
            .or_else(|| v.pointer("/error"))
            .or_else(|| v.pointer("/message"))
            .or_else(|| v.pointer("/errors/0/message"))
            .or_else(|| v.pointer("/0/error/message"));
        if let Some(m) = msg {
            return m.as_str().map(String::from).unwrap_or_else(|| m.to_string());
        }
    }
    crate::util::truncate_chars(body.trim(), 300)
}

/// Try candidates in order until one returns a 2xx response.
pub async fn execute(
    h: &Harness,
    cands: Vec<Candidate>,
    body: &Value,
    stream: bool,
    source: &str,
    mut on_fail: impl FnMut(&Attempt),
) -> Result<Routed, RouteError> {
    let routing = h.settings().routing;
    let mut attempts: Vec<Attempt> = Vec::new();
    let mut last_status = 502u16;
    for cand in cands {
        let payload = prepare_body(body, &cand.entry.model, stream);
        let mut req = h.http.post(cand.endpoint.chat_url()).header("User-Agent", USER_AGENT).json(&payload);
        if !cand.endpoint.api_key.is_empty() {
            req = req.bearer_auth(&cand.endpoint.api_key);
        }
        for (k, v) in &cand.endpoint.extra_headers {
            req = req.header(k, v);
        }
        if stream {
            req = req.header("Accept", "text/event-stream");
        } else {
            req = req.timeout(Duration::from_secs(routing.request_timeout_secs.max(10)));
        }
        let started = Instant::now();
        let sent = tokio::time::timeout(
            Duration::from_secs(if stream { 90 } else { routing.request_timeout_secs.max(10) + 5 }),
            req.send(),
        )
        .await;
        let fail = |status: Option<u16>, error: String| Attempt { api_id: cand.entry.api_id.clone(), status, error };
        let attempt = match sent {
            Err(_) => {
                h.set_cooldown(&cand.entry.api_id, Duration::from_secs(30), "timed out");
                fail(None, "timed out waiting for the provider".into())
            }
            Ok(Err(e)) => {
                h.set_cooldown(&cand.entry.api_id, Duration::from_secs(30), "network error");
                fail(None, format!("network error: {e}"))
            }
            Ok(Ok(resp)) if resp.status().is_success() => {
                h.clear_cooldown(&cand.entry.api_id);
                return Ok(Routed { candidate: cand, response: resp, attempts, started });
            }
            Ok(Ok(resp)) => {
                let status = resp.status().as_u16();
                let retry = parse_retry(resp.headers());
                let text = resp.text().await.unwrap_or_default();
                if let Some((d, why)) = cooldown_for(status, retry, routing.cooldown_secs) {
                    h.set_cooldown(&cand.entry.api_id, d, why);
                }
                last_status = status;
                fail(Some(status), error_text(&text))
            }
        };
        h.store.record_usage(&UsageRecord {
            provider: cand.entry.provider.clone(),
            model: cand.entry.model.clone(),
            family: cand.entry.family.clone(),
            source: source.into(),
            ok: false,
            status: attempt.status,
            error: Some(attempt.error.clone()),
            duration_ms: Some(started.elapsed().as_millis() as i64),
            ..Default::default()
        });
        tracing::info!("route {} failed ({:?}): {}", attempt.api_id, attempt.status, attempt.error);
        on_fail(&attempt);
        attempts.push(attempt);
    }
    let message = if attempts.is_empty() {
        "no candidates".to_string()
    } else {
        let list: Vec<String> = attempts
            .iter()
            .map(|a| format!("{} → {}{}", a.api_id, a.status.map(|s| format!("{s} ")).unwrap_or_default(), a.error))
            .collect();
        format!("All {} route(s) failed: {}", attempts.len(), list.join(" | "))
    };
    // If every failure was a rate limit, tell the client so (429), else 502.
    let status = if !attempts.is_empty() && attempts.iter().all(|a| a.status == Some(429)) {
        429
    } else if attempts.len() == 1 {
        last_status
    } else {
        502
    };
    Err(RouteError { status, message, attempts })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn auto_presets() {
        assert_eq!(auto_preset("auto"), Some(Preset::General));
        assert_eq!(auto_preset("harness/auto"), Some(Preset::General));
        assert_eq!(auto_preset("openrouter/auto"), Some(Preset::General));
        assert_eq!(auto_preset("auto:coding"), Some(Preset::Coding));
        assert_eq!(auto_preset("auto-fast"), Some(Preset::Fast));
        assert_eq!(auto_preset("autopilot"), None);
        assert_eq!(auto_preset("groq/llama"), None);
    }

    #[test]
    fn durations() {
        assert_eq!(parse_duration("2m59.5s"), Some(Duration::from_secs_f64(179.5)));
        assert_eq!(parse_duration("7.66s"), Some(Duration::from_secs_f64(7.66)));
        assert_eq!(parse_duration("350ms"), Some(Duration::from_millis(350)));
        assert_eq!(parse_duration("12"), Some(Duration::from_secs(12)));
        assert_eq!(parse_duration("1h2m3s"), Some(Duration::from_secs(3723)));
        assert_eq!(parse_duration("soon"), None);
    }

    #[test]
    fn body_prep() {
        let b = serde_json::json!({"model":"auto","models":["a","b"],"provider":{"order":["x"]},"messages":[],"stream":true,"stream_options":{"include_usage":true}});
        let p = prepare_body(&b, "llama-3.3-70b-versatile", false);
        assert_eq!(p["model"], "llama-3.3-70b-versatile");
        assert!(p.get("models").is_none() && p.get("provider").is_none());
        assert!(p.get("stream").is_none() && p.get("stream_options").is_none());
        let p = prepare_body(&b, "m", true);
        assert_eq!(p["stream"], true);
        assert!(p.get("stream_options").is_some());
    }

    #[test]
    fn error_texts() {
        assert_eq!(error_text(r#"{"error":{"message":"Rate limit reached"}}"#), "Rate limit reached");
        assert_eq!(error_text(r#"[{"error":{"code":429,"message":"Quota exceeded"}}]"#), "Quota exceeded");
        assert_eq!(error_text("plain"), "plain");
    }
}
