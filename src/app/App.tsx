import { useCallback, useEffect, useMemo, useState } from "react";
import { getCurrentWindow } from "@tauri-apps/api/window";

import { Minus, Settings, WindowMaximize, X, ArrowRight } from "../components/Icons";
import { Logo } from "../components/Logo";
import { StartupScreen } from "../components/StartupScreen";
import { ToastRegion, type ToastKind, type ToastMessage } from "../components/Toast";
import { DictatePage } from "../features/dictate/DictatePage";
import { HistoryPage } from "../features/history/HistoryPage";
import { SettingsPage } from "../features/settings/SettingsPage";
import { ModesPage } from "../features/modes/ModesPage";
import { VocabularyPage } from "../features/vocabulary/VocabularyPage";
import { MeetingBar, MeetingStartButton } from "../features/meeting/MeetingBar";
import { useMeeting } from "../features/meeting/useMeeting";
import { getAdapter, type AppAdapter } from "../lib/tauri";
import { useI18n, type TranslationKey } from "../lib/i18n";
import { useAppModel } from "./useAppModel";
import { normalizeError } from "../lib/errors";

type PageId = "dictate" | "history" | "modes" | "vocabulary" | "settings";

const navigation = ["dictate", "history", "modes", "vocabulary", "settings"] as const;

const navLabels: Record<PageId, TranslationKey> = {
  dictate: "nav.dictate",
  history: "nav.history",
  vocabulary: "vocab.title",
  modes: "modes.title",
  settings: "nav.settings",
};

function isTauriWindow(): boolean {
  return "__TAURI_INTERNALS__" in window;
}

