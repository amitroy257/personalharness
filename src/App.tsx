import { useCallback, useEffect, useState, type ReactElement } from "react";
import { call } from "./lib/api";
import { DEVICE } from "./lib/format";
import type { Overview } from "./lib/types";
import { Icon } from "./components/ui";
import ChatPage from "./pages/Chat";
import RankingsPage from "./pages/Rankings";
import ModelsPage from "./pages/Models";
import ProvidersPage from "./pages/Providers";
import ApiPage from "./pages/ApiPage";
import SettingsPage from "./pages/Settings";

export type Page = "chat" | "rankings" | "models" | "providers" | "api" | "settings";

const NAV: { id: Page; label: string; icon: () => ReactElement }[] = [
  { id: "chat", label: "Chat", icon: Icon.chat },
  { id: "rankings", label: "Rankings", icon: Icon.trophy },
  { id: "models", label: "Models", icon: Icon.cube },
  { id: "providers", label: "Providers", icon: Icon.key },
  { id: "api", label: "Local API", icon: Icon.plug },
  { id: "settings", label: "Privacy & Settings", icon: Icon.shield },
];

export interface Nav {
  go: (p: Page) => void;
  useModel: (apiId: string) => void;
  refreshOverview: () => void;
}

export default function App() {
  const [page, setPage] = useState<Page>("chat");
  const [overview, setOverview] = useState<Overview | null>(null);
  const [chatModel, setChatModel] = useState<string | null>(null);

  const refreshOverview = useCallback(() => {
    call<Overview>("overview").then(setOverview).catch(() => {});
  }, []);

  useEffect(() => {
    refreshOverview();
    const t = window.setInterval(refreshOverview, 20000);
    return () => window.clearInterval(t);
  }, [refreshOverview]);

  useEffect(() => {
    if (overview && overview.providers_configured === 0 && page === "chat" && !sessionStorage.getItem("ph_seen_onboarding")) {
      try {
        sessionStorage.setItem("ph_seen_onboarding", "1");
      } catch {
        /* ignore */
      }
      setPage("providers");
    }
  }, [overview, page]);

  const nav: Nav = {
    go: setPage,
    useModel: (apiId) => {
      setChatModel(apiId);
      setPage("chat");
    },
    refreshOverview,
  };

  const s = overview?.server;
  return (
    <div className="app">
      <aside className="sidebar">
        <div className="brand">
          <div className="logo">◆</div>
          Personal Harness
        </div>
        {NAV.map((n) => (
          <button key={n.id} className={`nav-item ${page === n.id ? "active" : ""}`} onClick={() => setPage(n.id)}>
            <n.icon />
            {n.label}
          </button>
        ))}
        <div className="spacer" />
        <div className="status-card">
          <div>
            <span className={`dot ${s?.running ? "on" : s?.enabled ? "off" : ""}`} />
            {s?.running ? `API on :${s.port}` : s?.enabled ? "API not running" : "API off"}
          </div>
          <div>
            {overview?.models_available ?? 0} models · {overview?.providers_configured ?? 0} providers
          </div>
          <div>🔒 Data stays on {DEVICE}</div>
        </div>
      </aside>
      <main className="main">
        {page === "chat" && <ChatPage nav={nav} overview={overview} forcedModel={chatModel} onForcedModelUsed={() => setChatModel(null)} />}
        {page === "rankings" && <RankingsPage nav={nav} />}
        {page === "models" && <ModelsPage nav={nav} />}
        {page === "providers" && <ProvidersPage nav={nav} />}
        {page === "api" && <ApiPage nav={nav} />}
        {page === "settings" && <SettingsPage nav={nav} />}
      </main>
    </div>
  );
}
