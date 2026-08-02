import { useCallback, useEffect, useMemo, useState } from "react";
import { getCurrentWindow } from "@tauri-apps/api/window";

import { Home, Minus, Settings, X, Clock3 } from "../components/Icons";
import { Logo } from "../components/Logo";
import { ToastRegion, type ToastKind, type ToastMessage } from "../components/Toast";
import { TodayPage } from "../features/today/TodayPage";
import { HistoryPage } from "../features/history/HistoryPage";
import { SettingsPage } from "../features/settings/SettingsPage";
import { getAdapter, type AppAdapter } from "../lib/tauri";
import { useI18n, type TranslationKey } from "../lib/i18n";
import { applyTheme } from "./theme";
import { useAppModel } from "./useAppModel";

type PageId = "today" | "history" | "settings";

const navigation = [
  ["today", Home],
  ["history", Clock3],
  ["settings", Settings],
] as const;

const navLabels: Record<PageId, TranslationKey> = {
  today: "nav.today",
  history: "nav.history",
  settings: "nav.settings",
};

function isTauriWindow(): boolean {
  return "__TAURI_INTERNALS__" in window;
}

export function App({ adapter: adapterProp }: { adapter?: AppAdapter }) {
  const adapter = useMemo(() => adapterProp ?? getAdapter(), [adapterProp]);
  const { t, applyPreference } = useI18n();
  const [page, setPage] = useState<PageId>("today");
  const [toasts, setToasts] = useState<ToastMessage[]>([]);
  const toast = useCallback((text: string, kind: ToastKind = "error") => {
    const id = Date.now() + Math.random();
    setToasts((items) => [...items, { id, text, kind }].slice(-3));
    window.setTimeout(() => setToasts((items) => items.filter((item) => item.id !== id)), 5000);
  }, []);
  const { snapshot, setSnapshot, history, refreshHistory, loading } = useAppModel(adapter, toast);
  useEffect(() => {
    applyTheme(snapshot.settings.theme);
  }, [snapshot.settings.theme]);
  useEffect(() => {
    applyPreference(snapshot.settings.language);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [snapshot.settings.language]);
  const minimize = useCallback(() => {
    if (isTauriWindow()) void getCurrentWindow().minimize();
  }, []);
  const close = useCallback(() => {
    if (isTauriWindow()) void getCurrentWindow().close();
  }, []);

  let content;
  if (loading) {
    content = <div className="page page-skeleton" aria-label={t("app.loading")}><span /><span /><span /></div>;
  } else if (page === "today") {
    content = <TodayPage adapter={adapter} snapshot={snapshot} recordings={history} onSnapshot={setSnapshot} onHistory={() => setPage("history")} onSettings={() => setPage("settings")} onToast={toast} />;
  } else if (page === "history") {
    content = <HistoryPage adapter={adapter} recordings={history} onRefresh={refreshHistory} onToast={toast} />;
  } else {
    content = <SettingsPage adapter={adapter} initialSettings={snapshot.settings} onSettingsChange={(settings) => setSnapshot((current) => ({ ...current, settings }))} onToast={toast} />;
  }

  return (
    <main className="app-shell">
      <aside className="rail" aria-label={t("nav.rail")} data-tauri-drag-region>
        <span className="rail-logo" title="Loquara"><Logo /></span>
        <nav className="rail-nav">
          {navigation.map(([id, Icon]) => {
            const label = t(navLabels[id]);
            return (
              <button
                key={id}
                className={page === id ? "rail-btn rail-btn--active" : "rail-btn"}
                aria-current={page === id ? "page" : undefined}
                aria-label={label}
                title={label}
                onClick={() => setPage(id)}
              >
                <Icon size={20} strokeWidth={1.8} />
                <span className="rail-btn__label">{label}</span>
              </button>
            );
          })}
        </nav>
        <div className="rail-window-controls">
          <button className="rail-btn rail-btn--control" aria-label={t("win.minimize")} title={t("win.minimize")} onClick={minimize}><Minus size={16} /></button>
          <button className="rail-btn rail-btn--control rail-btn--close" aria-label={t("win.close")} title={t("win.close")} onClick={close}><X size={16} /></button>
        </div>
      </aside>
      <div className="app-content">{content}</div>
      <ToastRegion items={toasts} onDismiss={(id) => setToasts((items) => items.filter((item) => item.id !== id))} />
    </main>
  );
}
