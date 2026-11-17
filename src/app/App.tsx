import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { getCurrentWindow } from "@tauri-apps/api/window";

import { Minus, Settings, WindowMaximize, X } from "../components/Icons";
import { Logo } from "../components/Logo";
import { StartupScreen } from "../components/StartupScreen";
import { NoticeDetails, NoticeIcon, ToastRegion, type ToastKind, type ToastMessage } from "../components/Toast";
import { DictatePage } from "../features/dictate/DictatePage";
import { HistoryPage } from "../features/history/HistoryPage";
import { SettingsPage } from "../features/settings/SettingsPage";
import { VocabularyPage } from "../features/vocabulary/VocabularyPage";
import { getAdapter, type AppAdapter } from "../lib/tauri";
import { useI18n, type TranslationKey } from "../lib/i18n";
import { useAppModel } from "./useAppModel";
import { errorDetail, normalizeError } from "../lib/errors";

type PageId = "dictate" | "history" | "vocabulary" | "settings";

const navigation = ["dictate", "history", "vocabulary", "settings"] as const;

const navLabels: Record<PageId, TranslationKey> = {
  dictate: "nav.dictate",
  history: "nav.history",
  vocabulary: "vocab.title",
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
  // The delivery banner already explains a failed paste; a toast saying the
  // same thing again is noise. Keyed by the localized text both surfaces show.
  const bannerText = useRef<string | null>(null);
  const toast = useCallback((text: string, kind: ToastKind = "error", detail?: string) => {
    if (kind === "error" && text === bannerText.current) return;
    const id = Date.now() + Math.random();
    setToasts((items) => [...items.filter((item) => item.text !== text), { id, text, kind, detail }].slice(-3));
  }, []);
  const dismissToast = useCallback((id: number) => setToasts((items) => items.filter((item) => item.id !== id)), []);
  const { snapshot, setSnapshot, history, refreshHistory, loading } = useAppModel(adapter, toast);
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
  const deliveryMessage = deliveryNotice?.detail ? normalizeError(deliveryNotice.detail) : null;
  const deliveryDetail = deliveryNotice?.detail ? errorDetail(deliveryNotice.detail) : undefined;
  bannerText.current = deliveryMessage;
  useEffect(() => {
    // The paste error event can land just before the state that raises the
    // banner, so drop a toast that turns out to repeat it.
    if (deliveryMessage) setToasts((items) => items.filter((item) => item.text !== deliveryMessage));
  }, [deliveryMessage]);
  let content;
  if (page === "dictate") {
    content = <DictatePage adapter={adapter} snapshot={snapshot} recordings={history} modelReady={installed === "ready"} onSnapshot={setSnapshot} onHistory={openHistory} onToast={toast} />;
  } else if (page === "history") {
    content = <HistoryPage adapter={adapter} recordings={history} initialSelectedId={historySelectedId} onRefresh={refreshHistory} onToast={toast} />;
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
          {deliveryNotice && <div className="delivery-notice" role="alert">
            <NoticeIcon kind="error" />
            <div className="delivery-notice__body">
              <p className="delivery-notice__text"><strong>{t("overlay.deliveryFailed")}</strong>{deliveryMessage && <span>{deliveryMessage}</span>}</p>
              <NoticeDetails detail={deliveryDetail} />
            </div>
            <button className="icon-button notice-dismiss" aria-label={t("overlay.dismiss")} title={t("overlay.dismiss")} onClick={() => {
              void adapter.dismissOverlayNotice(deliveryNotice.key, deliveryNotice.generation).then((next) => setSnapshot((current) =>
                current.overlayNotice?.key === deliveryNotice.key && current.overlayNotice.generation === deliveryNotice.generation ? next : current,
              )).catch((error) => toast(normalizeError(error), "error", errorDetail(error)));
            }}><X size={14} /></button>
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
        </footer>
      </div>
      </StartupScreen>
      <ToastRegion items={toasts} onDismiss={dismissToast} />
    </main>
  );
}
