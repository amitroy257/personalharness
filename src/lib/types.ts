export type Preset = "general" | "coding" | "reasoning" | "fast";
export type ViewName = "recommended" | "strongest" | "popular" | "hot" | "mine";

export interface Route {
  api_id: string;
  provider: string;
  provider_name: string;
  model: string;
  context_length: number | null;
  cooling_until: number | null;
  attempts: number;
  success_rate: number | null;
  tokens_per_sec: number | null;
}

export interface Personal {
  uses: number;
  attempts: number;
  success_rate: number;
  tokens_per_sec: number | null;
  thumbs_up: number;
  thumbs_down: number;
  score: number;
  confidence: number;
  last_used: number | null;
}

export interface RankedModel {
  rank: number;
  family: string;
  name: string;
  routes: Route[];
  strength: number;
  strength_estimated: boolean;
  coding: number;
  popularity: number | null;
  hot: number | null;
  speed: number;
  released: number | null;
  personal: Personal | null;
  score: number;
  presets: Record<Preset, number>;
  available: boolean;
  badges: string[];
  why: string[];
}

export interface SourceStatus {
  id: string;
  label: string;
  ok: boolean;
  count: number;
  error: string | null;
  fetched_at: number;
  detail: string;
}

export interface SignalsInfo {
  fetched_at: number;
  arena_as_of: string | null;
  sources: SourceStatus[];
}

export interface RankingsResponse {
  items: RankedModel[];
  signals: SignalsInfo;
}

export interface Provider {
  id: string;
  name: string;
  builtin: boolean;
  enabled: boolean;
  usable: boolean;
  key_set: boolean;
  key_hint: string;
  account_id: string;
  needs_account_id: boolean;
  key_url: string;
  docs_url: string;
  free_tier: string;
  base_url: string;
  default_base_url: string;
  static_models: string[];
  model_count: number;
  fetched_at: number | null;
  error: string | null;
}

export interface ModelItem {
  api_id: string;
  provider: string;
  provider_name: string;
  model: string;
  name: string;
  family: string;
  context_length: number | null;
  hidden: boolean;
  cooldown: { until_ms: number; reason: string } | null;
}

export interface Conversation {
  id: string;
  title: string;
  model: string;
  system_prompt: string;
  created_at: number;
  updated_at: number;
}

export interface Message {
  id: string;
  conversation_id: string;
  role: "user" | "assistant" | "system";
  content: string;
  reasoning: string;
  api_id: string;
  family: string;
  created_at: number;
  rating: number;
  prompt_tokens: number | null;
  completion_tokens: number | null;
  duration_ms: number | null;
  notes: string;
}

export type ChatEvent =
  | { type: "conversation"; id: string; title: string; model: string }
  | { type: "user_message"; message: Message }
  | { type: "attempt"; api_id: string; provider: string; model: string }
  | { type: "fallback"; api_id: string; status: number | null; error: string }
  | { type: "delta"; content: string }
  | { type: "reasoning"; content: string }
  | { type: "done"; message: Message }
  | { type: "error"; error: string; message: Message | null };

export interface ServerStatus {
  enabled: boolean;
  running: boolean;
  port: number;
  base_url: string;
  error: string | null;
}

export interface Overview {
  providers_configured: number;
  models_available: number;
  server: ServerStatus;
  signals: { fetched_at: number; arena_as_of: string | null };
  default_model: string;
  version: string;
}

export interface SettingsView {
  server: { enabled: boolean; port: number; token: string };
  routing: { max_attempts: number; cooldown_secs: number; request_timeout_secs: number; same_model_fallback: boolean };
  ranking: {
    public_data: boolean;
    refresh_hours: number;
    personal_weight: number;
    source_arena: boolean;
    source_huggingface: boolean;
    source_openrouter_models: boolean;
    openrouter_key_set: boolean;
    openrouter_key_hint: string;
    artificial_analysis_key_set: boolean;
    artificial_analysis_key_hint: string;
  };
  chat: { default_model: string; system_prompt: string; temperature: number | null };
  data_dir: string;
}

export interface PrivacyInfo {
  data_dir: string;
  files: { name: string; what: string }[];
  network: { host: string; why: string }[];
  server: string;
  telemetry: string;
}
