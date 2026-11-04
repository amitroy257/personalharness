//! Canonical model keys, so the same model can be recognised across providers
//! and public leaderboards, e.g. Groq `llama-3.3-70b-versatile`, Cerebras
//! `llama3.3-70b`, NVIDIA `meta/llama-3.3-70b-instruct` and Hugging Face
//! `meta-llama/Llama-3.3-70B-Instruct` all map to `llama-3.3-70b`.

use std::collections::{BTreeSet, HashMap};
use std::sync::LazyLock;

use regex::Regex;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Canon {
    /// All meaningful tokens, e.g. `qwen-3-235b-a22b`.
    pub primary: String,
    /// Primary without size tokens, e.g. `qwen-3`.
    pub secondary: String,
    pub sizes: BTreeSet<String>,
    pub tokens: Vec<String>,
    /// Had reasoning-effort / mode suffixes (high, thinking, max, ...).
    pub variant: bool,
}

const NOISE: &[&str] = &[
    "instruct",
    "chat",
    "it",
    "versatile",
    "latest",
    "preview",
    "exp",
    "experimental",
    "beta",
    "fp8",
    "fp16",
    "bf16",
    "int4",
    "int8",
    "awq",
    "gguf",
    "nvfp4",
    "mlx",
    "hf",
    "free",
    "online",
    "w4a16",
    "w8a8",
    "gptq",
    "dynamic",
    "fp4",
    "q4",
    "q8",
];
const QUANT: &[&str] = &["fp8", "fp16", "bf16", "int4", "int8", "awq", "nvfp4", "fp4", "w4a16", "w8a8", "gptq"];
const RESETS_NUMERIC: &[&str] = &["preview", "exp", "experimental", "beta", "latest"];
const VARIANT: &[&str] =
    &["high", "low", "medium", "max", "xhigh", "minimal", "thinking", "non", "nothink", "reasoning", "instant", "think"];
const VENDOR_PREFIX: &[&str] = &["meta", "zai", "nvidia"];

static RE_PAREN: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\([^)]*\)").unwrap());
static RE_DATE_LONG: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\d{4}-\d{2}-\d{2}").unwrap());
static RE_DATE_SHORT: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(^|-)\d{2}-\d{2}-\d{2}($|-)").unwrap());
static RE_LETTER_DIGIT: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^([a-z]{2,})(\d.*)$").unwrap());
static RE_SIZE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^(\d+(\.\d+)?[bm]|a\d+(\.\d+)?b|\d+e|\d+x\d+(\.\d+)?b)$").unwrap());
static RE_BUDGET: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^\d+k$").unwrap());
static RE_VERSION: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^v?\d+(\.\d+)*$").unwrap());

/// Strip provider/vendor prefixes: `@cf/meta/llama-3` -> `llama-3`,
/// `models/gemini-3-flash` -> `gemini-3-flash`, `qwen/qwen3:free` -> `qwen3`.
pub fn base_name(raw: &str) -> &str {
    let s = raw.trim();
    let s = s.rsplit('/').next().unwrap_or(s);
    s.split(':').next().unwrap_or(s)
}

