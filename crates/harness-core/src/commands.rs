//! Commands used by the UI. The Tauri app exposes them via IPC (`call` +
//! `chat`); the headless server exposes them under `/__ui/` for the browser.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tokio::sync::mpsc::UnboundedSender;

use crate::catalog;
use crate::harness::Harness;
use crate::providers;
use crate::ranking::View;
use crate::router;
use crate::settings::CustomProvider;
use crate::store::Message;
use crate::util::{key_hint, slugify, truncate_chars};

type R = Result<Value, String>;

fn arg_str(args: &Value, k: &str) -> Option<String> {
    args.get(k).and_then(|v| v.as_str()).map(|s| s.to_string())
}

fn req_str(args: &Value, k: &str) -> Result<String, String> {
    arg_str(args, k).filter(|s| !s.trim().is_empty()).ok_or_else(|| format!("missing '{k}'"))
}

fn e2s<E: std::fmt::Display>(e: E) -> String {
    e.to_string()
}

pub async fn call(h: &Arc<Harness>, name: &str, args: Value) -> R {
    match name {
        "overview" => overview(h).await,
        "get_settings" => Ok(settings_view(h)),
        "update_settings" => update_settings(h, &args).await,
        "list_providers" => Ok(json!(providers_view(h))),
        "save_provider" => save_provider(h, &args).await,
        "add_custom_provider" => add_custom_provider(h, &args).await,
        "remove_custom_provider" => {
            let id = req_str(&args, "id")?;
            h.update_settings(|s| s.custom_providers.retain(|c| c.id != id)).map_err(e2s)?;
            let _ = h.refresh_provider(&id).await; // drops cached catalog
            Ok(json!(providers_view(h)))
        }
        "refresh_models" => {
            let res = match arg_str(&args, "provider") {
                Some(p) => vec![(p.clone(), h.refresh_provider(&p).await.map_err(e2s))],
                None => h.refresh_all_providers().await,
            };
            Ok(json!(res
                .into_iter()
                .map(|(id, r)| json!({"provider": id, "ok": r.is_ok(), "count": r.as_ref().ok(), "error": r.err()}))
                .collect::<Vec<_>>()))
        }
        "list_models" => Ok(list_models(h)),
        "set_model_hidden" => {
            let api_id = req_str(&args, "api_id")?;
            let hidden = args.get("hidden").and_then(|v| v.as_bool()).unwrap_or(true);
            h.update_settings(|s| {
                if hidden {
                    s.hidden_models.insert(api_id.clone());
                } else {
                    s.hidden_models.remove(&api_id);
                }
            })
            .map_err(e2s)?;
            Ok(json!({"ok": true}))
        }
        "rankings" => {
            let view = View::parse(&arg_str(&args, "view").unwrap_or_default(), &arg_str(&args, "preset").unwrap_or_default());
            let mut items = h.rankings(view);
            if let Some(l) = args.get("limit").and_then(|v| v.as_u64()) {
                items.truncate(l as usize);
            }
            let sig = h.signals();
            Ok(json!({
                "items": items,
                "signals": {"fetched_at": sig.fetched_at, "arena_as_of": sig.arena_as_of, "sources": sig.sources},
            }))
        }
        "refresh_signals" => {
            let s = h.refresh_signals(true).await;
            Ok(json!({"fetched_at": s.fetched_at, "arena_as_of": s.arena_as_of, "sources": s.sources}))
        }
        "list_conversations" => Ok(json!(h.store.list_conversations(200))),
        "get_conversation" => {
            let id = req_str(&args, "id")?;
            let c = h.store.get_conversation(&id).ok_or("conversation not found")?;
            Ok(json!({"conversation": c, "messages": h.store.messages(&id)}))
        }
        "delete_conversation" => {
            h.store.delete_conversation(&req_str(&args, "id")?);
            Ok(json!({"ok": true}))
        }
        "update_conversation" => {
            let id = req_str(&args, "id")?;
            h.store.update_conversation(
                &id,
                arg_str(&args, "title").as_deref(),
                arg_str(&args, "model").as_deref(),
                arg_str(&args, "system_prompt").as_deref(),
            );
            Ok(json!({"ok": true}))
        }
        "rate_message" => {
            let id = req_str(&args, "id")?;
            let rating = args.get("rating").and_then(|v| v.as_i64()).unwrap_or(0);
            let ok = h.store.rate_message(&id, rating);
            h.invalidate_stats();
            Ok(json!({"ok": ok}))
        }
        "stop_generation" => {
            let id = req_str(&args, "conversation_id")?;
            if let Some(flag) = h.stops.lock().get(&id) {
                flag.store(true, Ordering::SeqCst);
            }
            Ok(json!({"ok": true}))
        }
        "usage_summary" => {
            let days = args.get("days").and_then(|v| v.as_i64()).unwrap_or(30);
            let mut fams: Vec<_> = h.store.family_stats(days).into_values().collect();
            fams.sort_by_key(|f| std::cmp::Reverse(f.attempts));
            Ok(json!({"totals": h.store.usage_totals(days), "families": fams, "cooldowns": h.cooldowns()}))
        }
        "server_status" => Ok(h.server_status().await),
        "regenerate_token" => {
            let t = crate::util::new_token();
            h.update_settings(|s| s.server.token = t.clone()).map_err(e2s)?;
            Ok(json!({"token": t}))
        }
        "wipe_data" => {
            let what = arg_str(&args, "what").unwrap_or_else(|| "all".into());
            h.store.wipe(&what).map_err(e2s)?;
            h.invalidate_stats();
            if what == "all" {
                h.update_settings(|s| {
                    let token = crate::util::new_token();
                    *s = Default::default();
                    s.server.token = token;
                })
                .map_err(e2s)?;
                h.start_server().await.ok();
            }
            Ok(json!({"ok": true}))
        }
        "privacy_info" => Ok(h.privacy_info()),
        "privacy_selfcheck" => Ok(selfcheck_json(h).await),
        other => Err(format!("unknown command '{other}'")),
    }
}