export function App({ adapter: adapterProp }: { adapter?: AppAdapter }) {
  const adapter = useMemo(() => adapterProp ?? getAdapter(), [adapterProp]);
  const { t, applyPreference } = useI18n();
  const [page, setPage] = useState<PageId>("dictate");
  const [historySelectedId, setHistorySelectedId] = useState<string>();
  const openHistory = useCallback((selectedId?: string) => {
    setHistorySelectedId(selectedId);
    setPage("history");
  }, []);
  const [toasts, setToasts] = useState<ToastMessage[]>([]);
  const toast = useCallback((text: string, kind: ToastKind = "error") => {
    const id = Date.now() + Math.random();
    setToasts((items) => [...items, { id, text, kind }].slice(-3));
    window.setTimeout(() => setToasts((items) => items.filter((item) => item.id !== id)), 5000);
  }, []);
  const { snapshot, setSnapshot, history, refreshHistory, loading } = useAppModel(adapter, toast);
  const onMeetingFinished = useCallback(() => {
    void refreshHistory();
  }, [refreshHistory]);
  const meeting = useMeeting(adapter, toast, onMeetingFinished);
  // A meeting adds its row when it starts and fills it in when it is
  // transcribed; history follows both.
  useEffect(() => {
    void refreshHistory();
  }, [meeting.status.phase, refreshHistory]);
  const startMeeting = meeting.start;
  useEffect(() => {
    let active = true;
    let unlisten: (() => void) | undefined;
    // The tray could not start the meeting (first use, or a problem it
    // cannot show); trying again here explains why.
    void adapter.onMeetingOpen(() => startMeeting()).then((dispose) => {
      if (active) unlisten = dispose;
      else dispose();
    });
    return () => { active = false; unlisten?.(); };
  }, [adapter, startMeeting]);
  const [activeModeName, setActiveModeName] = useState("");
  useEffect(() => {
    let active = true;
    void adapter.listModes().then((items) => {
      if (active) setActiveModeName(items.find((item) => item.id === snapshot.settings.activeMode)?.name ?? "");
    }).catch(() => { if (active) setActiveModeName(""); });
    return () => { active = false; };
  }, [adapter, page, snapshot.settings.activeMode]);
  // Name, maker and readiness all ride on the snapshot, which is broadcast on
  // every state change. Asking for them separately meant a query that missed
  // once — before the window was ready — left the panel insisting there was
  // no model while one was loading.
  const model = snapshot.model;
  const installed: "ready" | "missing" | "checking" = model
    ? model.installed
      ? "ready"
      : "missing"
    : "checking";


  const transfer = snapshot.download;
  // The snapshot says whether a transfer exists; the progress events say how
  // far along it is. Without the second, the chip would sit at 0% until the
  // download finished.
  const [transferLive, setTransferLive] = useState<{ model: string; done: number; total: number }>();
  useEffect(() => {
    let active = true;
    let unlisten: (() => void) | undefined;
    void adapter.onModelProgress((progress) => {
      if (!active) return;
      setTransferLive({
        model: progress.model,
        done: progress.downloadedBytes,
        total: progress.totalBytes ?? 0,
      });
    }).then((dispose) => { if (active) unlisten = dispose; else dispose(); });
    return () => { active = false; unlisten?.(); };
  }, [adapter]);
  const live = transfer && transferLive?.model === transfer.model ? transferLive : undefined;
  const transferDone = live?.done ?? transfer?.downloadedBytes ?? 0;
  const transferTotal = live?.total || transfer?.totalBytes || 0;
  const transferPercent = transferTotal
    ? Math.min(100, Math.round((transferDone / transferTotal) * 100))
    : 0;
  const dictationLabels: Partial<Record<typeof snapshot.dictation.status, TranslationKey>> = {
    recording: "engine.recording",
    cancelling: "engine.cancelling",
    processing: "engine.processing",
    pasting: "engine.pasting",
    failed: "engine.failed",
  };
  const dictationLabel = dictationLabels[snapshot.dictation.status];
  const engine: { state: string; label: TranslationKey } =
    dictationLabel
      ? { state: snapshot.dictation.status, label: dictationLabel }
      : transfer
        ? { state: "downloading", label: "engine.downloading" }
      : snapshot.modelLoading
        ? { state: "loading", label: "engine.loading" }
        : installed === "checking"
          ? { state: "checking", label: "engine.checking" }
          : installed === "ready"
            ? { state: "ready", label: "engine.ready" }
            : model?.source === "cloud"
              ? { state: "missing", label: "engine.noKey" }
              : { state: "missing", label: "engine.missing" };
  useEffect(() => {
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

  const deliveryNotice = snapshot.overlayNotice?.kind === "deliveryFailed" ? snapshot.overlayNotice : null;
  let content;
  if (page === "dictate") {
    content = <DictatePage adapter={adapter} snapshot={snapshot} recordings={history} modelReady={installed === "ready"} onSnapshot={setSnapshot} onHistory={openHistory} onSettings={() => setPage("settings")} onToast={toast} />;
  } else if (page === "history") {
    content = <HistoryPage adapter={adapter} recordings={history} initialSelectedId={historySelectedId} onRefresh={refreshHistory} onToast={toast} onRetryMeeting={meeting.retry} />;
  } else if (page === "modes") {
    content = <ModesPage adapter={adapter} settings={snapshot.settings} onSettingsChange={(settings) => setSnapshot((current) => ({ ...current, settings }))} onToast={toast} />;
  } else if (page === "vocabulary") {
    content = <VocabularyPage adapter={adapter} onToast={toast} />;
  } else {
    content = <SettingsPage adapter={adapter} initialSettings={snapshot.settings} download={snapshot.download} onSettingsChange={(settings) => setSnapshot((current) => ({ ...current, settings }))} onModelsChanged={() => { void adapter.getAppSnapshot().then(setSnapshot).catch(() => {}); }} onToast={toast} />;
  }

  return (
    <main className="app-shell">
      <header className="titlebar" data-tauri-drag-region>
        <span className="titlebar__brand"><Logo size={16} /><span>Loquara</span></span>
        <div className="titlebar__drag" data-tauri-drag-region />
        <div className="titlebar__controls">
          <button className="win-btn" aria-label={t("win.minimize")} title={t("win.minimize")} onClick={minimize}><Minus size={14} /></button>
          <button className="win-btn" aria-label={t("win.maximize")} title={t("win.maximize")} onClick={toggleMaximize}><WindowMaximize size={12} /></button>
          <button className="win-btn win-btn--close" aria-label={t("win.close")} title={t("win.close")} onClick={close}><X size={14} /></button>
        </div>
      </header>
      <StartupScreen loading={loading}>
      <div className="app-body">
          <nav className="top-nav" aria-label={t("nav.rail")}>
            {navigation.map((id) => {
              const label = t(navLabels[id]);
              return (
                <button
                  key={id}
                  className={`top-nav__button${page === id ? " top-nav__button--active" : ""}${id === "settings" ? " top-nav__settings" : ""}`}
                  aria-current={page === id ? "page" : undefined}
                  aria-label={label}
                  title={label}
                  onClick={() => {
                    if (id === "history") setHistorySelectedId(undefined);
                    setPage(id);
                  }}
                >
                  {id === "settings" && <Settings size={17} strokeWidth={1.7} />}<span>{label}</span>
                </button>
              );
            })}
          </nav>
        <div className="app-content">
          <MeetingBar meeting={meeting} />
          {deliveryNotice && <div className="delivery-notice" role="alert">
            <div><strong>{t("overlay.deliveryFailed")}</strong>{deliveryNotice.detail && <p>{deliveryNotice.detail}</p>}</div>
            <button className="icon-button" aria-label={t("overlay.dismiss")} title={t("overlay.dismiss")} onClick={() => {
              void adapter.dismissOverlayNotice(deliveryNotice.key, deliveryNotice.generation).then((next) => setSnapshot((current) =>
                current.overlayNotice?.key === deliveryNotice.key && current.overlayNotice.generation === deliveryNotice.generation ? next : current,
              )).catch((error) => toast(normalizeError(error), "error"));
            }}><X size={16} /></button>
          </div>}
          <div className="app-scroll">{content}</div>
        </div>
        <footer className="app-statusbar">
          <button
            className={`engine-status engine-status--${engine.state}`}
            onClick={() => setPage("settings")}
            aria-label={`${t("engine.openSettings", { model: model?.display ?? "" })} · ${engine.state === "downloading" ? t("engine.downloading", { percent: transferPercent }) : t(engine.label)}`}
            title={`${t("engine.openSettings", { model: model?.display ?? "" })} · ${engine.state === "downloading" ? t("engine.downloading", { percent: transferPercent }) : t(engine.label)}`}
          >
            <i aria-hidden="true" />
            <span className="engine-status__model">{model?.display ?? t("engine.checking")}</span>
              {(page !== "dictate" || ["downloading", "checking", "loading"].includes(engine.state)) && <span className="engine-status__state">
                {engine.state === "downloading" ? t("engine.downloading", { percent: transferPercent }) : t(engine.label)}
              </span>}
          </button>
          <MeetingStartButton meeting={meeting} />
          <button className="mode-shortcut" onClick={() => setPage("modes")} aria-label={t("modes.change")}>
            {activeModeName ? t("dictate.modeLabel", { mode: activeModeName }) : t("modes.title")}<ArrowRight size={13} />
          </button>
        </footer>
      </div>
      </StartupScreen>
      <ToastRegion items={toasts} onDismiss={(id) => setToasts((items) => items.filter((item) => item.id !== id))} />
    </main>
  );
}
