//! Turn public signals + your local usage into rankings and recommendations.
//!
//! Every model *you can actually call* (a provider is configured and the model
//! is not hidden) gets four public signals on a 0–100 scale:
//!
//! * **strength**: LMArena text/code score relative to the #1 model, and/or
//!   the Artificial Analysis index. Models missing from public leaderboards
//!   get a size-based estimate capped near the bottom of those lists (they
//!   never made the top lists).
//! * **popularity**: OpenRouter token volume (optional key), Hugging Face
//!   downloads and LMArena vote counts (log-scaled).
//! * **hot**: Hugging Face trending score, release recency and OpenRouter
//!   usage growth.
//! * **speed**: your measured tokens/sec, else a per-provider prior.
//!
//! Presets weight these differently, then your own success rate, speed and
//! thumbs up/down are blended in proportionally to how much data exists.

use std::collections::{BTreeMap, HashMap};

use serde::{Deserialize, Serialize};

use crate::catalog::ModelEntry;
use crate::normalize::{canon, param_billions, Canon, Matcher};
use crate::signals::Signals;
use crate::store::FamilyStats;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Hash)]
#[serde(rename_all = "lowercase")]
pub enum Preset {
    General,
    Coding,
    Reasoning,
    Fast,
}

impl Preset {
    pub const ALL: [Preset; 4] = [Preset::General, Preset::Coding, Preset::Reasoning, Preset::Fast];

    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "" | "general" | "default" | "best" => Some(Preset::General),
            "coding" | "code" | "coder" => Some(Preset::Coding),
            "reasoning" | "reason" | "think" | "thinking" | "math" => Some(Preset::Reasoning),
            "fast" | "speed" | "quick" => Some(Preset::Fast),
            _ => None,
        }
    }

    pub fn key(self) -> &'static str {
        match self {
            Preset::General => "general",
            Preset::Coding => "coding",
            Preset::Reasoning => "reasoning",
            Preset::Fast => "fast",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum View {
    Recommended(Preset),
    Strongest,
    Popular,
    Hot,
    Mine,
}

impl View {
    pub fn parse(view: &str, preset: &str) -> Self {
        match view.trim().to_ascii_lowercase().as_str() {
            "strongest" | "strength" => View::Strongest,
            "popular" | "most-used" | "most_used" | "usage" => View::Popular,
            "hot" | "trending" => View::Hot,
            "mine" | "personal" => View::Mine,
            _ => View::Recommended(Preset::parse(preset).unwrap_or(Preset::General)),
        }
    }
}

pub struct RankInput<'a> {
    pub entries: &'a [ModelEntry],
    pub signals: &'a Signals,
    pub family_stats: &'a HashMap<String, FamilyStats>,
    /// `api_id` -> (attempts, successes, tokens/sec)
    pub route_stats: &'a HashMap<String, (i64, i64, Option<f64>)>,
    /// `api_id` -> cooldown end (unix ms)
    pub cooldowns: &'a HashMap<String, i64>,
    /// provider id -> (display name, speed prior)
    pub providers: &'a HashMap<String, (String, f64)>,
    pub personal_weight: f64,
    pub now_ms: i64,
}

#[derive(Clone, Debug, Serialize)]
pub struct Route {
    pub api_id: String,
    pub provider: String,
    pub provider_name: String,
    pub model: String,
    pub context_length: Option<u64>,
    pub cooling_until: Option<i64>,
    pub attempts: i64,
    pub success_rate: Option<f64>,
    pub tokens_per_sec: Option<f64>,
}

#[derive(Clone, Debug, Serialize)]
pub struct Personal {
    pub uses: i64,
    pub attempts: i64,
    pub success_rate: f64,
    pub tokens_per_sec: Option<f64>,
    pub thumbs_up: i64,
    pub thumbs_down: i64,
    pub score: f64,
    pub confidence: f64,
    pub last_used: Option<i64>,
}