pub async fn selfcheck_json(h: &Arc<Harness>) -> Value {
    let checks = crate::selfcheck::run(h).await;
    let count = |st: crate::selfcheck::Status| checks.iter().filter(|c| c.status == st).count();
    json!({
        "platform": std::env::consts::OS,
        "desktop": h.is_desktop(),
        "passed": count(crate::selfcheck::Status::Pass),
        "failed": count(crate::selfcheck::Status::Fail),
        "checks": checks,
    })
}

async fn overview(h: &Arc<Harness>) -> R {
    let entries = h.entries(false);
    let s = h.settings();
    let configured = catalog::all_endpoints(&s).iter().filter(|e| e.usable()).count();
    let sig = h.signals();
    Ok(json!({
        "providers_configured": configured,
        "models_available": entries.len(),
        "server": h.server_status().await,
        "signals": {"fetched_at": sig.fetched_at, "arena_as_of": sig.arena_as_of},
        "default_model": s.chat.default_model,
        "version": env!("CARGO_PKG_VERSION"),
    }))
}

fn settings_view(h: &Harness) -> Value {
    let s = h.settings();
    json!({
        "server": {"enabled": s.server.enabled, "port": s.server.port, "token": s.server.token},
        "routing": s.routing,
        "ranking": {
            "public_data": s.ranking.public_data,
            "refresh_hours": s.ranking.refresh_hours,
            "personal_weight": s.ranking.personal_weight,
            "source_arena": s.ranking.source_arena,
            "source_huggingface": s.ranking.source_huggingface,
            "source_openrouter_models": s.ranking.source_openrouter_models,
            "openrouter_key_set": !s.ranking.openrouter_key.is_empty(),
            "openrouter_key_hint": key_hint(&s.ranking.openrouter_key),
            "artificial_analysis_key_set": !s.ranking.artificial_analysis_key.is_empty(),
            "artificial_analysis_key_hint": key_hint(&s.ranking.artificial_analysis_key),
        },
        "chat": s.chat,
        "data_dir": h.data_dir.display().to_string(),
    })
}