pub fn canon(raw: &str) -> Canon {
    let mut s = raw.trim().to_ascii_lowercase();
    let mut variant = false;
    if RE_PAREN.is_match(&s) {
        variant = true;
        s = RE_PAREN.replace_all(&s, "").to_string();
    }
    let mut s = base_name(&s).to_string();
    s = RE_DATE_LONG.replace_all(&s, "").to_string();
    s = RE_DATE_SHORT.replace_all(&s, "-").to_string();
    let s = s.replace(['_', ' ', '/'], "-");

    let mut tokens: Vec<String> = Vec::new();
    let mut drop_numeric = false;
    let mut prev_raw = String::new();
    for raw_tok in s.split('-').filter(|t| !t.is_empty()) {
        let tok = raw_tok.trim_matches('.');
        if tok.is_empty() {
            continue;
        }
        if NOISE.contains(&tok) {
            if RESETS_NUMERIC.contains(&tok) {
                drop_numeric = true;
            }
            prev_raw = tok.to_string();
            continue;
        }
        if tok == "fast" && QUANT.contains(&prev_raw.as_str()) {
            prev_raw = tok.to_string();
            continue;
        }
        let is_num = tok.chars().all(|c| c.is_ascii_digit());
        if is_num && (drop_numeric || tok.len() >= 4) {
            prev_raw = tok.to_string();
            continue;
        }
        if VARIANT.contains(&tok) || RE_BUDGET.is_match(tok) {
            variant = true;
            prev_raw = tok.to_string();
            continue;
        }
        drop_numeric = false;
        prev_raw = tok.to_string();
        if let Some(c) = RE_LETTER_DIGIT.captures(tok) {
            tokens.push(c[1].to_string());
            tokens.push(c[2].to_string());
        } else {
            tokens.push(tok.to_string());
        }
    }

    if tokens.len() > 1 && VENDOR_PREFIX.contains(&tokens[0].as_str()) {
        tokens.remove(0);
    }

    // `4`,`5` -> `4.5` (claude-opus-4-5, grok-4-1)
    let mut merged: Vec<String> = Vec::with_capacity(tokens.len());
    for t in tokens {
        let small_int = t.len() <= 2 && t.chars().all(|c| c.is_ascii_digit());
        if small_int {
            if let Some(last) = merged.last_mut() {
                let last_is_ver = !last.is_empty()
                    && last.chars().all(|c| c.is_ascii_digit() || c == '.')
                    && last.split('.').all(|p| p.len() <= 2);
                if last_is_ver {
                    last.push('.');
                    last.push_str(&t);
                    continue;
                }
            }
        }
        merged.push(t);
    }

    let sizes: BTreeSet<String> = merged.iter().filter(|t| RE_SIZE.is_match(t)).cloned().collect();
    let secondary: Vec<&str> = merged.iter().filter(|t| !RE_SIZE.is_match(t)).map(|s| s.as_str()).collect();
    Canon { primary: merged.join("-"), secondary: secondary.join("-"), sizes, tokens: merged, variant }
}

/// Index over a list of named things (leaderboard rows, HF repos, ...).
pub struct Matcher {
    canons: Vec<Canon>,
    by_primary: HashMap<String, Vec<usize>>,
    by_secondary: HashMap<String, Vec<usize>>,
}

impl Matcher {
    pub fn new<'a>(names: impl IntoIterator<Item = &'a str>) -> Self {
        let canons: Vec<Canon> = names.into_iter().map(canon).collect();
        let mut by_primary: HashMap<String, Vec<usize>> = HashMap::new();
        let mut by_secondary: HashMap<String, Vec<usize>> = HashMap::new();
        for (i, c) in canons.iter().enumerate() {
            if c.primary.is_empty() {
                continue;
            }
            by_primary.entry(c.primary.clone()).or_default().push(i);
            by_secondary.entry(c.secondary.clone()).or_default().push(i);
        }
        Self { canons, by_primary, by_secondary }
    }

    /// Indices of entries that denote the same model as `query`. When several
    /// rows match (e.g. `x` and `x-high`), plain (non-variant) rows win.
    pub fn find(&self, query: &Canon) -> Vec<usize> {
        if query.primary.is_empty() {
            return vec![];
        }
        if let Some(hits) = self.by_primary.get(&query.primary) {
            return self.prefer_plain(hits);
        }
        // Same family, sizes omitted on one side (llama-4-maverick vs llama-4-maverick-17b-128e).
        if let Some(hits) = self.by_secondary.get(&query.secondary) {
            let size_sets: BTreeSet<&BTreeSet<String>> = hits.iter().map(|&i| &self.canons[i].sizes).collect();
            if size_sets.len() == 1 {
                let set = *size_sets.iter().next().unwrap();
                if set.is_subset(&query.sizes) {
                    return self.prefer_plain(hits);
                }
            }
        }
        // `mistral-large` (alias of the latest) -> `mistral-large-3`: entries whose
        // primary extends the query only with version-like tokens; newest wins.
        let qt = &query.tokens;
        let mut best: Option<(Vec<u64>, Vec<usize>)> = None;
        for hits in self.by_primary.values() {
            let c = &self.canons[hits[0]];
            if c.tokens.len() <= qt.len() || c.tokens[..qt.len()] != qt[..] {
                continue;
            }
            let extra = &c.tokens[qt.len()..];
            if !extra.iter().all(|t| RE_VERSION.is_match(t)) {
                continue;
            }
            let key: Vec<u64> = extra
                .iter()
                .flat_map(|t| t.trim_start_matches('v').split('.').map(|n| n.parse::<u64>().unwrap_or(0)).collect::<Vec<_>>())
                .collect();
            if best.as_ref().map(|(k, _)| key > *k).unwrap_or(true) {
                best = Some((key, hits.clone()));
            }
        }
        best.map(|(_, h)| self.prefer_plain(&h)).unwrap_or_default()
    }

    pub fn find_name(&self, name: &str) -> Vec<usize> {
        self.find(&canon(name))
    }

    fn prefer_plain(&self, hits: &[usize]) -> Vec<usize> {
        let plain: Vec<usize> = hits.iter().copied().filter(|&i| !self.canons[i].variant).collect();
        if plain.is_empty() {
            hits.to_vec()
        } else {
            plain
        }
    }
}

