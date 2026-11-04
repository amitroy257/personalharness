import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import type { Nav } from "../App";
import { call, chat, openExternal } from "../lib/api";
import { DEVICE, renderMarkdown, timeAgo } from "../lib/format";
import type { Conversation, Message, Overview, Preset, RankedModel, RankingsResponse } from "../lib/types";
import { CopyButton, useToast } from "../components/ui";

interface Streaming {
  content: string;
  reasoning: string;
  apiId: string;
  notes: string[];
}

const AUTO: { id: string; preset: Preset; label: string }[] = [
  { id: "auto", preset: "general", label: "Auto · best overall" },
  { id: "auto:coding", preset: "coding", label: "Auto · coding" },
  { id: "auto:reasoning", preset: "reasoning", label: "Auto · reasoning" },
  { id: "auto:fast", preset: "fast", label: "Auto · fastest" },
];

function ModelPicker({ value, onChange, ranked }: { value: string; onChange: (v: string) => void; ranked: RankedModel[] }) {
  const best = (p: Preset) => [...ranked].filter((m) => m.available).sort((a, b) => b.presets[p] - a.presets[p])[0]?.name;
  const top = ranked.slice(0, 8);
  const all = useMemo(() => ranked.flatMap((m) => m.routes.map((r) => ({ id: r.api_id, label: `${m.name} — ${r.provider_name}` }))).sort((a, b) => a.label.localeCompare(b.label)), [ranked]);
  const known = AUTO.some((a) => a.id === value) || all.some((a) => a.id === value);
  return (
    <select className="input" value={value} onChange={(e) => onChange(e.target.value)} aria-label="Model">
      <optgroup label="Automatic — picks the best available model, falls back on rate limits">
        {AUTO.map((a) => {
          const b = best(a.preset);
          return (
            <option key={a.id} value={a.id}>
              {a.label}
              {b ? `  (now: ${b})` : ""}
            </option>
          );
        })}
      </optgroup>
      {top.length > 0 && (
        <optgroup label="Top picks right now">
          {top.map((m) => (
            <option key={`top-${m.family}`} value={m.routes[0].api_id}>
              {m.rank}. {m.name}
              {m.badges.length ? `  · ${m.badges.join(", ")}` : ""}
            </option>
          ))}
        </optgroup>
      )}
      <optgroup label="All models">
        {!known && <option value={value}>{value}</option>}
        {all.map((a) => (
          <option key={a.id} value={a.id}>
            {a.label}
          </option>
        ))}
      </optgroup>
    </select>
  );
}

function AssistantBody({ content }: { content: string }) {
  const html = useMemo(() => renderMarkdown(content), [content]);
  return (
    <div
      className="md"
      dangerouslySetInnerHTML={{ __html: html }}
      onClick={(e) => {
        const a = (e.target as HTMLElement).closest("a");
        if (a) {
          e.preventDefault();
          openExternal(a.getAttribute("href") || "");
        }
      }}
    />
  );
}

function MessageView({ m, onRate }: { m: Message; onRate: (id: string, r: number) => void }) {
  if (m.role === "user") {
    return (
      <div className="msg user">
        <div className="bubble">{m.content}</div>
      </div>
    );
  }
  const tps = m.completion_tokens && m.duration_ms ? m.completion_tokens / (m.duration_ms / 1000) : null;
  return (
    <div className="msg assistant">
      {m.reasoning && (
        <details className="reasoning">
          <summary>Thinking</summary>
          <pre>{m.reasoning}</pre>
        </details>
      )}
      {m.content ? (
        <div className="bubble">
          <AssistantBody content={m.content} />
        </div>
      ) : null}
      {m.notes && <div className="notes">{m.notes}</div>}
      {m.content && (
        <div className="meta">
          {m.api_id && <span className="chip">{m.api_id}</span>}
          {m.duration_ms != null && <span>{(m.duration_ms / 1000).toFixed(1)}s</span>}
          {m.completion_tokens != null && <span>{m.completion_tokens} tok</span>}
          {tps && tps > 0 && <span>{tps.toFixed(0)} tok/s</span>}
          <button className={m.rating > 0 ? "on" : ""} title="Good answer (improves your rankings)" onClick={() => onRate(m.id, m.rating > 0 ? 0 : 1)}>
            👍
          </button>
          <button className={m.rating < 0 ? "on" : ""} title="Bad answer (improves your rankings)" onClick={() => onRate(m.id, m.rating < 0 ? 0 : -1)}>
            👎
          </button>
          <CopyButton text={m.content} className="" label="Copy" />
        </div>
      )}
    </div>
  );
}

