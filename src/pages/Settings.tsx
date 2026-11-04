import { useCallback, useEffect, useState } from "react";
import type { Nav } from "../App";
import { call, copyText } from "../lib/api";
import type { PrivacyInfo, SettingsView } from "../lib/types";
import { Switch, useToast } from "../components/ui";

function SecretField({ label, set, hint, onSave }: { label: string; set: boolean; hint: string; onSave: (v: string) => void }) {
  const [v, setV] = useState("");
  return (
    <label className="field">
      {label}
      <div className="row">
        <input
          className="input grow"
          type="password"
          autoComplete="off"
          value={v}
          placeholder={set ? `Saved (${hint}) — paste to replace` : "Optional"}
          onChange={(e) => setV(e.target.value)}
        />
        <button
          className="btn"
          disabled={!v.trim()}
          onClick={() => {
            onSave(v.trim());
            setV("");
          }}
        >
          Save
        </button>
        {set && (
          <button className="btn danger" onClick={() => onSave("")}>
            Remove
          </button>
        )}
      </div>
    </label>
  );
}

export default function SettingsPage({ nav }: { nav: Nav }) {
  const toast = useToast();
  const [s, setS] = useState<SettingsView | null>(null);
  const [p, setP] = useState<PrivacyInfo | null>(null);
  const [sys, setSys] = useState("");

  const load = useCallback(async () => {
    const [a, b] = await Promise.all([call<SettingsView>("get_settings"), call<PrivacyInfo>("privacy_info")]);
    setS(a);
    setP(b);
    setSys(a.chat.system_prompt);
  }, []);
  useEffect(() => {
    load();
  }, [load]);

  const update = async (patch: Record<string, unknown>) => {
    try {
      setS(await call<SettingsView>("update_settings", patch));
      setP(await call<PrivacyInfo>("privacy_info"));
    } catch (e) {
      toast(String(e));
    }
  };

  const wipe = async (what: string, label: string) => {
    if (!window.confirm(`Permanently delete ${label}? This cannot be undone.`)) return;
    await call("wipe_data", { what });
    toast(`Deleted ${label}`);
    nav.refreshOverview();
    load();
  };

  if (!s || !p) return <div className="page muted">Loading…</div>;
  const r = s.ranking;

  return (
    <div className="page">
      <div className="page-header">
        <div>
          <h1>Privacy & Settings</h1>
          <p>Everything is stored locally inside the app's sandboxed container. There is no account, no cloud sync and no telemetry.</p>
        </div>
      </div>

      <div className="grid-2">
        <div className="card stack">
          <h3>Where your data lives</h3>
          <div className="row">
            <span className="mono small grow" style={{ wordBreak: "break-all" }}>
              {p.data_dir}
            </span>
            <button className="btn sm" onClick={async () => (await copyText(p.data_dir)) && toast("Path copied")}>
              Copy path
            </button>
          </div>
          <ul className="small" style={{ margin: 0, paddingLeft: 18 }}>
            {p.files.map((f) => (
              <li key={f.name}>
                <span className="mono">{f.name}</span> — {f.what}
              </li>
            ))}
            <li>{p.server}</li>
            <li>{p.telemetry}</li>
            <li>Local API requests are logged as metadata only (model, status, timing, token counts) — never prompt text.</li>
          </ul>
        </div>

        <div className="card stack">
          <h3>Network connections this app makes</h3>
          {p.network.length === 0 && <div className="small muted">None right now — no provider connected and public data is off.</div>}
          <ul className="small" style={{ margin: 0, paddingLeft: 18 }}>
            {p.network.map((n, i) => (
              <li key={i}>
                <span className="mono">{n.host}</span> — {n.why}
              </li>
            ))}
          </ul>
        </div>

        <div className="card stack">
          <h3>Ranking data</h3>
          <Switch
            checked={r.public_data}
            onChange={(v) => update({ ranking: { public_data: v } })}
            label="Download public leaderboard & trending data (anonymous)"
          />
          <div className="small muted" style={{ marginTop: -6 }}>
            Off = rankings use only the leaderboard snapshot bundled with the app plus your own usage.
          </div>
          <div className="stack" style={{ opacity: r.public_data ? 1 : 0.5, paddingLeft: 8 }}>
            <Switch checked={r.source_arena} onChange={(v) => update({ ranking: { source_arena: v } })} label="LMArena leaderboards (strength)" />
            <Switch checked={r.source_huggingface} onChange={(v) => update({ ranking: { source_huggingface: v } })} label="Hugging Face trending & downloads" />
            <Switch
              checked={r.source_openrouter_models}
              onChange={(v) => update({ ranking: { source_openrouter_models: v } })}
              label="OpenRouter public model list (release dates)"
            />
          </div>
          <label className="field">
            Refresh every (hours)
            <input className="input" type="number" min={1} max={168} value={r.refresh_hours} onChange={(e) => update({ ranking: { refresh_hours: Number(e.target.value) } })} />
          </label>
          <label className="field">
            How much your own results influence recommendations: {Math.round(r.personal_weight * 100)}%
            <input
              type="range"
              min={0}
              max={1}
              step={0.05}
              value={r.personal_weight}
              onChange={(e) => setS({ ...s, ranking: { ...r, personal_weight: Number(e.target.value) } })}
              onMouseUp={(e) => update({ ranking: { personal_weight: Number((e.target as HTMLInputElement).value) } })}
              onKeyUp={(e) => update({ ranking: { personal_weight: Number((e.target as HTMLInputElement).value) } })}
            />
          </label>
          <SecretField
            label="OpenRouter key — optional, only used to read global token-usage rankings (“Most used”)"
            set={r.openrouter_key_set}
            hint={r.openrouter_key_hint}
            onSave={(v) => update({ ranking: { openrouter_key: v } })}
          />
          <SecretField
            label="Artificial Analysis key — optional, adds intelligence & coding benchmark indexes"
            set={r.artificial_analysis_key_set}
            hint={r.artificial_analysis_key_hint}
            onSave={(v) => update({ ranking: { artificial_analysis_key: v } })}
          />
        </div>

        <div className="card stack">
          <h3>Chat defaults</h3>
          <label className="field">
            Default model for new chats
            <input className="input mono" value={s.chat.default_model} onChange={(e) => setS({ ...s, chat: { ...s.chat, default_model: e.target.value } })} onBlur={(e) => update({ chat: { default_model: e.target.value || "auto" } })} />
          </label>
          <label className="field">
            System prompt
            <textarea className="input" rows={4} value={sys} onChange={(e) => setSys(e.target.value)} onBlur={() => update({ chat: { system_prompt: sys } })} placeholder="Optional instructions for every chat" />
          </label>
          <label className="field">
            Temperature (empty = provider default)
            <input
              className="input"
              type="number"
              step={0.1}
              min={0}
              max={2}
              value={s.chat.temperature ?? ""}
              onChange={(e) => update({ chat: { temperature: e.target.value === "" ? null : Number(e.target.value) } })}
            />
          </label>
        </div>

        <div className="card stack">
          <h3>Delete data</h3>
          <div className="row">
            <button className="btn danger" onClick={() => wipe("chats", "all chats")}>
              Delete all chats
            </button>
            <button className="btn danger" onClick={() => wipe("usage", "usage history & ratings")}>
              Delete usage history
            </button>
            <button className="btn danger" onClick={() => wipe("all", "everything (keys, chats, history, settings)")}>
              Reset everything
            </button>
          </div>
        </div>
      </div>
    </div>
  );
}
