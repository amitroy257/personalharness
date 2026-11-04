import { useCallback, useEffect, useState } from "react";
import type { Nav } from "../App";
import { call } from "../lib/api";
import { DEVICE } from "../lib/format";
import type { ServerStatus, SettingsView } from "../lib/types";
import { CopyButton, Switch, useToast } from "../components/ui";

export default function ApiPage({ nav }: { nav: Nav }) {
  const toast = useToast();
  const [st, setSt] = useState<ServerStatus | null>(null);
  const [s, setS] = useState<SettingsView | null>(null);
  const [reveal, setReveal] = useState(false);
  const [port, setPort] = useState("");

  const load = useCallback(async () => {
    const [a, b] = await Promise.all([call<ServerStatus>("server_status"), call<SettingsView>("get_settings")]);
    setSt(a);
    setS(b);
    setPort(String(b.server.port));
  }, []);
  useEffect(() => {
    load();
  }, [load]);

  const update = async (patch: Record<string, unknown>) => {
    try {
      setS(await call<SettingsView>("update_settings", patch));
      setSt(await call<ServerStatus>("server_status"));
      nav.refreshOverview();
    } catch (e) {
      toast(String(e));
      load();
    }
  };

  if (!s || !st) return <div className="page muted">Loading…</div>;
  const base = st.base_url;
  const token = s.server.token;
  const masked = token.slice(0, 6) + "•".repeat(20);

  const curl = `curl ${base}/chat/completions \\
  -H "Authorization: Bearer ${reveal ? token : "$HARNESS_KEY"}" \\
  -H "Content-Type: application/json" \\
  -d '{"model": "auto", "messages": [{"role": "user", "content": "Hello!"}]}'`;
  const py = `from openai import OpenAI

client = OpenAI(base_url="${base}", api_key="${reveal ? token : "<your token>"}")
r = client.chat.completions.create(
    model="auto",            # or "auto:coding", "auto:fast", "groq/llama-3.3-70b-versatile", ...
    messages=[{"role": "user", "content": "Hello!"}],
    extra_body={"models": ["auto:fast"]},   # optional OpenRouter-style fallbacks
)
print(r.model, r.choices[0].message.content)`;
  const js = `import OpenAI from "openai";

const client = new OpenAI({ baseURL: "${base}", apiKey: "${reveal ? token : "<your token>"}" });
const r = await client.chat.completions.create({ model: "auto", messages: [{ role: "user", content: "Hello!" }] });
console.log(r.model, r.choices[0].message.content);`;

  return (
    <div className="page">
      <div className="page-header">
        <div>
          <h1>Local API</h1>
          <p>An OpenAI-compatible endpoint on {DEVICE}, so Cursor, VS Code extensions, scripts and other apps can use your free models like OpenRouter.</p>
        </div>
      </div>

      <div className="grid-2">
        <div className="card stack">
          <div className="row">
            <h3 className="grow">Server</h3>
            <span className={`pill ${st.running ? "good" : "bad"}`}>{st.running ? "Running" : st.enabled ? "Not running" : "Off"}</span>
          </div>
          {st.error && <div className="banner bad small">{st.error}</div>}
          <label className="field">
            Base URL
            <div className="row">
              <input className="input mono grow" readOnly value={base} />
              <CopyButton text={base} />
            </div>
          </label>
          <label className="field">
            API key (local token)
            <div className="row">
              <input className="input mono grow" readOnly value={reveal ? token : masked} />
              <button className="btn sm" onClick={() => setReveal(!reveal)}>
                {reveal ? "Hide" : "Show"}
              </button>
              <CopyButton text={token} />
            </div>
          </label>
          <div className="row">
            <Switch checked={s.server.enabled} onChange={(v) => update({ server: { enabled: v } })} label="Enabled" />
            <div className="grow" />
            <label className="field" style={{ width: 110 }}>
              Port
              <input className="input" value={port} onChange={(e) => setPort(e.target.value.replace(/\D/g, ""))} />
            </label>
            <button className="btn" style={{ alignSelf: "end" }} disabled={port === String(s.server.port)} onClick={() => update({ server: { port: Number(port) } })}>
              Apply
            </button>
          </div>
          <div className="row">
            <button
              className="btn danger sm"
              onClick={async () => {
                const r = await call<{ token: string }>("regenerate_token");
                setS({ ...s, server: { ...s.server, token: r.token } });
                toast("New token created — update your apps");
              }}
            >
              Regenerate token
            </button>
            <span className="small muted">Listens on 127.0.0.1 only. Other devices and websites cannot reach it.</span>
          </div>
        </div>

        <div className="card stack">
          <h3>Routing</h3>
          <div className="small muted">
            Like OpenRouter: when a free model is rate-limited or down, the request moves to the same model on another provider, then to the next-best
            model. Rate-limited routes cool down before being retried.
          </div>
          <label className="field">
            Max attempts per request
            <input
              className="input"
              type="number"
              min={1}
              max={10}
              value={s.routing.max_attempts}
              onChange={(e) => update({ routing: { max_attempts: Number(e.target.value) } })}
            />
          </label>
          <label className="field">
            Default cooldown after a rate limit (seconds)
            <input
              className="input"
              type="number"
              min={5}
              max={3600}
              value={s.routing.cooldown_secs}
              onChange={(e) => update({ routing: { cooldown_secs: Number(e.target.value) } })}
            />
          </label>
          <Switch
            checked={s.routing.same_model_fallback}
            onChange={(v) => update({ routing: { same_model_fallback: v } })}
            label="If a specific provider/model fails, try the same model on other providers"
          />
          <div className="small">
            <b>Model ids:</b> <span className="mono">auto</span>, <span className="mono">auto:coding</span>, <span className="mono">auto:reasoning</span>,{" "}
            <span className="mono">auto:fast</span>, any <span className="mono">provider/model</span> from the Models page, or a bare model name (e.g.{" "}
            <span className="mono">llama-3.3-70b</span>) to use whichever provider serves it. Send <span className="mono">models: [...]</span> for explicit
            fallbacks.
          </div>
        </div>
      </div>

      <div className="card stack" style={{ marginTop: 14 }}>
        <div className="row">
          <h3 className="grow">Use it</h3>
          <span className="small muted">Responses include the model actually used (`model`, `x-harness-model` header).</span>
        </div>
        <div className="small muted">curl</div>
        <pre className="code-block">{curl}</pre>
        <div className="small muted">Python (openai SDK)</div>
        <pre className="code-block">{py}</pre>
        <div className="small muted">JavaScript / TypeScript</div>
        <pre className="code-block">{js}</pre>
        <div className="small">
          <b>In other apps:</b> choose “OpenAI-compatible” / “custom OpenAI base URL”, set the base URL to <span className="mono">{base}</span>, the API key
          to your token, and the model to <span className="mono">auto</span> (or any id from Models). Also available:{" "}
          <span className="mono">GET /v1/models</span> and <span className="mono">GET /v1/rankings?view=hot</span>.
        </div>
      </div>
    </div>
  );
}