#[derive(Clone, Debug, Serialize)]
pub struct RankedModel {
    pub rank: usize,
    pub family: String,
    pub name: String,
    pub routes: Vec<Route>,
    pub strength: f64,
    pub strength_estimated: bool,
    pub coding: f64,
    pub popularity: Option<f64>,
    pub hot: Option<f64>,
    pub speed: f64,
    /// Unix seconds.
    pub released: Option<i64>,
    pub personal: Option<Personal>,
    /// Score in the requested view.
    pub score: f64,
    /// Recommendation score per preset.
    pub presets: BTreeMap<&'static str, f64>,
    pub available: bool,
    pub badges: Vec<String>,
    pub why: Vec<String>,
}

fn clamp(x: f64) -> f64 {
    x.clamp(0.0, 100.0)
}

fn log_norm(v: f64, max: f64) -> f64 {
    if v <= 0.0 || max <= 0.0 {
        return 0.0;
    }
    clamp(100.0 * (1.0 + v).ln() / (1.0 + max).ln())
}

pub fn speed_score(tps: f64) -> f64 {
    log_norm(tps, 1000.0)
}

fn weighted(parts: &[(Option<f64>, f64)]) -> Option<f64> {
    let (mut s, mut w) = (0.0, 0.0);
    for (v, wt) in parts {
        if let Some(v) = v {
            s += v * wt;
            w += wt;
        }
    }
    (w > 0.0).then(|| s / w)
}

fn mean(v: &[f64]) -> Option<f64> {
    (!v.is_empty()).then(|| v.iter().sum::<f64>() / v.len() as f64)
}

fn fmt_tokens(t: f64) -> String {
    if t >= 1e9 {
        format!("{:.1}B", t / 1e9)
    } else if t >= 1e6 {
        format!("{:.1}M", t / 1e6)
    } else if t >= 1e3 {
        format!("{:.0}K", t / 1e3)
    } else {
        format!("{t:.0}")
    }
}

const TEXT_ELO_PER_POINT: f64 = 5.0;
const CODE_ELO_PER_POINT: f64 = 8.0;