async fn update_settings(h: &Arc<Harness>, args: &Value) -> R {
    let before = h.settings();
    h.update_settings(|s| {
        if let Some(sv) = args.get("server") {
            if let Some(v) = sv.get("enabled").and_then(|v| v.as_bool()) {
                s.server.enabled = v;
            }
            if let Some(v) = sv.get("port").and_then(|v| v.as_u64()) {
                if (1024..=65535).contains(&v) {
                    s.server.port = v as u16;
                }
            }
        }
        if let Some(r) = args.get("routing") {
            if let Some(v) = r.get("max_attempts").and_then(|v| v.as_u64()) {
                s.routing.max_attempts = v.clamp(1, 10) as u32;
            }
            if let Some(v) = r.get("cooldown_secs").and_then(|v| v.as_u64()) {
                s.routing.cooldown_secs = v.clamp(5, 3600);
            }
            if let Some(v) = r.get("request_timeout_secs").and_then(|v| v.as_u64()) {
                s.routing.request_timeout_secs = v.clamp(10, 1800);
            }
            if let Some(v) = r.get("same_model_fallback").and_then(|v| v.as_bool()) {
                s.routing.same_model_fallback = v;
            }
        }
        if let Some(r) = args.get("ranking") {
            let b = |k: &str| r.get(k).and_then(|v| v.as_bool());
            if let Some(v) = b("public_data") {
                s.ranking.public_data = v;
            }
            if let Some(v) = b("source_arena") {
                s.ranking.source_arena = v;
            }
            if let Some(v) = b("source_huggingface") {
                s.ranking.source_huggingface = v;
            }
            if let Some(v) = b("source_openrouter_models") {
                s.ranking.source_openrouter_models = v;
            }
            if let Some(v) = r.get("refresh_hours").and_then(|v| v.as_u64()) {
                s.ranking.refresh_hours = v.clamp(1, 168);
            }
            if let Some(v) = r.get("personal_weight").and_then(|v| v.as_f64()) {
                s.ranking.personal_weight = v.clamp(0.0, 1.0);
            }
            if let Some(v) = r.get("openrouter_key").and_then(|v| v.as_str()) {
                s.ranking.openrouter_key = v.trim().to_string();
            }
            if let Some(v) = r.get("artificial_analysis_key").and_then(|v| v.as_str()) {
                s.ranking.artificial_analysis_key = v.trim().to_string();
            }
        }
        if let Some(c) = args.get("chat") {
            if let Some(v) = c.get("default_model").and_then(|v| v.as_str()) {
                s.chat.default_model = v.to_string();
            }
            if let Some(v) = c.get("system_prompt").and_then(|v| v.as_str()) {
                s.chat.system_prompt = v.to_string();
            }
            if let Some(v) = c.get("temperature") {
                s.chat.temperature = v.as_f64().map(|t| t.clamp(0.0, 2.0));
            }
        }
    })
    .map_err(e2s)?;
    let after = h.settings();
    if before.server.port != after.server.port || before.server.enabled != after.server.enabled {
        if let Err(e) = h.start_server().await {
            return Err(e.to_string());
        }
    }
    let ranking_changed = serde_json::to_string(&before.ranking).ok() != serde_json::to_string(&after.ranking).ok();
    if ranking_changed {
        let h2 = h.clone();
        tokio::spawn(async move {
            h2.refresh_signals(true).await;
        });
    }
    Ok(settings_view(h))
}

fn providers_view(h: &Harness) -> Vec<Value> {
    let s = h.settings();
    catalog::all_endpoints(&s)
        .into_iter()
        .map(|ep| {
            let def = providers::builtin(&ep.provider);
            let cat = h.catalog(&ep.provider);
            let ps = s.provider(&ep.provider);
            let custom = s.custom_providers.iter().find(|c| c.id == ep.provider);
            json!({
                "id": ep.provider,
                "name": ep.name,
                "builtin": def.is_some(),
                "enabled": ep.enabled,
                "usable": ep.usable(),
                "key_set": !ep.api_key.is_empty(),
                "key_hint": key_hint(&ep.api_key),
                "account_id": ps.account_id,
                "needs_account_id": def.map(|d| d.needs_account_id).unwrap_or(false),
                "key_url": def.map(|d| d.key_url).unwrap_or(""),
                "docs_url": def.map(|d| d.docs_url).unwrap_or(""),
                "free_tier": def.map(|d| d.free_tier.to_string()).unwrap_or_else(|| "Custom OpenAI-compatible endpoint".into()),
                "base_url": ep.base_url,
                "default_base_url": def.map(|d| d.base_url.to_string()).unwrap_or_else(|| ep.base_url.clone()),
                "static_models": custom.map(|c| c.models.clone()).unwrap_or_default(),
                "model_count": cat.as_ref().map(|c| c.models.len()).unwrap_or(0),
                "fetched_at": cat.as_ref().map(|c| c.fetched_at),
                "error": cat.as_ref().and_then(|c| c.error.clone()),
            })
        })
        .collect()
}

