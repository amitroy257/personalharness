import { useCallback, useEffect, useState } from "react";
import type { Nav } from "../App";
import { call, openExternal } from "../lib/api";
import { timeAgo } from "../lib/format";
import type { Provider } from "../lib/types";
import { Switch, useToast } from "../components/ui";

interface SaveResult {
  provider: Provider;
  refresh: { ok: boolean; count?: number; error?: string };
}

function ProviderCard({ p, onSaved }: { p: Provider; onSaved: (p: Provider) => void }) {
  const toast = useToast();
  const [key, setKey] = useState("");
  const [account, setAccount] = useState(p.account_id);
  const [baseUrl, setBaseUrl] = useState(p.base_url === p.default_base_url ? "" : p.base_url);
  const [models, setModels] = useState(p.static_models.join(", "));
  const [busy, setBusy] = useState(false);
  const [result, setResult] = useState<SaveResult["refresh"] | null>(null);

  const save = async (extra: Record<string, unknown> = {}) => {
    setBusy(true);
    setResult(null);
    try {
      const args: Record<string, unknown> = { id: p.id, ...extra };
      if (key.trim()) args.api_key = key.trim();
      if (p.needs_account_id) args.account_id = account;
      if (p.builtin) args.base_url = baseUrl.trim() || null;
      else {
        if (baseUrl.trim()) args.base_url = baseUrl.trim();
        args.models = models;
      }
      const r = await call<SaveResult>("save_provider", args);
      setKey("");
      setResult(r.refresh);
      onSaved(r.provider);
    } catch (e) {
      toast(String(e));
    } finally {
      setBusy(false);
    }
  };

  const status = !p.key_set ? (
    <span className="pill">Not set up</span>
  ) : !p.enabled ? (
    <span className="pill">Disabled</span>
  ) : p.error ? (
    <span className="pill bad">Error</span>
  ) : (
    <span className="pill good">● {p.model_count} models</span>
  );

  return (
    <div className="card stack">
      <div className="prov-head">
        <div>
          <h3>{p.name}</h3>
          <div className="small muted">{p.free_tier}</div>
        </div>
        {status}
      </div>
      {p.key_url && (
        <div className="row small">
          <a onClick={() => openExternal(p.key_url)}>Get a free key ↗</a>
          {p.docs_url && <a onClick={() => openExternal(p.docs_url)}>Free-tier limits ↗</a>}
        </div>
      )}
      <label className="field">
        API key
        <input
          className="input"
          type="password"
          autoComplete="off"
          spellCheck={false}
          placeholder={p.key_set ? `Saved (${p.key_hint}) — paste to replace` : "Paste your key"}
          value={key}
          onChange={(e) => setKey(e.target.value)}
        />
      </label>
      {p.needs_account_id && (
        <label className="field">
          Account ID
          <input className="input" value={account} onChange={(e) => setAccount(e.target.value)} placeholder="Cloudflare dashboard → Workers AI → Account ID" />
        </label>
      )}
      {!p.builtin && (
        <label className="field">
          Models (optional, comma separated — leave empty to auto-discover via /models)
          <input className="input" value={models} onChange={(e) => setModels(e.target.value)} />
        </label>
      )}
      <details>
        <summary className="small muted">Advanced</summary>
        <label className="field" style={{ marginTop: 8 }}>
          Base URL {p.builtin && <span>(default {p.default_base_url})</span>}
          <input className="input mono" value={baseUrl} onChange={(e) => setBaseUrl(e.target.value)} placeholder={p.default_base_url} />
        </label>
      </details>
      <div className="row">
        <Switch checked={p.enabled} onChange={(v) => save({ enabled: v })} label="Enabled" />
        <div className="grow" />
        {p.fetched_at && <span className="small muted">checked {timeAgo(p.fetched_at)}</span>}
        <button className="btn primary" onClick={() => save()} disabled={busy || (!key.trim() && !p.key_set && p.builtin)}>
          {busy ? "Checking…" : p.key_set ? "Save & test" : "Connect"}
        </button>
      </div>
      {result && (
        <div className={`banner ${result.ok ? "" : "bad"} small`}>
          {result.ok ? `Connected — found ${result.count} chat models.` : `Could not list models: ${result.error}`}
        </div>
      )}
      {!result && p.error && p.key_set && <div className="banner bad small">{p.error}</div>}
    </div>
  );
}