/// Compute every family's signals and preset scores (unsorted, rank 0).
pub fn evaluate(inp: &RankInput) -> Vec<RankedModel> {
    let sig = inp.signals;
    let now_s = inp.now_ms / 1000;

    let m_text = Matcher::new(sig.arena_text.iter().map(|e| e.model.as_str()));
    let m_code = Matcher::new(sig.arena_code.iter().map(|e| e.model.as_str()));
    let m_hf = Matcher::new(sig.hf.iter().map(|e| e.id.as_str()));
    let m_orm = Matcher::new(sig.or_models.iter().map(|e| e.id.as_str()));
    let m_oru = Matcher::new(sig.or_usage.iter().map(|e| e.slug.as_str()));
    let m_aa_slug = Matcher::new(sig.aa.iter().map(|e| e.slug.as_str()));
    let m_aa_name = Matcher::new(sig.aa.iter().map(|e| e.name.as_str()));

    let top_text = sig.arena_text.iter().map(|e| e.score).fold(f64::MIN, f64::max);
    let top_code = sig.arena_code.iter().map(|e| e.score).fold(f64::MIN, f64::max);
    let norm_text = |elo: f64| clamp(100.0 - (top_text - elo) / TEXT_ELO_PER_POINT);
    let norm_code = |elo: f64| clamp(100.0 - (top_code - elo) / CODE_ELO_PER_POINT);
    // Models absent from the leaderboards' top lists are, roughly, weaker than
    // the bottom of those lists: cap estimates at the 10th percentile.
    let floor = {
        let pct10 = |mut v: Vec<f64>| -> Option<f64> {
            if v.is_empty() {
                return None;
            }
            v.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
            Some(v[v.len() / 10])
        };
        let code = pct10(sig.arena_code.iter().map(|e| norm_code(e.score)).collect());
        let text = pct10(sig.arena_text.iter().map(|e| norm_text(e.score)).collect());
        code.or(text).unwrap_or(30.0).clamp(10.0, 40.0)
    };
    let max_aa_int = sig.aa.iter().filter_map(|e| e.intelligence).fold(0.0, f64::max);
    let max_aa_code = sig.aa.iter().filter_map(|e| e.coding).fold(0.0, f64::max);
    let max_or_tokens = sig.or_usage.iter().map(|e| e.tokens_7d).fold(0.0, f64::max);
    let max_hf_dl = sig.hf.iter().map(|e| e.downloads as f64).fold(0.0, f64::max);
    let max_hf_trend = sig.hf.iter().map(|e| e.trending).fold(0.0, f64::max);
    let max_votes =
        sig.arena_text.iter().chain(sig.arena_code.iter()).filter_map(|e| e.votes).map(|v| v as f64).fold(0.0, f64::max);
    let as_of = sig.arena_as_of.clone().unwrap_or_default();

    let mut families: BTreeMap<&str, Vec<&ModelEntry>> = BTreeMap::new();
    for e in inp.entries {
        if !e.family.is_empty() {
            families.entry(e.family.as_str()).or_default().push(e);
        }
    }

    let mut out = Vec::with_capacity(families.len());
    for (family, entries) in families {
        let c: Canon = canon(&entries[0].model);
        let mut why = Vec::new();

        // ---- strength
        let text_hits = m_text.find(&c);
        let code_hits = m_code.find(&c);
        let elo_text = mean(&text_hits.iter().map(|&i| sig.arena_text[i].score).collect::<Vec<_>>());
        let elo_code = mean(&code_hits.iter().map(|&i| sig.arena_code[i].score).collect::<Vec<_>>());
        let mut aa_hits = m_aa_slug.find(&c);
        if aa_hits.is_empty() {
            aa_hits = m_aa_name.find(&c);
        }
        let aa_int = mean(&aa_hits.iter().filter_map(|&i| sig.aa[i].intelligence).collect::<Vec<_>>())
            .filter(|_| max_aa_int > 0.0)
            .map(|v| clamp(100.0 * v / max_aa_int));
        let aa_code = mean(&aa_hits.iter().filter_map(|&i| sig.aa[i].coding).collect::<Vec<_>>())
            .filter(|_| max_aa_code > 0.0)
            .map(|v| clamp(100.0 * v / max_aa_code));
        let s_text = elo_text.map(norm_text);
        let s_code = elo_code.map(norm_code);
        if let Some(e) = elo_text {
            why.push(format!("LMArena text score {e:.0} (#1 is {top_text:.0}; data as of {as_of})"));
        }
        if let Some(e) = elo_code {
            why.push(format!("LMArena code score {e:.0} (#1 is {top_code:.0})"));
        }
        if let Some(v) = aa_int {
            why.push(format!("Artificial Analysis intelligence {v:.0}/100 of the top model"));
        }

        // ---- release date
        let or_hits = m_orm.find(&c);
        let hf_hits = m_hf.find(&c);
        let arena_first_seen = text_hits
            .iter()
            .filter_map(|&i| sig.arena_text[i].first_seen.as_deref())
            .chain(code_hits.iter().filter_map(|&i| sig.arena_code[i].first_seen.as_deref()))
            .filter_map(crate::signals::parse_time)
            .min();
        let released = or_hits
            .iter()
            .filter_map(|&i| sig.or_models[i].created)
            .max()
            .or_else(|| aa_hits.iter().filter_map(|&i| sig.aa[i].release).max())
            .or_else(|| hf_hits.iter().filter_map(|&i| sig.hf[i].created_at).min())
            .or(arena_first_seen)
            .or_else(|| entries.iter().filter_map(|e| e.created).max());
        let age_days = released.map(|r| ((now_s - r) as f64 / 86_400.0).max(0.0));

        let mut strength_estimated = false;
        let strength = match weighted(&[(s_text, 1.0), (s_code, 0.5), (aa_int, 1.0)]) {
            Some(s) => s,
            None => {
                strength_estimated = true;
                let size = param_billions(&c).map(|b| (b.max(1.0).ln() / 1000f64.ln()).clamp(0.2, 1.0)).unwrap_or(0.55);
                let mut est = 5.0 + (floor - 5.0) * size;
                if age_days.map(|a| a < 45.0).unwrap_or(false) {
                    est = est.max(floor + 3.0);
                    why.push("New release, not on public leaderboards yet: strength is a rough estimate".into());
                } else {
                    why.push("Not in public leaderboard top lists: strength estimated from model size".into());
                }
                est
            }
        };
        let coding = weighted(&[(s_code, 1.0), (aa_code, 1.0), (s_text, 0.35)]).unwrap_or(strength);

        // ---- popularity
        let oru_hits = m_oru.find(&c);
        let or_tokens: f64 = oru_hits.iter().map(|&i| sig.or_usage[i].tokens_7d).sum();
        let p_or = (!oru_hits.is_empty()).then(|| log_norm(or_tokens, max_or_tokens));
        let hf_dl: f64 = hf_hits.iter().map(|&i| sig.hf[i].downloads as f64).sum();
        let p_hf = (!hf_hits.is_empty() && hf_dl > 0.0).then(|| log_norm(hf_dl, max_hf_dl));
        let votes: f64 = text_hits
            .iter()
            .filter_map(|&i| sig.arena_text[i].votes)
            .chain(code_hits.iter().filter_map(|&i| sig.arena_code[i].votes))
            .map(|v| v as f64)
            .sum();
        let p_votes = (votes > 0.0).then(|| log_norm(votes, max_votes));
        let popularity = weighted(&[(p_or, 0.6), (p_hf, 0.25), (p_votes, 0.15)]);
        if or_tokens > 0.0 {
            why.push(format!("OpenRouter: {} tokens in the last 7 days", fmt_tokens(or_tokens)));
        }
        if hf_dl > 0.0 {
            why.push(format!("Hugging Face: {} downloads in 30 days", fmt_tokens(hf_dl)));
        }
        if votes > 0.0 {
            why.push(format!("LMArena: {} community votes", fmt_tokens(votes)));
        }

        // ---- hotness
        let trend = hf_hits.iter().map(|&i| sig.hf[i].trending).fold(0.0, f64::max);
        let h_hf = (trend > 0.0 && max_hf_trend > 0.0).then(|| clamp(100.0 * (trend / max_hf_trend).sqrt()));
        let h_new = age_days.map(|a| clamp(100.0 * (-a / 75.0).exp()));
        let h_or = if oru_hits.is_empty() {
            None
        } else {
            let r3: f64 = oru_hits.iter().map(|&i| sig.or_usage[i].tokens_recent3).sum::<f64>() / 3.0;
            let p4: f64 = oru_hits.iter().map(|&i| sig.or_usage[i].tokens_prior4).sum::<f64>() / 4.0;
            if p4 > 0.0 {
                Some(clamp(50.0 + 30.0 * (r3.max(1.0) / p4).log2()))
            } else if r3 > 0.0 {
                Some(85.0)
            } else {
                None
            }
        };
        let hot = weighted(&[(h_hf, 0.4), (h_new, 0.35), (h_or, 0.25)]);
        if trend > 0.0 {
            why.push(format!("Hugging Face trending score {trend:.0}"));
        }
        if let Some(a) = age_days {
            why.push(format!("Released about {} days ago", a.round() as i64));
        }

        // ---- routes & speed
        let mut routes: Vec<Route> = entries
            .iter()
            .map(|e| {
                let rs = inp.route_stats.get(&e.api_id);
                let (pname, _) = inp.providers.get(&e.provider).cloned().unwrap_or((e.provider.clone(), 50.0));
                Route {
                    api_id: e.api_id.clone(),
                    provider: e.provider.clone(),
                    provider_name: pname,
                    model: e.model.clone(),
                    context_length: e.context_length,
                    cooling_until: inp.cooldowns.get(&e.api_id).copied().filter(|t| *t > inp.now_ms),
                    attempts: rs.map(|r| r.0).unwrap_or(0),
                    success_rate: rs.filter(|r| r.0 > 0).map(|r| r.1 as f64 / r.0 as f64),
                    tokens_per_sec: rs.and_then(|r| r.2),
                }
            })
            .collect();
        let prior_of = |r: &Route| inp.providers.get(&r.provider).map(|p| p.1).unwrap_or(50.0);
        routes.sort_by(|a, b| {
            let key = |r: &Route| {
                let smoothed = (r.success_rate.unwrap_or(0.8) * r.attempts as f64 + 0.8 * 2.0) / (r.attempts as f64 + 2.0);
                let spd = r.tokens_per_sec.map(speed_score).unwrap_or_else(|| prior_of(r));
                (r.cooling_until.is_none(), smoothed + spd / 1000.0)
            };
            let (ka, kb) = (key(a), key(b));
            kb.0.cmp(&ka.0).then(kb.1.partial_cmp(&ka.1).unwrap_or(std::cmp::Ordering::Equal))
        });
        let available = routes.iter().any(|r| r.cooling_until.is_none());
        let size_adj = match param_billions(&c) {
            Some(b) if b < 15.0 => 5.0,
            Some(b) if b > 200.0 => -8.0,
            _ => 0.0,
        };
        let prior = routes.iter().map(&prior_of).fold(0.0, f64::max) + size_adj;
        let measured = routes
            .iter()
            .filter(|r| r.attempts >= 3)
            .filter_map(|r| r.tokens_per_sec)
            .fold(None, |acc: Option<f64>, t| Some(acc.map_or(t, |a| a.max(t))));
        let speed = clamp(match measured {
            Some(t) => 0.7 * speed_score(t) + 0.3 * prior,
            None => prior,
        });

        // ---- personal
        let personal = inp.family_stats.get(family).map(|f| {
            let n = f.attempts as f64;
            let success = (f.successes as f64 + 1.0) / (n + 2.0);
            let rating = (f.thumbs_up as f64 + 1.0) / ((f.thumbs_up + f.thumbs_down) as f64 + 2.0);
            let spd = f.tokens_per_sec.map(|t| speed_score(t) / 100.0).unwrap_or(0.5);
            let score = 100.0 * (0.45 * success + 0.30 * rating + 0.25 * spd);
            let confidence = ((n + 3.0 * (f.thumbs_up + f.thumbs_down) as f64) / 20.0).min(1.0);
            Personal {
                uses: f.successes,
                attempts: f.attempts,
                success_rate: if f.attempts > 0 { f.successes as f64 / n } else { 1.0 },
                tokens_per_sec: f.tokens_per_sec,
                thumbs_up: f.thumbs_up,
                thumbs_down: f.thumbs_down,
                score,
                confidence,
                last_used: f.last_used,
            }
        });
        if let Some(p) = &personal {
            if p.attempts > 0 {
                let mut s = format!("You: {} calls, {:.0}% success", p.attempts, p.success_rate * 100.0);
                if let Some(t) = p.tokens_per_sec {
                    s.push_str(&format!(", {t:.0} tok/s"));
                }
                if p.thumbs_up + p.thumbs_down > 0 {
                    s.push_str(&format!(", 👍 {} 👎 {}", p.thumbs_up, p.thumbs_down));
                }
                why.push(s);
            }
        }
        if !available {
            why.push("Rate-limited on every provider right now (cooling down)".into());
        }

        // ---- presets
        let ids_lower: Vec<String> = entries.iter().map(|e| e.model.to_ascii_lowercase()).collect();
        let has = |needles: &[&str]| ids_lower.iter().any(|id| needles.iter().any(|n| id.contains(n)));
        let code_bonus = if has(&["code", "coder", "codex", "devstral", "codestral"]) { 8.0 } else { 0.0 };
        let reason_bonus =
            if has(&["r1", "thinking", "reasoner", "reasoning", "qwq", "magistral", "-think"]) { 5.0 } else { 0.0 };
        let pop = popularity.unwrap_or(15.0);
        let hotv = hot.unwrap_or(15.0);
        let pw = inp.personal_weight.clamp(0.0, 1.0);
        let blend = |public: f64| -> f64 {
            let mut v = match &personal {
                Some(p) => (1.0 - pw * p.confidence) * public + pw * p.confidence * p.score,
                None => public,
            };
            if !available {
                v -= 25.0;
            }
            clamp(v)
        };
        let mut presets = BTreeMap::new();
        presets.insert("general", blend(0.55 * strength + 0.25 * pop + 0.20 * hotv));
        presets.insert("coding", blend(0.60 * coding + 0.20 * pop + 0.20 * hotv + code_bonus));
        presets.insert("reasoning", blend(0.75 * strength + 0.15 * pop + 0.10 * hotv + reason_bonus));
        // speed dominates; quality scales it so a fast-but-weak model does not always win
        presets.insert("fast", blend(speed * (0.65 + 0.35 * strength / 100.0)));

        let name = entries
            .iter()
            .map(|e| e.name.as_str())
            .min_by(|a, b| a.len().cmp(&b.len()).then(a.cmp(b)))
            .unwrap_or(family)
            .to_string();

        out.push(RankedModel {
            rank: 0,
            family: family.to_string(),
            name,
            routes,
            strength,
            strength_estimated,
            coding,
            popularity,
            hot,
            speed,
            released,
            personal,
            score: 0.0,
            presets,
            available,
            badges: vec![],
            why,
        });
    }

    // ---- badges (computed over models you can use right now)
    let mut award = |label: &str, f: &dyn Fn(&RankedModel) -> Option<f64>| {
        let best = out
            .iter()
            .enumerate()
            .filter(|(_, m)| m.available)
            .filter_map(|(i, m)| f(m).map(|v| (i, v)))
            .max_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal));
        if let Some((i, _)) = best {
            out[i].badges.push(label.to_string());
        }
    };
    award("Best pick", &|m| m.presets.get("general").copied());
    award("Strongest", &|m| Some(m.strength));
    award("Best for coding", &|m| m.presets.get("coding").copied());
    award("Most used", &|m| m.popularity);
    award("Trending", &|m| m.hot);
    award("Fastest", &|m| Some(m.speed));
    out
}