async fn save_provider(h: &Arc<Harness>, args: &Value) -> R {
    let id = req_str(args, "id")?;
    let is_builtin = providers::builtin(&id).is_some();
    let exists_custom = h.settings().custom_providers.iter().any(|c| c.id == id);
    if !is_builtin && !exists_custom {
        return Err(format!("unknown provider '{id}'"));
    }
    h.update_settings(|s| {
        if is_builtin {
            let p = s.providers.entry(id.clone()).or_default();
            if let Some(k) = args.get("api_key").and_then(|v| v.as_str()) {
                p.api_key = k.trim().to_string();
                if !p.api_key.is_empty() && args.get("enabled").is_none() {
                    p.enabled = true;
                }
            }
            if let Some(v) = args.get("enabled").and_then(|v| v.as_bool()) {
                p.enabled = v;
            }
            if let Some(v) = args.get("account_id").and_then(|v| v.as_str()) {
                p.account_id = v.trim().to_string();
            }
            if let Some(v) = args.get("base_url") {
                p.base_url = v.as_str().map(|s| s.trim().trim_end_matches('/').to_string()).filter(|s| !s.is_empty());
            }
        } else if let Some(c) = s.custom_providers.iter_mut().find(|c| c.id == id) {
            if let Some(k) = args.get("api_key").and_then(|v| v.as_str()) {
                c.api_key = k.trim().to_string();
            }
            if let Some(v) = args.get("enabled").and_then(|v| v.as_bool()) {
                c.enabled = v;
            }
            if let Some(v) = args.get("base_url").and_then(|v| v.as_str()) {
                if !v.trim().is_empty() {
                    c.base_url = v.trim().trim_end_matches('/').to_string();
                }
            }
            if let Some(v) = args.get("models") {
                c.models = parse_models(v);
            }
            if let Some(v) = args.get("name").and_then(|v| v.as_str()) {
                if !v.trim().is_empty() {
                    c.name = v.trim().to_string();
                }
            }
        }
    })
    .map_err(e2s)?;
    let refresh = match h.refresh_provider(&id).await {
        Ok(n) => json!({"ok": true, "count": n}),
        Err(e) => json!({"ok": false, "error": e.to_string()}),
    };
    let view = providers_view(h).into_iter().find(|p| p["id"] == id.as_str()).unwrap_or(Value::Null);
    Ok(json!({"provider": view, "refresh": refresh}))
}

fn parse_models(v: &Value) -> Vec<String> {
    match v {
        Value::Array(a) => a.iter().filter_map(|x| x.as_str()).map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect(),
        Value::String(s) => s.split([',', '\n']).map(|x| x.trim().to_string()).filter(|x| !x.is_empty()).collect(),
        _ => vec![],
    }
}

async fn add_custom_provider(h: &Arc<Harness>, args: &Value) -> R {
    let name = req_str(args, "name")?;
    let base_url = req_str(args, "base_url")?.trim().trim_end_matches('/').to_string();
    if !(base_url.starts_with("http://") || base_url.starts_with("https://")) {
        return Err("base URL must start with http:// or https://".into());
    }
    let mut id = slugify(&name);
    if id.is_empty() {
        id = "custom".into();
    }
    let s = h.settings();
    let taken = |x: &str| providers::builtin(x).is_some() || s.custom_providers.iter().any(|c| c.id == x);
    let base_id = id.clone();
    let mut n = 2;
    while taken(&id) {
        id = format!("{base_id}-{n}");
        n += 1;
    }
    let cp = CustomProvider {
        id: id.clone(),
        name: name.trim().to_string(),
        base_url,
        api_key: arg_str(args, "api_key").unwrap_or_default().trim().to_string(),
        enabled: true,
        models: args.get("models").map(parse_models).unwrap_or_default(),
    };
    h.update_settings(|s| s.custom_providers.push(cp)).map_err(e2s)?;
    let refresh = match h.refresh_provider(&id).await {
        Ok(n) => json!({"ok": true, "count": n}),
        Err(e) => json!({"ok": false, "error": e.to_string()}),
    };
    Ok(json!({"id": id, "refresh": refresh, "providers": providers_view(h)}))
}

