//! Public ranking signals. All requests are anonymous GETs (no cookies, no
//! identifiers); optional keys are only sent to the service they belong to.
//!
//! * strength: LMArena text/code scores (via a public daily JSON mirror),
//!   optional Artificial Analysis indexes
//! * usage: optional OpenRouter Data API daily token rankings, Hugging Face
//!   downloads, LMArena vote counts
//! * hotness: Hugging Face trending score, release recency, OpenRouter usage
//!   growth

use std::time::Duration;

use chrono::{DateTime, NaiveDate, Utc};
use futures_util::future::join_all;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::catalog::USER_AGENT;
use crate::settings::RankingSettings;
use crate::util::now_ms;

const BASELINE_JSON: &str = include_str!("../data/baseline.json");

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct ArenaEntry {
    pub model: String,
    pub score: f64,
    pub votes: Option<i64>,
    pub vendor: Option<String>,
    pub license: Option<String>,
    /// Snapshot the score comes from.
    pub date: Option<String>,
    /// First snapshot in which the model appeared (a proxy for release date).
    #[serde(default)]
    pub first_seen: Option<String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct HfEntry {
    pub id: String,
    pub downloads: i64,
    pub likes: i64,
    pub trending: f64,
    pub created_at: Option<i64>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct OrModel {
    pub id: String,
    pub name: String,
    pub created: Option<i64>,
    pub context_length: Option<u64>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct OrUsage {
    pub slug: String,
    pub tokens_7d: f64,
    pub tokens_recent3: f64,
    pub tokens_prior4: f64,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct AaEntry {
    pub name: String,
    pub slug: String,
    pub intelligence: Option<f64>,
    pub coding: Option<f64>,
    pub tokens_per_sec: Option<f64>,
    pub release: Option<i64>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct SourceStatus {
    pub id: String,
    pub label: String,
    pub ok: bool,
    pub count: usize,
    pub error: Option<String>,
    pub fetched_at: i64,
    pub detail: String,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Signals {
    pub fetched_at: i64,
    pub arena_text: Vec<ArenaEntry>,
    pub arena_code: Vec<ArenaEntry>,
    pub arena_as_of: Option<String>,
    pub hf: Vec<HfEntry>,
    pub or_models: Vec<OrModel>,
    pub or_usage: Vec<OrUsage>,
    pub aa: Vec<AaEntry>,
    pub sources: Vec<SourceStatus>,
}

#[derive(Deserialize)]
struct Baseline {
    generated_at: String,
    boards: BaselineBoards,
}

#[derive(Deserialize)]
struct BaselineBoards {
    text: Vec<ArenaEntry>,
    code: Vec<ArenaEntry>,
}

/// Endpoints, overridable for tests.
#[derive(Clone, Debug)]
pub struct SignalUrls {
    pub arena_base: String,
    pub hf_api: String,
    pub or_models: String,
    pub or_rankings: String,
    pub aa_models: String,
}

impl Default for SignalUrls {
    fn default() -> Self {
        Self {
            arena_base: "https://raw.githubusercontent.com/oolong-tea-2026/arena-ai-leaderboards/main/data".into(),
            hf_api: "https://huggingface.co/api/models".into(),
            or_models: "https://openrouter.ai/api/v1/models".into(),
            or_rankings: "https://openrouter.ai/api/v1/datasets/rankings-daily".into(),
            aa_models: "https://artificialanalysis.ai/api/v2/data/llms/models".into(),
        }
    }
}

impl Signals {
    /// Bundled LMArena snapshot so rankings work offline and on first launch.
    pub fn baseline() -> Self {
        let b: Baseline = serde_json::from_str(BASELINE_JSON).expect("bundled baseline.json is valid");
        Signals {
            fetched_at: 0,
            arena_text: b.boards.text,
            arena_code: b.boards.code,
            arena_as_of: Some(b.generated_at.clone()),
            sources: vec![SourceStatus {
                id: "baseline".into(),
                label: "Bundled LMArena snapshot".into(),
                ok: true,
                count: 0,
                error: None,
                fetched_at: 0,
                detail: format!("Shipped with the app (generated {})", b.generated_at),
            }],
            ..Default::default()
        }
    }

    pub fn source(&self, id: &str) -> Option<&SourceStatus> {
        self.sources.iter().find(|s| s.id == id)
    }
}

fn merge_arena(mut fresh: Vec<ArenaEntry>, base: &[ArenaEntry]) -> Vec<ArenaEntry> {
    let known: std::collections::HashMap<&str, &ArenaEntry> = base.iter().map(|e| (e.model.as_str(), e)).collect();
    for e in fresh.iter_mut() {
        match known.get(e.model.as_str()) {
            Some(b) => e.first_seen = b.first_seen.clone(),
            // not in the bundled history => it appeared after the app was built
            None => e.first_seen = e.date.clone(),
        }
    }
    let mut seen: std::collections::HashSet<String> = fresh.iter().map(|e| e.model.clone()).collect();
    let mut out = fresh;
    for e in base {
        if seen.insert(e.model.clone()) {
            out.push(e.clone());
        }
    }
    out.sort_by(|a, b| b.score.partial_cmp(&a.score).unwrap_or(std::cmp::Ordering::Equal));
    out
}

async fn get_json(http: &reqwest::Client, url: String, headers: &[(&str, &str)]) -> anyhow::Result<Value> {
    let mut req = http.get(&url).header("User-Agent", USER_AGENT).timeout(Duration::from_secs(25));
    for (k, v) in headers {
        req = req.header(*k, *v);
    }
    let resp = req.send().await?;
    let status = resp.status();
    let body = resp.text().await?;
    if !status.is_success() {
        anyhow::bail!("HTTP {}", status.as_u16());
    }
    Ok(serde_json::from_str(&body)?)
}

// ---- parsers (pure, unit-tested) ----------------------------------------------

pub fn parse_arena_board(v: &Value) -> Vec<ArenaEntry> {
    v.get("models")
        .and_then(|m| m.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|m| {
                    Some(ArenaEntry {
                        model: m.get("model")?.as_str()?.to_string(),
                        score: m.get("score")?.as_f64()?,
                        votes: m.get("votes").and_then(|x| x.as_i64()),
                        vendor: m.get("vendor").and_then(|x| x.as_str()).map(String::from),
                        license: m.get("license").and_then(|x| x.as_str()).map(String::from),
                        date: None,
                        first_seen: None,
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

pub fn parse_time(s: &str) -> Option<i64> {
    if let Ok(t) = DateTime::parse_from_rfc3339(s) {
        return Some(t.timestamp());
    }
    NaiveDate::parse_from_str(&s[..s.len().min(10)], "%Y-%m-%d")
        .ok()
        .map(|d| d.and_hms_opt(0, 0, 0).unwrap().and_utc().timestamp())
}

pub fn parse_hf(v: &Value) -> Vec<HfEntry> {
    v.as_array()
        .map(|arr| {
            arr.iter()
                .filter_map(|m| {
                    Some(HfEntry {
                        id: m.get("id").or_else(|| m.get("modelId"))?.as_str()?.to_string(),
                        downloads: m.get("downloads").and_then(|x| x.as_i64()).unwrap_or(0),
                        likes: m.get("likes").and_then(|x| x.as_i64()).unwrap_or(0),
                        trending: m.get("trendingScore").and_then(|x| x.as_f64()).unwrap_or(0.0),
                        created_at: m.get("createdAt").and_then(|x| x.as_str()).and_then(parse_time),
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

pub fn parse_or_models(v: &Value) -> Vec<OrModel> {
    v.get("data")
        .and_then(|d| d.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|m| {
                    Some(OrModel {
                        id: m.get("id")?.as_str()?.to_string(),
                        name: m.get("name").and_then(|x| x.as_str()).unwrap_or("").to_string(),
                        created: m.get("created").and_then(|x| x.as_i64()),
                        context_length: m.get("context_length").and_then(|x| x.as_u64()),
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

fn num(v: &Value) -> Option<f64> {
    v.as_f64().or_else(|| v.as_str()?.parse().ok())
}

/// Rows of `{date, model_permaslug, total_tokens}`; aggregated per model with
/// a recent-vs-prior split for growth.
pub fn parse_or_rankings(v: &Value, today: NaiveDate) -> Vec<OrUsage> {
    let rows = v
        .get("data")
        .and_then(|d| d.as_array().cloned().or_else(|| d.get("rows").and_then(|r| r.as_array()).cloned()))
        .or_else(|| v.as_array().cloned())
        .unwrap_or_default();
    let mut agg: std::collections::BTreeMap<String, OrUsage> = Default::default();
    for r in rows {
        let Some(slug) = ["model_permaslug", "model", "slug", "model_slug"].iter().find_map(|k| r.get(*k)?.as_str()) else {
            continue;
        };
        if slug == "other" || slug.is_empty() {
            continue;
        }
        let tokens = ["total_tokens", "tokens"].iter().find_map(|k| num(r.get(*k)?)).unwrap_or(0.0);
        let day = r
            .get("date")
            .and_then(|d| d.as_str())
            .and_then(|d| NaiveDate::parse_from_str(&d[..d.len().min(10)], "%Y-%m-%d").ok());
        let e = agg.entry(slug.to_string()).or_insert_with(|| OrUsage { slug: slug.to_string(), ..Default::default() });
        e.tokens_7d += tokens;
        if let Some(d) = day {
            let age = (today - d).num_days();
            if age <= 3 {
                e.tokens_recent3 += tokens;
            } else {
                e.tokens_prior4 += tokens;
            }
        }
    }
    agg.into_values().collect()
}

pub fn parse_aa(v: &Value) -> Vec<AaEntry> {
    let arr = v.get("data").and_then(|d| d.as_array()).cloned().or_else(|| v.as_array().cloned()).unwrap_or_default();
    arr.iter()
        .filter_map(|m| {
            let name = m.get("name").and_then(|x| x.as_str()).unwrap_or("").to_string();
            let slug = m.get("slug").and_then(|x| x.as_str()).unwrap_or("").to_string();
            if name.is_empty() && slug.is_empty() {
                return None;
            }
            let ev = m.get("evaluations").cloned().unwrap_or(Value::Null);
            Some(AaEntry {
                name,
                slug,
                intelligence: ev.get("artificial_analysis_intelligence_index").and_then(num),
                coding: ev.get("artificial_analysis_coding_index").and_then(num),
                tokens_per_sec: m.get("median_output_tokens_per_second").and_then(num),
                release: m.get("release_date").and_then(|x| x.as_str()).and_then(parse_time),
            })
        })
        .collect()
}

// ---- fetchers ---------------------------------------------------------------

/// Find the newest snapshot that actually contains leaderboards (the mirror
/// sometimes publishes empty days) and fetch its text + code boards.
async fn fetch_arena(http: &reqwest::Client, base: &str) -> anyhow::Result<(Vec<ArenaEntry>, Vec<ArenaEntry>, String)> {
    let latest = get_json(http, format!("{base}/latest.json"), &[]).await?;
    let start = latest
        .get("date")
        .and_then(|d| d.as_str())
        .and_then(|d| NaiveDate::parse_from_str(d, "%Y-%m-%d").ok())
        .unwrap_or_else(|| Utc::now().date_naive());
    for chunk in (0..60).collect::<Vec<i64>>().chunks(6) {
        let days: Vec<NaiveDate> = chunk.iter().map(|b| start - chrono::Duration::days(*b)).collect();
        let idx = join_all(days.iter().map(|d| get_json(http, format!("{base}/{d}/_index.json"), &[]))).await;
        for (d, r) in days.iter().zip(idx) {
            let Ok(v) = r else { continue };
            let boards = v.get("leaderboards").and_then(|l| l.as_object());
            let has = |k: &str| boards.map(|b| b.contains_key(k)).unwrap_or(false);
            if !(has("text") || has("code")) {
                continue;
            }
            let ds = d.to_string();
            let text = if has("text") {
                get_json(http, format!("{base}/{ds}/text.json"), &[]).await.map(|v| parse_arena_board(&v)).unwrap_or_default()
            } else {
                vec![]
            };
            let code = if has("code") {
                get_json(http, format!("{base}/{ds}/code.json"), &[]).await.map(|v| parse_arena_board(&v)).unwrap_or_default()
            } else {
                vec![]
            };
            let tag = |mut v: Vec<ArenaEntry>| {
                v.iter_mut().for_each(|e| e.date = Some(ds.clone()));
                v
            };
            return Ok((tag(text), tag(code), ds));
        }
    }
    anyhow::bail!("no non-empty snapshot in the last 60 days")
}

async fn fetch_hf(http: &reqwest::Client, api: &str) -> anyhow::Result<Vec<HfEntry>> {
    let queries = [
        "pipeline_tag=text-generation&sort=trendingScore&direction=-1&limit=150",
        "pipeline_tag=image-text-to-text&sort=trendingScore&direction=-1&limit=80",
        "pipeline_tag=text-generation&sort=downloads&direction=-1&limit=200",
    ];
    let res = join_all(queries.iter().map(|q| get_json(http, format!("{api}?{q}"), &[]))).await;
    let mut out: Vec<HfEntry> = Vec::new();
    let mut seen = std::collections::HashSet::new();
    let mut first_err = None;
    for r in res {
        match r {
            Ok(v) => {
                for e in parse_hf(&v) {
                    if seen.insert(e.id.clone()) {
                        out.push(e);
                    }
                }
            }
            Err(e) => first_err = first_err.or(Some(e)),
        }
    }
    if out.is_empty() {
        if let Some(e) = first_err {
            return Err(e);
        }
    }
    Ok(out)
}

/// Refresh every enabled source concurrently. Sources that fail keep their
/// previous data (from `prev`), so a flaky network never empties rankings.
pub async fn refresh(http: &reqwest::Client, cfg: &RankingSettings, prev: &Signals, urls: &SignalUrls) -> Signals {
    let base = Signals::baseline();
    let now = now_ms();
    let mut out = Signals { fetched_at: now, ..Default::default() };
    let mut sources = vec![base.sources[0].clone()];

    let on = cfg.public_data;
    let today = Utc::now().date_naive();
    let or_url = format!(
        "{}?start_date={}&end_date={}",
        urls.or_rankings,
        today - chrono::Duration::days(7),
        today - chrono::Duration::days(1)
    );
    let or_auth = format!("Bearer {}", cfg.openrouter_key.trim());
    let aa_key = cfg.artificial_analysis_key.trim().to_string();

    let arena_f = async {
        if on && cfg.source_arena {
            Some(fetch_arena(http, &urls.arena_base).await)
        } else {
            None
        }
    };
    let hf_f = async {
        if on && cfg.source_huggingface {
            Some(fetch_hf(http, &urls.hf_api).await)
        } else {
            None
        }
    };
    let orm_f = async {
        if on && cfg.source_openrouter_models {
            Some(get_json(http, urls.or_models.clone(), &[]).await.map(|v| parse_or_models(&v)))
        } else {
            None
        }
    };
    let oru_f = async {
        if on && !cfg.openrouter_key.trim().is_empty() {
            Some(
                get_json(http, or_url.clone(), &[("Authorization", or_auth.as_str())])
                    .await
                    .map(|v| parse_or_rankings(&v, today)),
            )
        } else {
            None
        }
    };
    let aa_f = async {
        if on && !aa_key.is_empty() {
            Some(get_json(http, urls.aa_models.clone(), &[("x-api-key", aa_key.as_str())]).await.map(|v| parse_aa(&v)))
        } else {
            None
        }
    };
    let (arena, hf, orm, oru, aa) = tokio::join!(arena_f, hf_f, orm_f, oru_f, aa_f);

    let mut status = |id: &str, label: &str, res: Option<Result<usize, String>>, detail: String| {
        if let Some(r) = res {
            sources.push(SourceStatus {
                id: id.into(),
                label: label.into(),
                ok: r.is_ok(),
                count: *r.as_ref().unwrap_or(&0),
                error: r.err(),
                fetched_at: now,
                detail,
            });
        }
    };

    // Arena
    match arena {
        Some(Ok((text, code, date))) => {
            let n = text.len() + code.len();
            out.arena_text = merge_arena(text, &base.arena_text);
            out.arena_code = merge_arena(code, &base.arena_code);
            out.arena_as_of = Some(date.clone());
            status("arena", "LMArena leaderboards", Some(Ok(n)), format!("Latest snapshot {date}, merged with bundled history"));
        }
        other => {
            let keep = prev.arena_text.len() + prev.arena_code.len() > 0 && prev.source("arena").map(|s| s.ok).unwrap_or(false);
            if keep {
                out.arena_text = prev.arena_text.clone();
                out.arena_code = prev.arena_code.clone();
                out.arena_as_of = prev.arena_as_of.clone();
            } else {
                out.arena_text = base.arena_text.clone();
                out.arena_code = base.arena_code.clone();
                out.arena_as_of = base.arena_as_of.clone();
            }
            if let Some(Err(e)) = other {
                status("arena", "LMArena leaderboards", Some(Err(e.to_string())), "Using cached/bundled data".into());
            }
        }
    }

    match hf {
        Some(Ok(v)) => {
            status(
                "huggingface",
                "Hugging Face trending & downloads",
                Some(Ok(v.len())),
                "Open-weight models: trending score, 30-day downloads".into(),
            );
            out.hf = v;
        }
        Some(Err(e)) => {
            out.hf = prev.hf.clone();
            status("huggingface", "Hugging Face trending & downloads", Some(Err(e.to_string())), "Kept previous data".into());
        }
        None => {}
    }
    match orm {
        Some(Ok(v)) => {
            status(
                "openrouter_models",
                "OpenRouter model list",
                Some(Ok(v.len())),
                "Release dates and metadata (public, no key)".into(),
            );
            out.or_models = v;
        }
        Some(Err(e)) => {
            out.or_models = prev.or_models.clone();
            status("openrouter_models", "OpenRouter model list", Some(Err(e.to_string())), "Kept previous data".into());
        }
        None => {}
    }
    match oru {
        Some(Ok(v)) => {
            status(
                "openrouter_usage",
                "OpenRouter token rankings",
                Some(Ok(v.len())),
                "Daily tokens per model, last 7 days (Data API)".into(),
            );
            out.or_usage = v;
        }
        Some(Err(e)) => {
            out.or_usage = prev.or_usage.clone();
            status("openrouter_usage", "OpenRouter token rankings", Some(Err(e.to_string())), "Kept previous data".into());
        }
        None => {}
    }
    match aa {
        Some(Ok(v)) => {
            status(
                "artificial_analysis",
                "Artificial Analysis indexes",
                Some(Ok(v.len())),
                "Intelligence & coding indexes".into(),
            );
            out.aa = v;
        }
        Some(Err(e)) => {
            out.aa = prev.aa.clone();
            status("artificial_analysis", "Artificial Analysis indexes", Some(Err(e.to_string())), "Kept previous data".into());
        }
        None => {}
    }
    out.sources = sources;
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn baseline_loads() {
        let b = Signals::baseline();
        assert!(b.arena_text.len() > 20);
        assert!(b.arena_code.len() > 20);
        assert!(b.arena_code.iter().all(|e| e.score > 500.0));
    }

    #[test]
    fn parses_sources() {
        let a = parse_arena_board(
            &json!({"meta":{},"models":[{"rank":1,"model":"x","vendor":"V","license":"open","score":1400,"ci":5,"votes":100},{"model":"bad"}]}),
        );
        assert_eq!(a.len(), 1);
        assert_eq!(a[0].score, 1400.0);

        let h = parse_hf(
            &json!([{"id":"deepseek-ai/DeepSeek-V4-Flash","downloads":12345,"likes":10,"trendingScore":88,"createdAt":"2026-09-01T00:00:00.000Z"}]),
        );
        assert_eq!(h[0].downloads, 12345);
        assert_eq!(h[0].trending, 88.0);
        assert!(h[0].created_at.unwrap() > 1_700_000_000);

        let today = NaiveDate::from_ymd_opt(2026, 11, 4).unwrap();
        let r = parse_or_rankings(
            &json!({"data":[
                {"date":"2026-11-03","model_permaslug":"z-ai/glm-5.2","total_tokens":"1000"},
                {"date":"2026-10-29","model_permaslug":"z-ai/glm-5.2","total_tokens":"500"},
                {"date":"2026-11-03","model_permaslug":"other","total_tokens":"9"}
            ],"meta":{"as_of":"x"}}),
            today,
        );
        assert_eq!(r.len(), 1);
        assert_eq!(r[0].tokens_7d, 1500.0);
        assert_eq!(r[0].tokens_recent3, 1000.0);
        assert_eq!(r[0].tokens_prior4, 500.0);

        let aa = parse_aa(
            &json!({"data":[{"name":"GLM-5.2","slug":"glm-5-2","evaluations":{"artificial_analysis_intelligence_index":61.2,"artificial_analysis_coding_index":"55"},"median_output_tokens_per_second":80,"release_date":"2026-08-01"}]}),
        );
        assert_eq!(aa[0].intelligence, Some(61.2));
        assert_eq!(aa[0].coding, Some(55.0));
        assert!(aa[0].release.is_some());
    }

    #[test]
    fn merge_keeps_history_and_marks_new_models() {
        let base =
            vec![ArenaEntry { model: "old".into(), score: 1300.0, first_seen: Some("2026-05-01".into()), ..Default::default() }];
        let fresh = vec![
            ArenaEntry { model: "old".into(), score: 1310.0, date: Some("2026-10-07".into()), ..Default::default() },
            ArenaEntry { model: "brand-new".into(), score: 1500.0, date: Some("2026-10-07".into()), ..Default::default() },
        ];
        let m = merge_arena(fresh, &base);
        assert_eq!(m[0].model, "brand-new");
        assert_eq!(m[0].first_seen.as_deref(), Some("2026-10-07"));
        assert_eq!(m[1].score, 1310.0, "fresh score wins");
        assert_eq!(m[1].first_seen.as_deref(), Some("2026-05-01"));
    }

    #[tokio::test]
    async fn refresh_offline_keeps_baseline() {
        let http = reqwest::Client::new();
        let cfg = RankingSettings { public_data: false, ..Default::default() };
        let s = refresh(&http, &cfg, &Signals::default(), &SignalUrls::default()).await;
        assert!(!s.arena_text.is_empty());
        assert_eq!(s.sources.len(), 1);
    }
}