pub fn rank(inp: &RankInput, view: View) -> Vec<RankedModel> {
    let mut all = evaluate(inp);
    let key = |m: &RankedModel| -> f64 {
        match view {
            View::Recommended(p) => m.presets[p.key()],
            View::Strongest => m.strength,
            View::Popular => m.popularity.unwrap_or(-1.0),
            View::Hot => m.hot.unwrap_or(-1.0),
            View::Mine => m.personal.as_ref().map(|p| p.uses as f64 + p.score / 1000.0).unwrap_or(-1.0),
        }
    };
    if view == View::Mine {
        all.retain(|m| m.personal.as_ref().map(|p| p.attempts > 0 || p.thumbs_up + p.thumbs_down > 0).unwrap_or(false));
    }
    all.sort_by(|a, b| {
        key(b)
            .partial_cmp(&key(a))
            .unwrap_or(std::cmp::Ordering::Equal)
            .then(b.strength.partial_cmp(&a.strength).unwrap_or(std::cmp::Ordering::Equal))
            .then(a.family.cmp(&b.family))
    });
    for (i, m) in all.iter_mut().enumerate() {
        m.rank = i + 1;
        m.score = match view {
            View::Mine => m.personal.as_ref().map(|p| p.score).unwrap_or(0.0),
            _ => key(m).max(0.0),
        };
    }
    all
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::signals::{ArenaEntry, HfEntry};

    fn entry(p: &str, m: &str) -> ModelEntry {
        ModelEntry::new(p, m, None, None)
    }

    fn arena(model: &str, score: f64, votes: i64) -> ArenaEntry {
        ArenaEntry { model: model.into(), score, votes: Some(votes), ..Default::default() }
    }

    fn input<'a>(
        entries: &'a [ModelEntry],
        signals: &'a Signals,
        stats: &'a HashMap<String, FamilyStats>,
        routes: &'a HashMap<String, (i64, i64, Option<f64>)>,
        cooldowns: &'a HashMap<String, i64>,
        providers: &'a HashMap<String, (String, f64)>,
    ) -> RankInput<'a> {
        RankInput {
            entries,
            signals,
            family_stats: stats,
            route_stats: routes,
            cooldowns,
            providers,
            personal_weight: 0.35,
            now_ms: 1_790_000_000_000,
        }
    }

    fn providers() -> HashMap<String, (String, f64)> {
        [("groq", 92.0), ("cerebras", 97.0), ("nvidia", 50.0), ("gemini", 72.0)]
            .iter()
            .map(|(k, v)| (k.to_string(), (k.to_string(), *v)))
            .collect()
    }

    #[test]
    fn groups_providers_and_orders_by_strength() {
        let entries = vec![
            entry("groq", "llama-3.3-70b-versatile"),
            entry("cerebras", "llama3.3-70b"),
            entry("nvidia", "deepseek-ai/deepseek-v4-flash"),
            entry("gemini", "gemini-3.6-flash"),
        ];
        let signals = Signals {
            arena_text: vec![arena("frontier-x", 1520.0, 50_000)],
            arena_code: vec![
                arena("frontier-y", 1800.0, 9_000),
                arena("deepseek-v4-flash-high", 1581.0, 4_000),
                arena("gemini-3.6-flash", 1531.0, 6_000),
                arena("granite-4.1-8b", 1194.0, 100),
            ],
            hf: vec![HfEntry {
                id: "deepseek-ai/DeepSeek-V4-Flash".into(),
                downloads: 900_000,
                trending: 300.0,
                ..Default::default()
            }],
            ..Default::default()
        };
        let (stats, routes, cds, provs) = (HashMap::new(), HashMap::new(), HashMap::new(), providers());
        let inp = input(&entries, &signals, &stats, &routes, &cds, &provs);
        let r = rank(&inp, View::Strongest);
        assert_eq!(r.len(), 3, "llama on two providers is one model");
        assert_eq!(r[0].family, "deepseek-v4-flash");
        assert_eq!(r[1].family, "gemini-3.6-flash");
        let llama = r.iter().find(|m| m.family == "llama-3.3-70b").unwrap();
        assert!(llama.strength_estimated);
        assert!(llama.strength < r[1].strength);
        assert_eq!(llama.routes.len(), 2);
        assert_eq!(llama.routes[0].provider, "cerebras", "faster provider first when no history");
        assert!(r[0].badges.contains(&"Strongest".to_string()));
        let hot = rank(&inp, View::Hot);
        assert_eq!(hot[0].family, "deepseek-v4-flash");
        let fast = rank(&inp, View::Recommended(Preset::Fast));
        assert_eq!(fast[0].family, "llama-3.3-70b", "Cerebras/Groq speed wins the fast preset");
    }

    #[test]
    fn personal_history_and_cooldowns_matter() {
        let entries = vec![entry("groq", "model-a-70b"), entry("groq", "model-b-70b")];
        let signals = Signals::default();
        let mut stats = HashMap::new();
        stats.insert(
            "model-b-70b".to_string(),
            FamilyStats {
                family: "model-b-70b".into(),
                attempts: 40,
                successes: 40,
                thumbs_up: 6,
                tokens_per_sec: Some(400.0),
                ..Default::default()
            },
        );
        stats.insert(
            "model-a-70b".to_string(),
            FamilyStats { family: "model-a-70b".into(), attempts: 40, successes: 10, thumbs_down: 5, ..Default::default() },
        );
        let (routes, provs) = (HashMap::new(), providers());
        let cds = HashMap::new();
        let inp = input(&entries, &signals, &stats, &routes, &cds, &provs);
        let r = rank(&inp, View::Recommended(Preset::General));
        assert_eq!(r[0].family, "model-b-70b");
        let mine = rank(&inp, View::Mine);
        assert_eq!(mine.len(), 2);

        let mut cds = HashMap::new();
        cds.insert("groq/model-b-70b".to_string(), inp.now_ms + 30_000);
        let inp2 = input(&entries, &signals, &stats, &routes, &cds, &provs);
        let r = rank(&inp2, View::Recommended(Preset::General));
        assert_eq!(r[0].family, "model-a-70b", "cooling model drops below");
        assert!(!r[1].available);
    }

    #[test]
    fn view_parse() {
        assert_eq!(View::parse("hot", ""), View::Hot);
        assert_eq!(View::parse("recommended", "coding"), View::Recommended(Preset::Coding));
        assert_eq!(View::parse("", "nonsense"), View::Recommended(Preset::General));
    }
}