fn list_models(h: &Harness) -> Value {
    let s = h.settings();
    let names: std::collections::HashMap<String, String> =
        catalog::all_endpoints(&s).into_iter().map(|e| (e.provider.clone(), e.name.clone())).collect();
    let cds = h.cooldowns();
    let items: Vec<Value> = h
        .entries(true)
        .into_iter()
        .map(|m| {
            json!({
                "api_id": m.api_id,
                "provider": m.provider,
                "provider_name": names.get(&m.provider).cloned().unwrap_or_default(),
                "model": m.model,
                "name": m.name,
                "family": m.family,
                "context_length": m.context_length,
                "hidden": s.hidden_models.contains(&m.api_id),
                "cooldown": cds.get(&m.api_id),
            })
        })
        .collect();
    json!({"items": items})
}

// ---- chat -------------------------------------------------------------------------------

#[derive(Serialize, Clone, Debug)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ChatEvent {
    Conversation { id: String, title: String, model: String },
    UserMessage { message: Message },
    Attempt { api_id: String, provider: String, model: String },
    Fallback { api_id: String, status: Option<u16>, error: String },
    Delta { content: String },
    Reasoning { content: String },
    Done { message: Message },
    Error { error: String, message: Option<Message> },
}

#[derive(Deserialize)]
struct SendArgs {
    conversation_id: Option<String>,
    model: Option<String>,
    content: String,
    system_prompt: Option<String>,
    temperature: Option<f64>,
}

fn trim_history(mut msgs: Vec<(String, String)>, budget_chars: usize) -> Vec<(String, String)> {
    let mut total: usize = msgs.iter().map(|(_, c)| c.len()).sum();
    while total > budget_chars && msgs.len() > 1 {
        let (_, c) = msgs.remove(0);
        total -= c.len();
    }
    msgs
}

