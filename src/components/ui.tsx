import { createContext, useCallback, useContext, useState, type ReactNode } from "react";
import { copyText } from "../lib/api";

const P = { fill: "none", stroke: "currentColor", strokeWidth: 1.8, strokeLinecap: "round" as const, strokeLinejoin: "round" as const };

export const Icon = {
  chat: () => (
    <svg viewBox="0 0 24 24" {...P}>
      <path d="M21 12a8 8 0 0 1-11.6 7.1L4 20l1-4.6A8 8 0 1 1 21 12Z" />
    </svg>
  ),
  trophy: () => (
    <svg viewBox="0 0 24 24" {...P}>
      <path d="M8 21h8M12 17v4M7 4h10v5a5 5 0 0 1-10 0V4ZM7 6H4a3 3 0 0 0 3 4M17 6h3a3 3 0 0 1-3 4" />
    </svg>
  ),
  cube: () => (
    <svg viewBox="0 0 24 24" {...P}>
      <path d="m12 3 8 4.5v9L12 21l-8-4.5v-9L12 3ZM12 12l8-4.5M12 12v9M12 12 4 7.5" />
    </svg>
  ),
  key: () => (
    <svg viewBox="0 0 24 24" {...P}>
      <circle cx="8" cy="15" r="4" />
      <path d="m10.8 12.2 8.7-8.7M17 6l2.5 2.5M15 8l2 2" />
    </svg>
  ),
  plug: () => (
    <svg viewBox="0 0 24 24" {...P}>
      <path d="M9 3v5M15 3v5M7 8h10v3a5 5 0 0 1-10 0V8ZM12 16v5" />
    </svg>
  ),
  shield: () => (
    <svg viewBox="0 0 24 24" {...P}>
      <path d="M12 3 5 6v6c0 4.4 3 7.9 7 9 4-1.1 7-4.6 7-9V6l-7-3Z" />
      <path d="m9 12 2 2 4-4" />
    </svg>
  ),
};

type Toast = (msg: string) => void;
const ToastCtx = createContext<Toast>(() => {});
export const useToast = () => useContext(ToastCtx);

export function ToastProvider({ children }: { children: ReactNode }) {
  const [msg, setMsg] = useState<string | null>(null);
  const show = useCallback((m: string) => {
    setMsg(m);
    window.setTimeout(() => setMsg((cur) => (cur === m ? null : cur)), 2200);
  }, []);
  return (
    <ToastCtx.Provider value={show}>
      {children}
      {msg && <div className="toast">{msg}</div>}
    </ToastCtx.Provider>
  );
}

export function CopyButton({ text, label = "Copy", className = "btn sm" }: { text: string; label?: string; className?: string }) {
  const toast = useToast();
  return (
    <button
      className={className}
      onClick={async () => {
        if (await copyText(text)) toast("Copied");
      }}
    >
      {label}
    </button>
  );
}

export function Switch({ checked, onChange, label }: { checked: boolean; onChange: (v: boolean) => void; label?: ReactNode }) {
  return (
    <label className="switch">
      <input type="checkbox" checked={checked} onChange={(e) => onChange(e.target.checked)} />
      {label && <span>{label}</span>}
    </label>
  );
}

export function Bar({ label, value, color, estimated }: { label: string; value: number | null; color: string; estimated?: boolean }) {
  const v = value == null ? 0 : Math.max(0, Math.min(100, value));
  return (
    <div className="bar" title={value == null ? `${label}: no public data` : `${label}: ${v.toFixed(0)}/100${estimated ? " (estimated)" : ""}`}>
      <div className="lbl">
        <span>{label}</span>
        <span>{value == null ? "—" : `${estimated ? "~" : ""}${v.toFixed(0)}`}</span>
      </div>
      <div className="track">
        <div className={`fill${estimated ? " est" : ""}`} style={{ width: `${v}%`, background: color }} />
      </div>
    </div>
  );
}

const BADGE_CLASS: Record<string, string> = {
  "Best pick": "accent",
  Strongest: "accent",
  "Best for coding": "accent",
  "Most used": "good",
  Trending: "hot",
  Fastest: "good",
};

const BADGE_ICON: Record<string, string> = {
  "Best pick": "★",
  Strongest: "💪",
  "Best for coding": "⌨",
  "Most used": "👥",
  Trending: "🔥",
  Fastest: "⚡",
};

export function Badges({ badges }: { badges: string[] }) {
  return (
    <>
      {badges.map((b) => (
        <span key={b} className={`pill ${BADGE_CLASS[b] || ""}`}>
          {BADGE_ICON[b] ? `${BADGE_ICON[b]} ` : ""}
          {b}
        </span>
      ))}
    </>
  );
}
