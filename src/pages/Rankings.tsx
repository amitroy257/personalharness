import { useCallback, useEffect, useState } from "react";
import type { Nav } from "../App";
import { call } from "../lib/api";
import { ctxLabel, releasedLabel, timeAgo } from "../lib/format";
import type { Preset, RankedModel, RankingsResponse, ViewName } from "../lib/types";
import { Badges, Bar, CopyButton, useToast } from "../components/ui";

const VIEWS: { id: ViewName; label: string; blurb: string }[] = [
  { id: "recommended", label: "Recommended", blurb: "What to use now: strength, popularity, momentum and your own results, for the models you can call." },
  { id: "strongest", label: "Strongest", blurb: "Quality from public leaderboards (LMArena; Artificial Analysis if you add a key)." },
  { id: "popular", label: "Most used", blurb: "Global usage: OpenRouter token volume (optional key), Hugging Face downloads, LMArena votes." },
  { id: "hot", label: "Hot 🔥", blurb: "Momentum: Hugging Face trending, how recently it was released, OpenRouter growth." },
  { id: "mine", label: "My usage", blurb: "Your own history on this Mac: calls, success rate, speed and 👍/👎." },
];

const PRESETS: { id: Preset; label: string }[] = [
  { id: "general", label: "General" },
  { id: "coding", label: "Coding" },
  { id: "reasoning", label: "Reasoning" },
  { id: "fast", label: "Fast" },
];

function Row({ m, view, nav, expanded, toggle }: { m: RankedModel; view: ViewName; nav: Nav; expanded: boolean; toggle: () => void }) {
  const first = m.routes[0];
  return (
    <div className={`rank-row ${m.available ? "" : "unavailable"}`}>
      <div className="rank-num">{m.rank}</div>
      <div style={{ minWidth: 0 }}>
        <div className="row">
          <span className="model-name">{m.name}</span>
          <Badges badges={m.badges} />
        </div>
        <div className="row small muted" style={{ marginTop: 4 }}>
          {m.routes.map((r) => (
            <span key={r.api_id} className={`chip ${r.cooling_until ? "cool" : ""}`} title={r.cooling_until ? "Rate-limited, cooling down" : r.api_id}>
              {r.provider_name}
            </span>
          ))}
          {m.released && <span>{releasedLabel(m.released)}</span>}
          {first?.context_length && <span>{ctxLabel(first.context_length)} ctx</span>}
          {m.personal && m.personal.attempts > 0 && (
            <span>
              you: {m.personal.attempts} calls · {Math.round(m.personal.success_rate * 100)}% ok
            </span>
          )}
        </div>
      </div>
      <div className="bars">
        <Bar label="Strength" value={view === "recommended" ? m.strength : m.strength} color="var(--bar-strength)" estimated={m.strength_estimated} />
        <Bar label="Popularity" value={m.popularity} color="var(--bar-pop)" />
        <Bar label="Hot" value={m.hot} color="var(--bar-hot)" />
        <Bar label="Speed" value={m.speed} color="var(--bar-speed)" />
      </div>
      <div className="row actions" style={{ justifyContent: "flex-end" }}>
        <div style={{ textAlign: "right", minWidth: 44 }}>
          <div style={{ fontSize: 18, fontWeight: 700 }}>{m.score.toFixed(0)}</div>
          <div className="small muted">score</div>
        </div>
        <button className="btn sm primary" onClick={() => nav.useModel(first.api_id)}>
          Chat
        </button>
        <CopyButton text={first.api_id} label="Copy ID" />
        <button className="btn sm ghost" onClick={toggle} aria-expanded={expanded}>
          {expanded ? "Hide" : "Why?"}
        </button>
      </div>
      {expanded && (
        <ul className="why">
          {m.why.map((w, i) => (
            <li key={i}>{w}</li>
          ))}
          <li>
            Routes in order: {m.routes.map((r) => r.api_id).join(" → ")}
          </li>
        </ul>
      )}
    </div>
  );
}