export default function ProvidersPage({ nav }: { nav: Nav }) {
  const toast = useToast();
  const [list, setList] = useState<Provider[]>([]);
  const [name, setName] = useState("");
  const [url, setUrl] = useState("");
  const [key, setKey] = useState("");

  const load = useCallback(() => call<Provider[]>("list_providers").then(setList), []);
  useEffect(() => {
    load();
  }, [load]);

  const onSaved = (p: Provider) => {
    setList((xs) => xs.map((x) => (x.id === p.id ? p : x)));
    nav.refreshOverview();
  };

  const addCustom = async () => {
    try {
      const r = await call<{ providers: Provider[]; refresh: { ok: boolean; error?: string } }>("add_custom_provider", { name, base_url: url, api_key: key });
      setList(r.providers);
      setName("");
      setUrl("");
      setKey("");
      toast(r.refresh.ok ? "Provider added" : `Added, but listing models failed: ${r.refresh.error}`);
      nav.refreshOverview();
    } catch (e) {
      toast(String(e));
    }
  };

  const remove = async (id: string) => {
    const r = await call<Provider[]>("remove_custom_provider", { id });
    setList(r);
    nav.refreshOverview();
  };

  const connected = list.filter((p) => p.usable).length;
  const builtin = list.filter((p) => p.builtin);
  const custom = list.filter((p) => !p.builtin);

  return (
    <div className="page">
      <div className="page-header">
        <div>
          <h1>Providers</h1>
          <p>
            Connect free tiers directly with your own keys. {connected} connected. Keys are stored only in this app's private data folder and are sent only
            to the provider they belong to.
          </p>
        </div>
      </div>
      <div className="banner warn small" style={{ marginBottom: 14 }}>
        Your prompts go to whichever provider answers them. Free tiers may log or use prompts to improve their models — avoid sending secrets to free
        tiers.
      </div>
      <div className="grid-2">
        {builtin.map((p) => (
          <ProviderCard key={p.id} p={p} onSaved={onSaved} />
        ))}
      </div>

      <h2 style={{ fontSize: 17, marginTop: 28 }}>Custom OpenAI-compatible providers</h2>
      <p className="muted small" style={{ marginTop: -6 }}>
        Any endpoint that speaks the OpenAI chat-completions API: a local Ollama or LM Studio server (http://localhost:11434/v1), OpenRouter, or another
        free service.
      </p>
      <div className="grid-2">
        {custom.map((p) => (
          <div key={p.id} className="stack">
            <ProviderCard p={p} onSaved={onSaved} />
            <button className="btn danger sm" style={{ justifySelf: "end" }} onClick={() => remove(p.id)}>
              Remove {p.name}
            </button>
          </div>
        ))}
        <div className="card stack">
          <h3>Add a provider</h3>
          <label className="field">
            Name
            <input className="input" value={name} onChange={(e) => setName(e.target.value)} placeholder="e.g. Local Ollama" />
          </label>
          <label className="field">
            Base URL
            <input className="input mono" value={url} onChange={(e) => setUrl(e.target.value)} placeholder="http://localhost:11434/v1" />
          </label>
          <label className="field">
            API key (optional)
            <input className="input" type="password" value={key} onChange={(e) => setKey(e.target.value)} />
          </label>
          <div className="row">
            <div className="grow" />
            <button className="btn primary" onClick={addCustom} disabled={!name.trim() || !url.trim()}>
              Add provider
            </button>
          </div>
        </div>
      </div>
    </div>
  );
}