/// Parameter count in billions, if the name says (MoE: geometric mean of
/// total and active, which tracks quality better than either alone).
pub fn param_billions(c: &Canon) -> Option<f64> {
    let mut total: Option<f64> = None;
    let mut active: Option<f64> = None;
    for s in &c.sizes {
        if let Some(rest) = s.strip_prefix('a') {
            active = rest.trim_end_matches('b').parse().ok();
        } else if let Some((n, b)) = s.split_once('x') {
            let n: f64 = n.parse().unwrap_or(1.0);
            let b: f64 = b.trim_end_matches('b').parse().unwrap_or(0.0);
            total = Some(n * b);
        } else if let Some(v) = s.strip_suffix('b') {
            let v: f64 = v.parse().unwrap_or(0.0);
            total = Some(total.map_or(v, |t: f64| t.max(v)));
        } else if let Some(v) = s.strip_suffix('m') {
            total = Some(v.parse::<f64>().unwrap_or(0.0) / 1000.0);
        }
    }
    match (total, active) {
        (Some(t), Some(a)) if t > 0.0 && a > 0.0 => Some((t * a).sqrt()),
        (Some(t), _) if t > 0.0 => Some(t),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(s: &str) -> String {
        canon(s).primary
    }

    #[test]
    fn same_model_across_providers() {
        let want = "llama-3.3-70b";
        for id in [
            "llama-3.3-70b-versatile",
            "llama3.3-70b",
            "meta/llama-3.3-70b-instruct",
            "meta-llama/Llama-3.3-70B-Instruct",
            "@cf/meta/llama-3.3-70b-instruct-fp8-fast",
            "meta-llama-3.3-70b-instruct",
        ] {
            assert_eq!(p(id), want, "{id}");
        }
    }

    #[test]
    fn strips_dates_variants_noise() {
        assert_eq!(p("qwen3-235b-a22b-instruct-2507"), "qwen-3-235b-a22b");
        assert_eq!(p("qwen-3-235b-a22b-instruct-2507"), "qwen-3-235b-a22b");
        assert_eq!(p("Qwen/Qwen3-235B-A22B-Instruct-2507"), "qwen-3-235b-a22b");
        assert_eq!(p("claude-opus-4-5-20251101-thinking-32k"), "claude-opus-4.5");
        assert_eq!(p("gpt-4.1-2025-04-14"), "gpt-4.1");
        assert_eq!(p("openai/gpt-4.1"), "gpt-4.1");
        assert_eq!(p("deepseek-v4-pro-high-20260813"), "deepseek-v4-pro");
        assert_eq!(p("deepseek-ai/DeepSeek-V4-Pro"), "deepseek-v4-pro");
        assert_eq!(p("gemini-3-flash (thinking-minimal)"), "gemini-3-flash");
        assert_eq!(p("models/gemini-3.6-flash"), "gemini-3.6-flash");
        assert_eq!(p("gemini-3.6-flash-preview-09-2026"), "gemini-3.6-flash");
        assert_eq!(p("glm-5.2 (max)"), "glm-5.2");
        assert_eq!(p("zai-glm-4.7"), "glm-4.7");
        assert_eq!(p("z-ai/glm-4.7"), "glm-4.7");
        assert_eq!(p("nvidia-nemotron-3-ultra-550b-a55b-nvfp4"), "nemotron-3-ultra-550b-a55b");
        assert_eq!(p("nvidia/nemotron-3-ultra-550b-a55b:free"), "nemotron-3-ultra-550b-a55b");
        assert_eq!(p("moonshotai/kimi-k2-instruct-0905"), "kimi-k2");
        assert_eq!(p("kimi-k2-0905-preview"), "kimi-k2");
        assert_eq!(p("grok-4-1-fast-reasoning"), "grok-4.1-fast");
        assert_eq!(p("mimo-v2-flash (non-thinking)"), "mimo-v2-flash");
        assert_eq!(p("gpt-oss-120b"), "gpt-oss-120b");
        assert_eq!(p("openai/gpt-oss-120b"), "gpt-oss-120b");
        assert_eq!(p("gemma-4-26b-a4b"), "gemma-4-26b-a4b");
        assert_eq!(p("google/gemma-3-27b-it"), "gemma-3-27b");
        assert_eq!(p("llama-3.1-8b-instant"), "llama-3.1-8b");
        assert_eq!(p("llama3.1-8b"), "llama-3.1-8b");
    }

    #[test]
    fn variant_flag() {
        assert!(canon("gemini-3.6-flash-high").variant);
        assert!(!canon("gemini-3.6-flash").variant);
        assert_eq!(canon("gemini-3.6-flash-high").primary, "gemini-3.6-flash");
    }

    #[test]
    fn matcher_prefers_plain_and_handles_sizes() {
        let names = [
            "gemini-3.6-flash-high",
            "gemini-3.6-flash",
            "gemini-3.5-flash-lite",
            "llama-4-maverick",
            "qwen3.8-max",
            "qwen3.8-27b",
            "mistral-large-3",
            "mistral-medium-3",
            "mistral-medium-3.5",
        ];
        let m = Matcher::new(names);
        assert_eq!(m.find_name("models/gemini-3.6-flash"), vec![1]);
        assert_eq!(m.find_name("gemini-3.6-flash-lite"), Vec::<usize>::new());
        assert_eq!(m.find_name("meta-llama/llama-4-maverick-17b-128e-instruct"), vec![3]);
        assert_eq!(m.find_name("qwen/qwen3.8-27b"), vec![5]);
        // differently sized sibling must not borrow the Max model's score
        assert_eq!(m.find_name("qwen3.8-35b-a3b"), Vec::<usize>::new());
        assert_eq!(m.find_name("mistral-large-latest"), vec![6]);
        assert_eq!(m.find_name("mistral-medium-latest"), vec![8]);
        assert_eq!(m.find_name("totally-unknown-model"), Vec::<usize>::new());
    }

    #[test]
    fn params() {
        assert_eq!(param_billions(&canon("llama-3.3-70b")), Some(70.0));
        let moe = param_billions(&canon("qwen3-235b-a22b")).unwrap();
        assert!((moe - (235.0f64 * 22.0).sqrt()).abs() < 1e-6);
        assert_eq!(param_billions(&canon("gemini-3.6-flash")), None);
        assert_eq!(param_billions(&canon("mixtral-8x7b")), Some(56.0));
    }
}
