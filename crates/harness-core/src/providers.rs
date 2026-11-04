//! Built-in free-tier providers. Every provider here speaks the OpenAI
//! chat-completions protocol; they differ only in how models are discovered.

use serde::Serialize;

#[derive(Clone, Copy, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ModelsSource {
    /// `GET {base}/models` returning `{ data: [{ id, ... }] }`.
    OpenAi,
    /// GitHub Models catalog at `https://models.github.ai/catalog/models`.
    GitHubCatalog,
    /// Cloudflare `GET /accounts/{id}/ai/models/search?task=Text Generation`.
    CloudflareSearch,
    /// Fixed list (used where only specific models are free).
    Static,
}

#[derive(Clone, Debug, Serialize)]
pub struct ProviderDef {
    pub id: &'static str,
    pub name: &'static str,
    /// May contain `{account_id}`.
    pub base_url: &'static str,
    pub models: ModelsSource,
    pub static_models: &'static [&'static str],
    pub key_url: &'static str,
    pub docs_url: &'static str,
    pub free_tier: &'static str,
    pub needs_account_id: bool,
    /// Rough throughput prior (0-100) used before we have measurements.
    pub speed_prior: f64,
    #[serde(skip)]
    pub extra_headers: &'static [(&'static str, &'static str)],
}

pub const BUILTIN: &[ProviderDef] = &[
    ProviderDef {
        id: "groq",
        name: "Groq",
        base_url: "https://api.groq.com/openai/v1",
        models: ModelsSource::OpenAi,
        static_models: &[],
        key_url: "https://console.groq.com/keys",
        docs_url: "https://console.groq.com/docs/rate-limits",
        free_tier: "Free plan with per-model request and token limits, no card needed. Very fast LPU inference.",
        needs_account_id: false,
        speed_prior: 92.0,
        extra_headers: &[],
    },
    ProviderDef {
        id: "gemini",
        name: "Google Gemini",
        base_url: "https://generativelanguage.googleapis.com/v1beta/openai",
        models: ModelsSource::OpenAi,
        static_models: &[],
        key_url: "https://aistudio.google.com/app/apikey",
        docs_url: "https://ai.google.dev/gemini-api/docs/rate-limits",
        free_tier: "Free tier for Flash, Flash-Lite and Gemma models with daily limits. Use a key from a project WITHOUT billing to stay free. Google may use free-tier prompts to improve its products.",
        needs_account_id: false,
        speed_prior: 72.0,
        extra_headers: &[],
    },
    ProviderDef {
        id: "mistral",
        name: "Mistral AI",
        base_url: "https://api.mistral.ai/v1",
        models: ModelsSource::OpenAi,
        static_models: &[],
        key_url: "https://console.mistral.ai/api-keys",
        docs_url: "https://docs.mistral.ai/getting-started/quickstart/",
        free_tier: "Free 'Experiment' plan that covers all models with low rate limits (phone verification required).",
        needs_account_id: false,
        speed_prior: 60.0,
        extra_headers: &[],
    },
    ProviderDef {
        id: "cerebras",
        name: "Cerebras",
        base_url: "https://api.cerebras.ai/v1",
        models: ModelsSource::OpenAi,
        static_models: &[],
        key_url: "https://cloud.cerebras.ai/",
        docs_url: "https://inference-docs.cerebras.ai/support/rate-limits",
        free_tier: "Free tier with daily token limits and extremely fast output. Some models have a reduced context window on the free tier.",
        needs_account_id: false,
        speed_prior: 97.0,
        extra_headers: &[],
    },
    ProviderDef {
        id: "github",
        name: "GitHub Models",
        base_url: "https://models.github.ai/inference",
        models: ModelsSource::GitHubCatalog,
        static_models: &[],
        key_url: "https://github.com/settings/personal-access-tokens/new",
        docs_url: "https://docs.github.com/en/github-models/use-github-models/prototyping-with-ai-models#rate-limits",
        free_tier: "Free for every GitHub account with low daily limits; includes OpenAI, DeepSeek, Llama and more. Create a fine-grained token with the 'Models: read' permission.",
        needs_account_id: false,
        speed_prior: 45.0,
        extra_headers: &[("X-GitHub-Api-Version", "2022-11-28")],
    },
    ProviderDef {
        id: "nvidia",
        name: "NVIDIA NIM",
        base_url: "https://integrate.api.nvidia.com/v1",
        models: ModelsSource::OpenAi,
        static_models: &[],
        key_url: "https://build.nvidia.com/settings/api-keys",
        docs_url: "https://build.nvidia.com/models",
        free_tier: "Free developer access to many hosted open models (rate-limited).",
        needs_account_id: false,
        speed_prior: 50.0,
        extra_headers: &[],
    },
    ProviderDef {
        id: "cloudflare",
        name: "Cloudflare Workers AI",
        base_url: "https://api.cloudflare.com/client/v4/accounts/{account_id}/ai/v1",
        models: ModelsSource::CloudflareSearch,
        static_models: &[],
        key_url: "https://dash.cloudflare.com/profile/api-tokens",
        docs_url: "https://developers.cloudflare.com/workers-ai/platform/pricing/",
        free_tier: "10,000 free Neurons per day. Needs your Account ID and an API token with Workers AI permission.",
        needs_account_id: true,
        speed_prior: 55.0,
        extra_headers: &[],
    },
    ProviderDef {
        id: "sambanova",
        name: "SambaNova Cloud",
        base_url: "https://api.sambanova.ai/v1",
        models: ModelsSource::OpenAi,
        static_models: &[],
        key_url: "https://cloud.sambanova.ai/apis",
        docs_url: "https://docs.sambanova.ai/",
        free_tier: "Free tier with low daily limits; fast inference of large open models.",
        needs_account_id: false,
        speed_prior: 85.0,
        extra_headers: &[],
    },
    ProviderDef {
        id: "huggingface",
        name: "Hugging Face",
        base_url: "https://router.huggingface.co/v1",
        models: ModelsSource::OpenAi,
        static_models: &[],
        key_url: "https://huggingface.co/settings/tokens",
        docs_url: "https://huggingface.co/docs/inference-providers/pricing",
        free_tier: "Small monthly free credits routed across many inference providers.",
        needs_account_id: false,
        speed_prior: 55.0,
        extra_headers: &[],
    },
    ProviderDef {
        id: "zhipu",
        name: "Zhipu BigModel (GLM Flash)",
        base_url: "https://open.bigmodel.cn/api/paas/v4",
        models: ModelsSource::Static,
        static_models: &["glm-4.7-flash", "glm-4.6v-flash"],
        key_url: "https://open.bigmodel.cn/usercenter/apikeys",
        docs_url: "https://open.bigmodel.cn/pricing",
        free_tier: "The GLM Flash models are free (one request at a time). Only the free Flash models are listed.",
        needs_account_id: false,
        speed_prior: 45.0,
        extra_headers: &[],
    },
];

