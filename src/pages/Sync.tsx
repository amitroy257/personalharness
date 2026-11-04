import { useCallback, useEffect, useState } from "react";
import type { Nav } from "../App";
import { call, openExternal } from "../lib/api";
import { DEVICE, timeAgo } from "../lib/format";
import { useToast } from "../components/ui";

interface SyncStatus {
  signed_in: boolean;
  email?: string;
  server?: string;
  device?: string;
  last_sync_at?: number;
  last_error?: string | null;
  pending_changes?: number;
}

const GUIDE = "https://github.com/amitroy257/personalharness/blob/claude/inspiring-hawking-qbjayu/docs/SYNC_SERVER.md";

export default function SyncPage({ nav, onChanged }: { nav: Nav; onChanged: () => void }) {
  const toast = useToast();
  const [st, setSt] = useState<SyncStatus | null>(null);
  const [mode, setMode] = useState<"login" | "signup">("login");
  const [server, setServer] = useState(() => {
    try {
      return localStorage.getItem("ph_sync_server") || "";
    } catch {
      return "";
    }
  });
  const [email, setEmail] = useState("");
  const [password, setPassword] = useState("");
  const [confirm, setConfirm] = useState("");
  const [code, setCode] = useState("");
  const [understood, setUnderstood] = useState(false);
  const [busy, setBusy] = useState<string | null>(null);
  const [delPw, setDelPw] = useState("");

  const load = useCallback(() => call<SyncStatus>("sync_status").then(setSt), []);
  useEffect(() => {
    load();
    const t = window.setInterval(load, 5000);
    return () => window.clearInterval(t);
  }, [load]);

  const run = async (label: string, fn: () => Promise<unknown>, done?: string) => {
    setBusy(label);
    try {
      await fn();
      await load();
      onChanged();
      nav.refreshOverview();
      if (done) toast(done);
    } catch (e) {
      toast(String(e));
    } finally {
      setBusy(null);
    }
  };

  const submit = () => {
    try {
      localStorage.setItem("ph_sync_server", server.trim());
    } catch {
      /* ignore */
    }
    if (mode === "signup") {
      if (password !== confirm) return toast("Passwords do not match");
      return run(
        "Creating your encrypted account…",
        () => call("sync_signup", { server, email, password, signup_code: code }),
        "Account created — this device is now syncing",
      ).then(() => {
        setPassword("");
        setConfirm("");
      });
    }
    return run("Unlocking your data…", () => call("sync_login", { server, email, password }), "Signed in — your data is syncing").then(() =>
      setPassword(""),
    );
  };

  if (!st) return <div className="page muted">Loading…</div>;

  return (
    <div className="page">
      <div className="page-header">
        <div>
          <h1>Account & Sync</h1>
          <p>Use the same chats, providers, keys and settings on your phone and computers. Everything is encrypted on {DEVICE} before it is uploaded.</p>
        </div>
      </div>

      <div className="grid-2">
        {st.signed_in ? (
          <div className="card stack">
            <div className="row">
              <h3 className="grow">Signed in</h3>
              <span className={`pill ${st.last_error ? "bad" : "good"}`}>{st.last_error ? "Sync error" : "● Syncing"}</span>
            </div>
            <table className="table small">
              <tbody>
                <tr>
                  <td className="muted">Account</td>
                  <td>
                    <b>{st.email}</b>
                  </td>
                </tr>
                <tr>
                  <td className="muted">Server</td>
                  <td className="mono" style={{ wordBreak: "break-all" }}>
                    {st.server}
                  </td>
                </tr>
                <tr>
                  <td className="muted">This device</td>
                  <td>{st.device}</td>
                </tr>
                <tr>
                  <td className="muted">Last sync</td>
                  <td>{timeAgo(st.last_sync_at ?? 0)}</td>
                </tr>
                <tr>
                  <td className="muted">Waiting to upload</td>
                  <td>{st.pending_changes ?? 0} change(s)</td>
                </tr>
              </tbody>
            </table>
            {st.last_error && <div className="banner bad small">{st.last_error}</div>}
            <div className="row">
              <button className="btn primary" disabled={!!busy} onClick={() => run("Syncing…", () => call("sync_now"), "Up to date")}>
                {busy === "Syncing…" ? "Syncing…" : "Sync now"}
              </button>
              <button className="btn" disabled={!!busy} onClick={() => run("Signing out…", () => call("sync_logout"), "Signed out — data on this device is kept")}>
                Sign out of this device
              </button>
            </div>
            <hr className="sep" />
            <details>
              <summary className="small" style={{ color: "var(--bad)" }}>
                Delete account and all synced data
              </summary>
              <div className="stack" style={{ marginTop: 10 }}>
                <div className="small muted">
                  Removes your account and every encrypted record from the server. Data stored locally on your devices is not deleted.
                </div>
                <input className="input" type="password" placeholder="Your password" value={delPw} onChange={(e) => setDelPw(e.target.value)} />
                <button
                  className="btn danger"
                  disabled={!delPw || !!busy}
                  onClick={() => {
                    if (!window.confirm("Permanently delete your sync account and all synced data?")) return;
                    run("Deleting…", () => call("sync_delete_account", { password: delPw }), "Account deleted").then(() => setDelPw(""));
                  }}
                >
                  Delete account
                </button>
              </div>
            </details>
          </div>
        ) : (
          <div className="card stack">
            <div className="tabs" style={{ justifySelf: "start" }}>
              <button className={`tab ${mode === "login" ? "active" : ""}`} onClick={() => setMode("login")}>
                Sign in
              </button>
              <button className={`tab ${mode === "signup" ? "active" : ""}`} onClick={() => setMode("signup")}>
                Create account
              </button>
            </div>
            <label className="field">
              Sync server
              <input
                className="input mono"
                inputMode="url"
                autoCapitalize="off"
                autoCorrect="off"
                placeholder="https://your-sync-server.fly.dev"
                value={server}
                onChange={(e) => setServer(e.target.value)}
              />
            </label>
            <label className="field">
              Email
              <input className="input" type="email" autoCapitalize="off" value={email} onChange={(e) => setEmail(e.target.value)} />
            </label>
            <label className="field">
              Password
              <input className="input" type="password" value={password} onChange={(e) => setPassword(e.target.value)} />
            </label>
            {mode === "signup" && (
              <>
                <label className="field">
                  Confirm password
                  <input className="input" type="password" value={confirm} onChange={(e) => setConfirm(e.target.value)} />
                </label>
                <label className="field">
                  Sign-up code (only if the server owner gave you one)
                  <input className="input" value={code} onChange={(e) => setCode(e.target.value)} />
                </label>
                <div className="banner warn small">
                  <div>
                    <b>Your password is the only key to your synced data.</b> It never leaves your devices, so nobody — including the server owner
                    — can reset it. If you forget it, the synced copy cannot be recovered (data on each device stays).
                  </div>
                </div>
                <label className="switch small">
                  <input type="checkbox" checked={understood} onChange={(e) => setUnderstood(e.target.checked)} />
                  <span>I understand and will keep my password safe</span>
                </label>
              </>
            )}
            <button
              className="btn primary"
              disabled={!!busy || !server.trim() || !email.trim() || !password || (mode === "signup" && (!understood || password.length < 10))}
              onClick={submit}
            >
              {busy ?? (mode === "signup" ? "Create account & start syncing" : "Sign in & sync")}
            </button>
            {mode === "signup" && password.length > 0 && password.length < 10 && <div className="small muted">Use at least 10 characters.</div>}
            <div className="small muted">
              Need a server? <a onClick={() => openExternal(GUIDE)}>Run your own in a few minutes ↗</a> (free on Fly.io, a VPS or a home PC).
            </div>
          </div>
        )}

        <div className="card stack">
          <h3>How your data is protected</h3>
          <ul className="small" style={{ margin: 0, paddingLeft: 18 }}>
            <li>
              <b>End-to-end encrypted.</b> Chats, keys and settings are encrypted on your device (XChaCha20-Poly1305) with a key derived from your
              password (Argon2id). The server stores only unreadable data — not even record names.
            </li>
            <li>
              <b>Zero knowledge.</b> Your password never leaves your devices; the server keeps only a hash of a separate login key.
            </li>
            <li>
              <b>Your server.</b> The sync server is self-hosted, and only you (and people you give a sign-up code) can create accounts.
            </li>
          </ul>
          <h3 style={{ marginTop: 6 }}>What syncs</h3>
          <ul className="small" style={{ margin: 0, paddingLeft: 18 }}>
            <li>Chats and messages, including deletions and 👍/👎 ratings</li>
            <li>Providers, API keys, hidden models, ranking and chat settings</li>
          </ul>
          <div className="small muted">
            Not synced (per device): the local API port and token, and usage statistics used for your personal rankings. Edits made on two
            devices at the same time: the most recent one wins.
          </div>
        </div>
      </div>
    </div>
  );
}