const SUGGESTIONS = [
  "Explain how transformers use attention, with a tiny worked example.",
  "Write a Python script that renames photos by their EXIF date.",
  "What are the trade-offs between SQLite and Postgres for a desktop app?",
  "Give me a 5-day plan to learn Rust as a JavaScript developer.",
];

export default function ChatPage({
  nav,
  overview,
  forcedModel,
  onForcedModelUsed,
  dataVersion,
}: {
  nav: Nav;
  overview: Overview | null;
  forcedModel: string | null;
  onForcedModelUsed: () => void;
  dataVersion: number;
}) {
  const toast = useToast();
  const [convs, setConvs] = useState<Conversation[]>([]);
  const [activeId, setActiveId] = useState<string | null>(null);
  const [messages, setMessages] = useState<Message[]>([]);
  const [model, setModel] = useState<string>("auto");
  const [ranked, setRanked] = useState<RankedModel[]>([]);
  const [input, setInput] = useState("");
  const [busy, setBusy] = useState(false);
  const [streaming, setStreaming] = useState<Streaming | null>(null);
  const scroller = useRef<HTMLDivElement>(null);
  const textarea = useRef<HTMLTextAreaElement>(null);
  const activeRef = useRef<string | null>(null);
  const [drawer, setDrawer] = useState(false);

  const loadConvs = useCallback(() => call<Conversation[]>("list_conversations").then(setConvs).catch(() => {}), []);

  useEffect(() => {
    loadConvs();
    call<RankingsResponse>("rankings", { view: "recommended", preset: "general" })
      .then((r) => setRanked(r.items))
      .catch(() => {});
  }, [loadConvs]);

  useEffect(() => {
    if (overview && !forcedModel && model === "auto" && overview.default_model) setModel(overview.default_model);
    // only on first overview
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [overview?.default_model]);

  useEffect(() => {
    if (forcedModel) {
      setModel(forcedModel);
      setActiveId(null);
      activeRef.current = null;
      setMessages([]);
      onForcedModelUsed();
      textarea.current?.focus();
    }
  }, [forcedModel, onForcedModelUsed]);

  // changes synced from another device: refresh the list and the open chat
  useEffect(() => {
    if (!dataVersion) return;
    loadConvs();
    const id = activeRef.current;
    if (id && !busy) {
      call<{ messages: Message[] }>("get_conversation", { id })
        .then((r) => setMessages(r.messages))
        .catch(() => newChatRef.current());
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [dataVersion]);

  useEffect(() => {
    const el = scroller.current;
    if (el) el.scrollTop = el.scrollHeight;
  }, [messages, streaming]);

  const openConv = async (c: Conversation) => {
    if (busy) return;
    setDrawer(false);
    setActiveId(c.id);
    activeRef.current = c.id;
    setModel(c.model || "auto");
    const r = await call<{ messages: Message[] }>("get_conversation", { id: c.id });
    setMessages(r.messages);
  };

  const newChat = () => {
    if (busy) return;
    setDrawer(false);
    setActiveId(null);
    activeRef.current = null;
    setMessages([]);
    textarea.current?.focus();
  };

  const newChatRef = useRef(newChat);
  newChatRef.current = newChat;

  const changeModel = (v: string) => {
    setModel(v);
    if (activeId) call("update_conversation", { id: activeId, model: v }).catch(() => {});
  };

  const send = async (text?: string) => {
    const content = (text ?? input).trim();
    if (!content || busy) return;
    setInput("");
    setBusy(true);
    setStreaming({ content: "", reasoning: "", apiId: "", notes: [] });
    try {
      await chat({ conversation_id: activeRef.current, model, content }, (e) => {
        switch (e.type) {
          case "conversation":
            activeRef.current = e.id;
            setActiveId(e.id);
            break;
          case "user_message":
            setMessages((m) => [...m, e.message]);
            break;
          case "attempt":
            setStreaming((s) => (s ? { ...s, apiId: e.api_id } : s));
            break;
          case "fallback":
            setStreaming((s) => (s ? { ...s, notes: [...s.notes, `${e.api_id} ${e.status ? `(${e.status})` : ""} failed — trying the next route…`] } : s));
            break;
          case "delta":
            setStreaming((s) => (s ? { ...s, content: s.content + e.content } : s));
            break;
          case "reasoning":
            setStreaming((s) => (s ? { ...s, reasoning: s.reasoning + e.content } : s));
            break;
          case "done":
            setMessages((m) => [...m, e.message]);
            setStreaming(null);
            break;
          case "error":
            if (e.message) setMessages((m) => [...m, e.message!]);
            else toast(e.error);
            setStreaming(null);
            break;
        }
      });
    } catch (err) {
      toast(String(err));
    } finally {
      setBusy(false);
      setStreaming(null);
      loadConvs();
      nav.refreshOverview();
    }
  };

  const rate = async (id: string, rating: number) => {
    await call("rate_message", { id, rating });
    setMessages((ms) => ms.map((m) => (m.id === id ? { ...m, rating } : m)));
  };

  const del = async (id: string) => {
    await call("delete_conversation", { id });
    if (id === activeId) newChat();
    loadConvs();
  };

  const noProviders = overview && overview.providers_configured === 0;

  return (
    <div className={`chat ${drawer ? "drawer-open" : ""}`}>
      {drawer && <div className="drawer-backdrop" onClick={() => setDrawer(false)} />}
      <div className="conv-list">
        <button className="btn primary" onClick={newChat} style={{ marginBottom: 8 }}>
          + New chat
        </button>
        {convs.length === 0 && <div className="muted small" style={{ padding: 10 }}>Your chats are stored only on {DEVICE}.</div>}
        {convs.map((c) => (
          <div key={c.id} className={`conv ${c.id === activeId ? "active" : ""}`} onClick={() => openConv(c)}>
            <div style={{ minWidth: 0 }}>
              <div className="t">{c.title || "Untitled"}</div>
              <div className="small muted">{timeAgo(c.updated_at)}</div>
            </div>
            <button
              className="btn ghost sm x"
              title="Delete chat"
              onClick={(e) => {
                e.stopPropagation();
                del(c.id);
              }}
            >
              ✕
            </button>
          </div>
        ))}
      </div>
      <div className="chat-main">
        <div className="chat-top">
          <button className="btn sm mobile-only" onClick={() => setDrawer(true)} aria-label="Show chats">
            ☰ Chats
          </button>
          <span className="muted small desktop-only">Model</span>
          <ModelPicker value={model} onChange={changeModel} ranked={ranked} />
          <button className="btn sm" onClick={() => nav.go("rankings")}>
            Why this model?
          </button>
        </div>
        <div className="messages" ref={scroller}>
          <div className="messages-inner">
            {noProviders && (
              <div className="banner">
                <div className="grow">
                  <b>Add a free API key to start.</b> Groq, Google Gemini, Cerebras, Mistral, GitHub Models and others give free access — keys stay on {DEVICE}.
                </div>
                <button className="btn primary" onClick={() => nav.go("providers")}>
                  Set up providers
                </button>
              </div>
            )}
            {messages.length === 0 && !streaming && !noProviders && (
              <div className="empty">
                <h2 style={{ margin: 0, color: "var(--text)" }}>What can I help with?</h2>
                <p>
                  {model.startsWith("auto")
                    ? "Auto mode picks the strongest available free model and switches automatically if one is rate-limited."
                    : `Using ${model}.`}
                </p>
                <div className="suggestions">
                  {SUGGESTIONS.map((s) => (
                    <button key={s} className="card suggestion" onClick={() => send(s)}>
                      {s}
                    </button>
                  ))}
                </div>
              </div>
            )}
            {messages.map((m) => (
              <MessageView key={m.id} m={m} onRate={rate} />
            ))}
            {streaming && (
              <div className="msg assistant">
                {streaming.notes.map((n, i) => (
                  <div key={i} className="notes">
                    {n}
                  </div>
                ))}
                {streaming.reasoning && (
                  <details className="reasoning" open={!streaming.content}>
                    <summary>Thinking…</summary>
                    <pre>{streaming.reasoning}</pre>
                  </details>
                )}
                <div className="bubble">
                  {streaming.content ? <AssistantBody content={streaming.content} /> : <span className="muted">{streaming.apiId ? `${streaming.apiId} is thinking ` : "Routing "}</span>}
                  <span className="typing" />
                </div>
              </div>
            )}
          </div>
        </div>
        <div className="composer">
          <div className="composer-inner">
            <textarea
              ref={textarea}
              className="input"
              rows={1}
              placeholder={window.innerWidth < 760 ? "Message…" : "Message… (Enter to send, Shift+Enter for a new line)"}
              value={input}
              onChange={(e) => {
                setInput(e.target.value);
                e.target.style.height = "auto";
                e.target.style.height = `${Math.min(220, e.target.scrollHeight)}px`;
              }}
              onKeyDown={(e) => {
                if (e.key === "Enter" && !e.shiftKey && !e.nativeEvent.isComposing) {
                  e.preventDefault();
                  send();
                }
              }}
            />
            {busy ? (
              <button className="btn" onClick={() => activeRef.current && call("stop_generation", { conversation_id: activeRef.current })}>
                Stop
              </button>
            ) : (
              <button className="btn primary" onClick={() => send()} disabled={!input.trim()}>
                Send
              </button>
            )}
          </div>
        </div>
      </div>
    </div>
  );
}