pub async fn send_message(h: &Arc<Harness>, args: Value, tx: UnboundedSender<ChatEvent>) {
    let emit = |ev: ChatEvent| {
        let _ = tx.send(ev);
    };
    let a: SendArgs = match serde_json::from_value(args) {
        Ok(a) => a,
        Err(e) => return emit(ChatEvent::Error { error: format!("bad request: {e}"), message: None }),
    };
    if a.content.trim().is_empty() {
        return emit(ChatEvent::Error { error: "message is empty".into(), message: None });
    }
    let settings = h.settings();
    let existing = a.conversation_id.as_deref().and_then(|id| h.store.get_conversation(id));
    let model = a
        .model
        .clone()
        .filter(|m| !m.trim().is_empty())
        .or_else(|| existing.as_ref().map(|c| c.model.clone()))
        .unwrap_or_else(|| settings.chat.default_model.clone());
    let conv = match existing {
        Some(c) => {
            if c.model != model {
                h.store.update_conversation(&c.id, None, Some(&model), None);
            }
            c
        }
        None => h.store.create_conversation(
            &truncate_chars(a.content.trim().lines().next().unwrap_or("New chat"), 60),
            &model,
            a.system_prompt.as_deref().unwrap_or(""),
        ),
    };
    emit(ChatEvent::Conversation { id: conv.id.clone(), title: conv.title.clone(), model: model.clone() });
    let user_msg = h.store.add_message(&conv.id, "user", a.content.trim(), "", "", "", (None, None), None, "");
    emit(ChatEvent::UserMessage { message: user_msg });

    let fail = |error: String| {
        let m = h.store.add_message(&conv.id, "assistant", "", "", "", "", (None, None), None, &format!("⚠️ {error}"));
        emit(ChatEvent::Error { error, message: Some(m) });
    };

    let cands = match router::resolve(h, &model, &[]) {
        Ok(c) => c,
        Err(e) => return fail(e),
    };
    let ctx_chars =
        cands.first().and_then(|c| c.entry.context_length).map(|t| (t as usize).saturating_mul(3).max(8_000)).unwrap_or(120_000);
    let history: Vec<(String, String)> = h
        .store
        .messages(&conv.id)
        .into_iter()
        .filter(|m| (m.role == "user" || m.role == "assistant") && !m.content.trim().is_empty())
        .map(|m| (m.role, m.content))
        .collect();
    let history = trim_history(history, ctx_chars);
    let system = a
        .system_prompt
        .filter(|s| !s.trim().is_empty())
        .or_else(|| Some(conv.system_prompt.clone()).filter(|s| !s.trim().is_empty()))
        .or_else(|| Some(settings.chat.system_prompt.clone()).filter(|s| !s.trim().is_empty()));
    let mut messages: Vec<Value> = vec![];
    if let Some(sys) = system {
        messages.push(json!({"role": "system", "content": sys}));
    }
    messages.extend(history.into_iter().map(|(r, c)| json!({"role": r, "content": c})));
    let mut body = json!({"messages": messages});
    if let Some(t) = a.temperature.or(settings.chat.temperature) {
        body["temperature"] = json!(t);
    }

    let stop = Arc::new(AtomicBool::new(false));
    h.stops.lock().insert(conv.id.clone(), stop.clone());
    let mut notes: Vec<String> = vec![];
    let routed = router::execute(h, cands, &body, true, "chat", |att| {
        notes.push(format!(
            "{} failed{}: {}",
            att.api_id,
            att.status.map(|s| format!(" ({s})")).unwrap_or_default(),
            truncate_chars(&att.error, 140)
        ));
        emit(ChatEvent::Fallback { api_id: att.api_id.clone(), status: att.status, error: att.error.clone() });
    })
    .await;
    let routed = match routed {
        Ok(r) => r,
        Err(e) => {
            h.stops.lock().remove(&conv.id);
            return fail(e.message);
        }
    };
    let cand = routed.candidate.clone();
    emit(ChatEvent::Attempt {
        api_id: cand.entry.api_id.clone(),
        provider: cand.entry.provider.clone(),
        model: cand.entry.model.clone(),
    });
    let out = crate::relay::pump(
        routed.response,
        routed.started,
        |_| !stop.load(Ordering::SeqCst),
        |info| {
            if !info.reasoning.is_empty() {
                emit(ChatEvent::Reasoning { content: info.reasoning.clone() });
            }
            if !info.content.is_empty() {
                emit(ChatEvent::Delta { content: info.content.clone() });
            }
            !stop.load(Ordering::SeqCst)
        },
    )
    .await;
    h.stops.lock().remove(&conv.id);
    h.store.record_usage(&out.usage_record(&cand, "chat"));
    if out.stopped {
        notes.push("Stopped by you".into());
    }
    if let Some(e) = &out.error {
        if out.content.trim().is_empty() {
            return fail(format!("{}: {e}", cand.entry.api_id));
        }
        notes.push(format!("Stream ended early: {e}"));
    }
    let usage = out.usage_record(&cand, "chat");
    let msg = h.store.add_message(
        &conv.id,
        "assistant",
        &out.content,
        &out.reasoning,
        &cand.entry.api_id,
        &cand.entry.family,
        (usage.prompt_tokens, usage.completion_tokens),
        Some(out.duration_ms),
        &notes.join("\n"),
    );
    emit(ChatEvent::Done { message: msg });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn history_trimming_keeps_latest() {
        let msgs = vec![("user".into(), "a".repeat(100)), ("assistant".into(), "b".repeat(100)), ("user".into(), "c".repeat(50))];
        let t = trim_history(msgs, 160);
        assert_eq!(t.len(), 2);
        assert!(t[1].1.starts_with('c'));
        let t = trim_history(vec![("user".into(), "x".repeat(500))], 10);
        assert_eq!(t.len(), 1, "never drops the last message");
    }

    #[test]
    fn models_parse() {
        assert_eq!(parse_models(&json!("a, b\nc")), vec!["a", "b", "c"]);
        assert_eq!(parse_models(&json!(["x", " "])), vec!["x"]);
    }
}