pub fn builtin(id: &str) -> Option<&'static ProviderDef> {
    BUILTIN.iter().find(|p| p.id == id)
}

/// Heuristic filter: keep chat/instruct LLMs, drop embeddings, speech, image
/// generation, guard/safety classifiers and similar endpoints.
pub fn is_chat_model(id: &str) -> bool {
    let l = id.to_ascii_lowercase();
    const DENY: &[&str] = &[
        "embed",
        "rerank",
        "whisper",
        "tts",
        "transcribe",
        "speech",
        "audio",
        "-guard",
        "guard-",
        "llamaguard",
        "shieldgemma",
        "safety",
        "moderation",
        "content-safety",
        "topic-control",
        "jailbreak",
        "reward",
        "clip",
        "parakeet",
        "canary",
        "imagen",
        "veo-",
        "dall-e",
        "flux",
        "stable-diffusion",
        "sdxl",
        "image-generation",
        "-image",
        "native-audio",
        "-live",
        "aqa",
        "retriever",
        "nv-embed",
        "bge-",
        "e5-",
        "ocr",
        "paligemma",
        "deplot",
        "kosmos",
        "fuyu",
        "-pii",
        "calibration",
        "nemoretriever",
        "cosmos",
        "riva",
        "lyria",
        "playai",
        "orpheus",
        "vision-encoder",
        "segment",
        "detr",
        "yolo",
        "-asr",
        "asr-",
        "text-to-",
        "-to-image",
        "melotts",
        "resnet",
        "llava-hf",
        "uform",
        "unum",
        "distil-whisper",
        "nvclip",
    ];
    !DENY.iter().any(|d| l.contains(d))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chat_filter() {
        assert!(is_chat_model("llama-3.3-70b-versatile"));
        assert!(is_chat_model("models/gemini-3.6-flash"));
        assert!(is_chat_model("deepseek-ai/deepseek-v4-flash"));
        assert!(is_chat_model("openai/gpt-oss-120b"));
        assert!(!is_chat_model("whisper-large-v3"));
        assert!(!is_chat_model("meta-llama/llama-guard-4-12b"));
        assert!(!is_chat_model("models/text-embedding-004"));
        assert!(!is_chat_model("nvidia/nv-embedqa-e5-v5"));
        assert!(!is_chat_model("models/imagen-4.0-generate"));
        assert!(!is_chat_model("playai-tts"));
        assert!(!is_chat_model("mistral-ocr-latest"));
    }

    #[test]
    fn builtin_ids_unique() {
        let mut ids: Vec<_> = BUILTIN.iter().map(|p| p.id).collect();
        ids.sort();
        ids.dedup();
        assert_eq!(ids.len(), BUILTIN.len());
    }
}
