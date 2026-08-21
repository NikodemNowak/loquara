import { useCallback, useEffect, useMemo, useState } from "react";
import { getCurrentWindow } from "@tauri-apps/api/window";

import { Home, Minus, Settings, Square, X, Clock3 } from "../components/Icons";
import { Logo } from "../components/Logo";
import { BrandLogo } from "../components/BrandLogo";
import { ToastRegion, type ToastKind, type ToastMessage } from "../components/Toast";
import { DictatePage } from "../features/dictate/DictatePage";
import { HistoryPage } from "../features/history/HistoryPage";
import { SettingsPage } from "../features/settings/SettingsPage";
import { getAdapter, type AppAdapter } from "../lib/tauri";
import type { ModelStatus } from "../lib/types";
import { useI18n, type TranslationKey } from "../lib/i18n";
import { useAppModel } from "./useAppModel";

type PageId = "dictate" | "history" | "settings";

const navigation = [
  ["dictate", Home],
  ["history", Clock3],
  ["settings", Settings],
] as const;

const navLabels: Record<PageId, TranslationKey> = {
  dictate: "nav.dictate",
  history: "nav.history",
  settings: "nav.settings",
};

function isTauriWindow(): boolean {
  return "__TAURI_INTERNALS__" in window;
}

export function App({ adapter: adapterProp }: { adapter?: AppAdapter }) {
  const adapter = useMemo(() => adapterProp ?? getAdapter(), [adapterProp]);
  const { t, applyPreference } = useI18n();
  const [page, setPage] = useState<PageId>("dictate");
  const [toasts, setToasts] = useState<ToastMessage[]>([]);
  const toast = useCallback((text: string, kind: ToastKind = "error") => {
    const id = Date.now() + Math.random();
    setToasts((items) => [...items, { id, text, kind }].slice(-3));
    window.setTimeout(() => setToasts((items) => items.filter((item) => item.id !== id)), 5000);
  }, []);
  const { snapshot, setSnapshot, history, refreshHistory, loading } = useAppModel(adapter, toast);
  const [model, setModel] = useState<{ display: string; provider: string }>();
  /** Whether the selected model is actually present on this machine. */
  const [installed, setInstalled] = useState<ModelStatus["state"] | "checking">("checking");
  const [reload, setReload] = useState(0);
  useEffect(() => {
    let active = true;
    void adapter.listModels()
      .then((models) => {
        if (!active) return;
        const current = models.find((item) => item.key === snapshot.settings.model);
        setModel(current
          ? { display: current.display, provider: current.provider }
          : { display: snapshot.settings.model, provider: snapshot.settings.model });
      })
      .catch(() => {
        if (active) setModel({ display: snapshot.settings.model, provider: snapshot.settings.model });
      });
    // Readiness is a fact about the disk, not something that can be inferred
    // from the app being idle: claiming "ready" with no model installed is
    // exactly the state a first run is in.
    void adapter.getModelStatus()
      .then((status) => { if (active) setInstalled(status.state); })
      .catch(() => { if (active) setInstalled("error"); });
    return () => { active = false; };
  }, [adapter, snapshot.settings.model, reload]);

  const engine: { state: string; label: TranslationKey } =
    snapshot.dictation.status === "recording" || snapshot.dictation.status === "cancelling"
      ? { state: "recording", label: "engine.recording" }
      : snapshot.modelLoading
        ? { state: "loading", label: "engine.loading" }
        : installed === "checking"
          ? { state: "checking", label: "engine.checking" }
          : installed === "ready"
            ? { state: "ready", label: "engine.ready" }
            : installed === "error"
              ? { state: "error", label: "engine.error" }
              : { state: "missing", label: "engine.missing" };
  useEffect(() => {
    // Loquara is dark-only; tell the OS so the window frame and native
    // scrollbars match rather than flashing a light chrome on launch.
    if (isTauriWindow()) void getCurrentWindow().setTheme("dark");
  }, []);
  useEffect(() => {
    applyPreference(snapshot.settings.language);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [snapshot.settings.language]);
  const minimize = useCallback(() => {
    if (isTauriWindow()) void getCurrentWindow().minimize();
  }, []);
  const toggleMaximize = useCallback(() => {
    if (isTauriWindow()) void getCurrentWindow().toggleMaximize();
  }, []);
  const close = useCallback(() => {
    if (isTauriWindow()) void getCurrentWindow().close();
  }, []);

  let content;
  if (loading) {
    content = <div className="page page-skeleton" aria-label={t("app.loading")}><span /><span /><span /></div>;
  } else if (page === "dictate") {
    content = <DictatePage adapter={adapter} snapshot={snapshot} recordings={history} modelReady={installed === "ready"} onSnapshot={setSnapshot} onHistory={() => setPage("history")} onSettings={() => setPage("settings")} onToast={toast} />;
  } else if (page === "history") {
    content = <HistoryPage adapter={adapter} recordings={history} onRefresh={refreshHistory} onToast={toast} />;
  } else {
    content = <SettingsPage adapter={adapter} initialSettings={snapshot.settings} onSettingsChange={(settings) => setSnapshot((current) => ({ ...current, settings }))} onModelsChanged={() => setReload((value) => value + 1)} onToast={toast} />;
  }

  return (
    <main className="app-shell">
      <header className="titlebar" data-tauri-drag-region>
        <span className="titlebar__brand"><Logo size={16} /><span>Loquara</span></span>
        <div className="titlebar__drag" data-tauri-drag-region />
        <div className="titlebar__controls">
          <button className="win-btn" aria-label={t("win.minimize")} title={t("win.minimize")} onClick={minimize}><Minus size={14} /></button>
          <button className="win-btn" aria-label={t("win.maximize")} title={t("win.maximize")} onClick={toggleMaximize}><Square size={12} /></button>
          <button className="win-btn win-btn--close" aria-label={t("win.close")} title={t("win.close")} onClick={close}><X size={14} /></button>
        </div>
      </header>
      <div className="app-body">
        <aside className="rail" aria-label={t("nav.rail")}>
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
                  <Icon size={18} strokeWidth={1.8} />
                  <span className="rail-btn__label">{label}</span>
                </button>
              );
            })}
          </nav>
          <button
            className={`engine-chip engine-chip--${engine.state}`}
            onClick={() => setPage("settings")}
            aria-label={t("engine.openSettings", { model: model?.display ?? "" })}
            title={t("engine.openSettings", { model: model?.display ?? "" })}
          >
            <span className="engine-chip__mark">
              {model ? <BrandLogo provider={model.provider} /> : null}
            </span>
            <span className="engine-chip__copy">
              <strong>{model?.display ?? ""}</strong>
              <span className="engine-chip__state"><i aria-hidden="true" />{t(engine.label)}</span>
            </span>
          </button>
        </aside>
        <div className="app-content"><div className="app-scroll">{content}</div></div>
      </div>
      <ToastRegion items={toasts} onDismiss={(id) => setToasts((items) => items.filter((item) => item.id !== id))} />
    </main>
  );
}