export default function RankingsPage({ nav }: { nav: Nav }) {
  const toast = useToast();
  const [view, setView] = useState<ViewName>("recommended");
  const [preset, setPreset] = useState<Preset>("general");
  const [data, setData] = useState<RankingsResponse | null>(null);
  const [expanded, setExpanded] = useState<Set<string>>(new Set());
  const [refreshing, setRefreshing] = useState(false);

  const load = useCallback(() => {
    call<RankingsResponse>("rankings", { view, preset }).then(setData).catch((e) => toast(String(e)));
  }, [view, preset, toast]);

  useEffect(load, [load]);

  const refresh = async () => {
    setRefreshing(true);
    try {
      await call("refresh_signals");
      await call("refresh_models");
      load();
      toast("Rankings refreshed");
    } catch (e) {
      toast(String(e));
    } finally {
      setRefreshing(false);
    }
  };

  const items = data?.items ?? [];
  const top = items.find((m) => m.available);
  const v = VIEWS.find((x) => x.id === view)!;

  return (
    <div className="page">
      <div className="page-header">
        <div>
          <h1>Model rankings</h1>
          <p>{v.blurb}</p>
        </div>
        <button className="btn" onClick={refresh} disabled={refreshing}>
          {refreshing ? "Refreshing…" : "↻ Refresh data"}
        </button>
      </div>

      <div className="row" style={{ marginBottom: 14, justifyContent: "space-between" }}>
        <div className="tabs">
          {VIEWS.map((x) => (
            <button key={x.id} className={`tab ${view === x.id ? "active" : ""}`} onClick={() => setView(x.id)}>
              {x.label}
            </button>
          ))}
        </div>
        {view === "recommended" && (
          <div className="tabs">
            {PRESETS.map((p) => (
              <button key={p.id} className={`tab ${preset === p.id ? "active" : ""}`} onClick={() => setPreset(p.id)}>
                {p.label}
              </button>
            ))}
          </div>
        )}
      </div>

      {data && items.length === 0 && (
        <div className="card empty">
          {view === "mine" ? (
            <p>No personal history yet. Chat or call the local API — your stats are recorded only on this Mac.</p>
          ) : (
            <>
              <p>No models available yet. Connect at least one free provider to see rankings for models you can actually use.</p>
              <button className="btn primary" onClick={() => nav.go("providers")}>
                Set up providers
              </button>
            </>
          )}
        </div>
      )}

      {view === "recommended" && top && (
        <div className="card hero" style={{ marginBottom: 14 }}>
          <div>
            <div className="small muted">Use this now for {PRESETS.find((p) => p.id === preset)!.label.toLowerCase()}</div>
            <div className="big">{top.name}</div>
            <div className="small muted">
              via {top.routes.filter((r) => !r.cooling_until).map((r) => r.provider_name).join(", ") || top.routes[0]?.provider_name} · or call{" "}
              <span className="mono">{preset === "general" ? "auto" : `auto:${preset}`}</span> to always get the current best
            </div>
            <ul className="small" style={{ margin: "8px 0 0", paddingLeft: 18 }}>
              {top.why.slice(0, 3).map((w, i) => (
                <li key={i}>{w}</li>
              ))}
            </ul>
          </div>
          <div className="row">
            <button className="btn primary" onClick={() => nav.useModel(preset === "general" ? "auto" : `auto:${preset}`)}>
              Chat in auto mode
            </button>
            <button className="btn" onClick={() => nav.useModel(top.routes[0].api_id)}>
              Chat with {top.name}
            </button>
          </div>
        </div>
      )}

      <div className="rank-list">
        {items.map((m) => (
          <Row
            key={m.family}
            m={m}
            view={view}
            nav={nav}
            expanded={expanded.has(m.family)}
            toggle={() =>
              setExpanded((s) => {
                const n = new Set(s);
                if (n.has(m.family)) n.delete(m.family);
                else n.add(m.family);
                return n;
              })
            }
          />
        ))}
      </div>

      {data && (
        <div className="card" style={{ marginTop: 18 }}>
          <h3>Data sources</h3>
          <p className="small muted" style={{ marginTop: 0 }}>
            Public data is fetched anonymously and cached locally{data.signals.fetched_at ? ` (last refresh ${timeAgo(data.signals.fetched_at)})` : ""}. Nothing about you is
            uploaded. Strength marked “~” is an estimate for models missing from public leaderboards.
          </p>
          <div className="sources">
            {data.signals.sources.map((s) => (
              <div key={s.id} className="row">
                <span className={`pill ${s.ok ? "good" : "bad"}`}>{s.ok ? "OK" : "Error"}</span>
                <b>{s.label}</b>
                <span className="muted">
                  {s.detail}
                  {s.count ? ` · ${s.count} entries` : ""}
                  {s.error ? ` · ${s.error}` : ""}
                </span>
              </div>
            ))}
          </div>
          <p className="small muted" style={{ marginBottom: 0 }}>
            Want global “most used” token data or benchmark indexes? Add an optional OpenRouter or Artificial Analysis key in{" "}
            <a onClick={() => nav.go("settings")}>Privacy & Settings</a>.
          </p>
        </div>
      )}
    </div>
  );
}
