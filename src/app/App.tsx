import { useCallback, useMemo, useState } from "react";

import { BookOpen, Home, Mic, Settings, SlidersHorizontal, Clock3 } from "../components/Icons";
import { ToastRegion, type ToastKind, type ToastMessage } from "../components/Toast";
import { TodayPage } from "../features/today/TodayPage";
import { HistoryPage } from "../features/history/HistoryPage";
import { VocabularyPage } from "../features/vocabulary/VocabularyPage";
import { ModesPage } from "../features/modes/ModesPage";
import { SettingsPage } from "../features/settings/SettingsPage";
import { getAdapter, type AppAdapter } from "../lib/tauri";
import { useAppModel } from "./useAppModel";

type PageId = "today" | "history" | "vocabulary" | "modes" | "settings";

const navigation = [
  ["today", "Dzisiaj", Home],
  ["history", "Historia", Clock3],
  ["vocabulary", "Słownik", BookOpen],
  ["modes", "Tryby", SlidersHorizontal],
  ["settings", "Ustawienia", Settings],
] as const;

export function App({ adapter: adapterProp }: { adapter?: AppAdapter }) {
  const adapter = useMemo(() => adapterProp ?? getAdapter(), [adapterProp]);
  const [page, setPage] = useState<PageId>("today");
  const [toasts, setToasts] = useState<ToastMessage[]>([]);
  const toast = useCallback((text: string, kind: ToastKind = "error") => {
    const id = Date.now() + Math.random();
    setToasts((items) => [...items, { id, text, kind }].slice(-3));
    window.setTimeout(() => setToasts((items) => items.filter((item) => item.id !== id)), 5000);
  }, []);
  const { snapshot, setSnapshot, history, refreshHistory, loading } = useAppModel(adapter, toast);

  let content;
  if (loading) {
    content = <div className="page page-skeleton" aria-label="Wczytywanie"><span /><span /><span /></div>;
  } else if (page === "today") {
    content = <TodayPage adapter={adapter} snapshot={snapshot} recordings={history} onSnapshot={setSnapshot} onHistory={() => setPage("history")} onToast={toast} />;
  } else if (page === "history") {
    content = <HistoryPage adapter={adapter} recordings={history} onRefresh={refreshHistory} onToast={toast} />;
  } else if (page === "vocabulary") {
    content = <VocabularyPage adapter={adapter} onToast={toast} />;
  } else if (page === "modes") {
    content = <ModesPage adapter={adapter} settings={snapshot.settings} onSettingsChange={(settings) => setSnapshot((current) => ({ ...current, settings }))} onToast={toast} />;
  } else {
    content = <SettingsPage adapter={adapter} initialSettings={snapshot.settings} onSettingsChange={(settings) => setSnapshot((current) => ({ ...current, settings }))} onToast={toast} />;
  }

  return (
    <main className="app-shell">
      <header className="app-topbar">
        <span className="wordmark-icon"><Mic size={17} /></span>
        <strong>Mów</strong>
        <span className="topbar-context">{navigation.find(([id]) => id === page)?.[1]}</span>
      </header>
      <aside className="sidebar">
        <nav aria-label="Główna nawigacja">
          {navigation.map(([id, label, Icon]) => (
            <button key={id} className={page === id ? "nav-item nav-item--active" : "nav-item"} aria-current={page === id ? "page" : undefined} onClick={() => setPage(id)}>
              <Icon size={18} strokeWidth={1.8} /><span>{label}</span>
            </button>
          ))}
        </nav>
        <footer className="sidebar-status">
          <span className="status-dot" />
          <div><strong>Gotowy</strong><small>Lokalnie</small></div>
        </footer>
      </aside>
      <div className="app-content">{content}</div>
      <ToastRegion items={toasts} onDismiss={(id) => setToasts((items) => items.filter((item) => item.id !== id))} />
    </main>
  );
}
