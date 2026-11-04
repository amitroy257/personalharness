// Transport: Tauri IPC inside the desktop app, HTTP when the UI is served by the
// headless `harness-server` (or the Vite dev server) in a browser.
import { Channel, invoke } from "@tauri-apps/api/core";
import type { ChatEvent } from "./types";

export const isTauri = typeof window !== "undefined" && "__TAURI_INTERNALS__" in window;

function webToken(): string {
  const m = location.hash.match(/token=([^&]+)/);
  if (m) {
    try {
      sessionStorage.setItem("ph_token", decodeURIComponent(m[1]));
    } catch {
      /* storage unavailable */
    }
    history.replaceState(null, "", location.pathname + location.search);
    return decodeURIComponent(m[1]);
  }
  try {
    return sessionStorage.getItem("ph_token") || "";
  } catch {
    return "";
  }
}

export async function call<T = unknown>(name: string, args: Record<string, unknown> = {}): Promise<T> {
  if (isTauri) {
    return invoke<T>("call", { name, args });
  }
  const r = await fetch(`/__ui/call/${name}`, {
    method: "POST",
    headers: { "Content-Type": "application/json", Authorization: `Bearer ${webToken()}` },
    body: JSON.stringify(args),
  });
  const text = await r.text();
  let j: unknown = null;
  try {
    j = JSON.parse(text);
  } catch {
    /* not json */
  }
  if (!r.ok) {
    const obj = j as { error?: unknown } | null;
    const err = obj?.error;
    const msg = typeof err === "string" ? err : (err as { message?: string } | undefined)?.message;
    throw new Error(msg || `${r.status} ${r.statusText}`);
  }
  return j as T;
}

export async function chat(args: Record<string, unknown>, onEvent: (e: ChatEvent) => void): Promise<void> {
  if (isTauri) {
    const ch = new Channel<ChatEvent>();
    ch.onmessage = onEvent;
    await invoke("chat", { args, onEvent: ch });
    return;
  }
  const r = await fetch("/__ui/chat", {
    method: "POST",
    headers: { "Content-Type": "application/json", Authorization: `Bearer ${webToken()}` },
    body: JSON.stringify(args),
  });
  if (!r.ok || !r.body) {
    onEvent({ type: "error", error: `${r.status} ${r.statusText}`, message: null });
    return;
  }
  const reader = r.body.getReader();
  const dec = new TextDecoder();
  let buf = "";
  for (;;) {
    const { value, done } = await reader.read();
    if (done) break;
    buf += dec.decode(value, { stream: true });
    let i: number;
    while ((i = buf.indexOf("\n\n")) >= 0) {
      const block = buf.slice(0, i);
      buf = buf.slice(i + 2);
      for (const line of block.split("\n")) {
        if (line.startsWith("data: ")) {
          try {
            onEvent(JSON.parse(line.slice(6)) as ChatEvent);
          } catch {
            /* ignore malformed */
          }
        }
      }
    }
  }
}

export async function openExternal(url: string): Promise<void> {
  if (!/^https?:\/\//i.test(url)) return;
  if (isTauri) {
    const { openUrl } = await import("@tauri-apps/plugin-opener");
    await openUrl(url);
  } else {
    window.open(url, "_blank", "noopener,noreferrer");
  }
}

export async function copyText(text: string): Promise<boolean> {
  try {
    await navigator.clipboard.writeText(text);
    return true;
  } catch {
    const ta = document.createElement("textarea");
    ta.value = text;
    document.body.appendChild(ta);
    ta.select();
    const ok = document.execCommand("copy");
    ta.remove();
    return ok;
  }
}
