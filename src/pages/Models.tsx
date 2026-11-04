import { useCallback, useEffect, useMemo, useState } from "react";
import type { Nav } from "../App";
import { call } from "../lib/api";
import { ctxLabel } from "../lib/format";
import type { ModelItem } from "../lib/types";
import { CopyButton, Switch, useToast } from "../components/ui";

export default function ModelsPage({ nav }: { nav: Nav }) {
  const toast = useToast();
  const [items, setItems] = useState<ModelItem[] | null>(null);
  const [q, setQ] = useState("");
  const [provider, setProvider] = useState("");
  const [busy, setBusy] = useState(false);

  const load = useCallback(() => call<{ items: ModelItem[] }>("list_models").then((r) => setItems(r.items)), []);
  useEffect(() => {
    load();
  }, [load]);

  const providers = useMemo(() => [...new Set((items ?? []).map((m) => m.provider_name))].sort(), [items]);
  const shown = (items ?? []).filter(
    (m) => (!provider || m.provider_name === provider) && (!q || `${m.api_id} ${m.family}`.toLowerCase().includes(q.toLowerCase())),
  );

  const refresh = async () => {
    setBusy(true);
    try {
      const r = await call<{ provider: string; ok: boolean; error: string | null }[]>("refresh_models");
      const bad = r.filter((x) => !x.ok);
      toast(bad.length ? `Refreshed, ${bad.length} provider(s) failed` : "Model lists refreshed");
      await load();
      nav.refreshOverview();
    } finally {
      setBusy(false);
    }
  };

  const setHidden = async (m: ModelItem, hidden: boolean) => {
    await call("set_model_hidden", { api_id: m.api_id, hidden });
    setItems((xs) => (xs ?? []).map((x) => (x.api_id === m.api_id ? { ...x, hidden } : x)));
  };

  return (
    <div className="page">
      <div className="page-header">
        <div>
          <h1>Models</h1>
          <p>Every chat model your free keys can use. Hide a model to keep it out of rankings and auto-routing.</p>
        </div>
        <button className="btn" onClick={refresh} disabled={busy}>
          {busy ? "Refreshing…" : "↻ Refresh model lists"}
        </button>
      </div>
      <div className="row" style={{ marginBottom: 12 }}>
        <input className="input grow" placeholder="Search models…" value={q} onChange={(e) => setQ(e.target.value)} style={{ maxWidth: 360 }} />
        <select className="input" value={provider} onChange={(e) => setProvider(e.target.value)} style={{ maxWidth: 220 }}>
          <option value="">All providers</option>
          {providers.map((p) => (
            <option key={p}>{p}</option>
          ))}
        </select>
        <span className="muted small">{shown.length} models</span>
      </div>
      {items && items.length === 0 ? (
        <div className="card empty">
          <p>No models yet — connect a provider first.</p>
          <button className="btn primary" onClick={() => nav.go("providers")}>
            Set up providers
          </button>
        </div>
      ) : (
        <div className="card" style={{ padding: 0, overflow: "auto" }}>
          <table className="table">
            <thead>
              <tr>
                <th>Model</th>
                <th>Provider</th>
                <th>API id</th>
                <th>Context</th>
                <th>Status</th>
                <th>Enabled</th>
              </tr>
            </thead>
            <tbody>
              {shown.map((m) => (
                <tr key={m.api_id} style={{ opacity: m.hidden ? 0.55 : 1 }}>
                  <td>
                    <b>{m.name}</b>
                    <div className="small muted">{m.family}</div>
                  </td>
                  <td>{m.provider_name}</td>
                  <td>
                    <div className="row">
                      <span className="mono">{m.api_id}</span>
                      <CopyButton text={m.api_id} />
                    </div>
                  </td>
                  <td>{ctxLabel(m.context_length)}</td>
                  <td>
                    {m.cooldown ? (
                      <span className="pill warn" title={m.cooldown.reason}>
                        cooling {Math.max(0, Math.round((m.cooldown.until_ms - Date.now()) / 1000))}s
                      </span>
                    ) : (
                      <span className="pill good">ready</span>
                    )}
                  </td>
                  <td>
                    <Switch checked={!m.hidden} onChange={(v) => setHidden(m, !v)} />
                  </td>
                </tr>
              ))}
            </tbody>
          </table>
        </div>
      )}
    </div>
  );
}
