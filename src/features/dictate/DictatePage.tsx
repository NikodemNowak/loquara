import { Fragment, useCallback, useEffect, useRef, useState } from "react";

import { Copy, Mic, Square, X, Check, ArrowRight, RotateCcw } from "../../components/Icons";
import { LevelMeter } from "../../components/LevelMeter";
import { AudioPlayer } from "../../components/AudioPlayer";
import type { ToastKind } from "../../components/Toast";
import type { AppAdapter } from "../../lib/tauri";
import type { AppSnapshot, DownloadStatus, Recording } from "../../lib/types";
import { matchingLiveTranscript } from "../../lib/types";
import { useTranscriptFollow } from "../../lib/useTranscriptFollow";
import { normalizeError } from "../../lib/errors";
import { dateLocale, useI18n, type TranslationKey } from "../../lib/i18n";
import { formatBytes } from "../../lib/bytes";

/**
 * Fetches the model and reports how it is going.
 *
 * The rate is measured over the last few seconds rather than the whole
 * transfer, so it settles quickly at the start and reacts when the line
 * changes — an average since the first byte would read as far too slow for
 * the first half of a download that is actually running fine.
 */
const RATE_WINDOW_MS = 4000;

function useModelDownload(adapter: AppAdapter, model: string | undefined, active: DownloadStatus | null | undefined) {
  const mine = active && active.model === model ? active : undefined;
  // Between the click and the app's first snapshot there is nothing to show
  // yet, and a button that stays pressable in that gap invites a second one.
  const [starting, setStarting] = useState(false);
  const [live, setLive] = useState<{ done: number; total: number }>();
  const [error, setError] = useState("");
  const [rate, setRate] = useState(0);
  // Oldest sample still inside the window, kept out of state: it changes on
  // every progress message and nothing renders from it directly.
  const mark = useRef<{ at: number; bytes: number }>({ at: 0, bytes: 0 });
  const running = Boolean(mine) || starting;

  useEffect(() => {
    if (!mine) {
      setLive(undefined);
      setRate(0);
      mark.current = { at: 0, bytes: 0 };
    } else {
      setStarting(false);
    }
  }, [mine]);

  useEffect(() => {
    let active = true;
    let unlisten: (() => void) | undefined;
    void adapter.onModelProgress((progress) => {
      if (!active || progress.model !== model) return;
      setLive({ done: progress.downloadedBytes, total: progress.totalBytes ?? 0 });
      const now = Date.now();
      const since = now - mark.current.at;
      if (!mark.current.at || progress.downloadedBytes < mark.current.bytes) {
        mark.current = { at: now, bytes: progress.downloadedBytes };
      } else if (since >= RATE_WINDOW_MS) {
        setRate(Math.round(((progress.downloadedBytes - mark.current.bytes) * 1000) / since));
        mark.current = { at: now, bytes: progress.downloadedBytes };
      }
    }).then((dispose) => {
      if (active) unlisten = dispose;
      else dispose();
    });
    return () => { active = false; unlisten?.(); };
  }, [adapter, model]);

  const start = useCallback(async () => {
    if (!model) return;
    setError("");
    setStarting(true);
    mark.current = { at: Date.now(), bytes: 0 };
    try {
      await adapter.downloadModel(model);
    } catch (failure) {
      setError(normalizeError(failure));
    } finally {
      setStarting(false);
    }
  }, [adapter, model]);

  const cancel = useCallback(async () => {
    try {
      await adapter.cancelDownload();
    } catch {
      // Nothing to say: the transfer either stops or finishes on its own.
    }
  }, [adapter]);

  const done = live?.done ?? mine?.downloadedBytes ?? 0;
  const total = live?.total || mine?.totalBytes || 0;
  const remaining = total - done;
  return {
    running,
    done,
    total,
    rate,
    error,
    fraction: total ? Math.min(1, done / total) : 0,
    eta: rate > 0 && remaining > 0 ? Math.round(remaining / rate) : 0,
    start,
    cancel,
  };
}

function formatClock(totalSeconds: number) {
  const minutes = String(Math.floor(totalSeconds / 60)).padStart(2, "0");
  const seconds = String(totalSeconds % 60).padStart(2, "0");
  return `${minutes}:${seconds}`;
}

function formatDuration(ms: number) {
  const total = Math.round(ms / 1000);
  return `${String(Math.floor(total / 60)).padStart(2, "0")}:${String(total % 60).padStart(2, "0")}`;
}

function formatTime(timestamp: number, lang: "pl" | "en") {
  return new Intl.DateTimeFormat(dateLocale(lang), { hour: "2-digit", minute: "2-digit" }).format(timestamp);
}

/**
 * The shortcut, drawn as keys.
 *
 * This is the app's primary control, so on the dictation screen it is set at
 * reading size rather than as a footnote.
 */
export function ShortcutKeys({
  shortcut,
  size = "inline",
}: {
  shortcut: string;
  size?: "inline" | "large";
}) {
  const { t } = useI18n();
  const keys = shortcut.split("+").map((key) => key.trim()).filter(Boolean);
  return (
    <span
      className={`shortcut shortcut--${size}`}
      aria-label={t("dictate.shortcut.label", { shortcut: keys.join(" + ") })}
    >
      {keys.map((key, index) => (
        <Fragment key={`${key}-${index}`}>
          {index > 0 && <b aria-hidden="true">+</b>}
          <kbd>{key}</kbd>
        </Fragment>
      ))}
    </span>
  );
}

/** What the app is doing, and what the user can do about it. */
interface Readout {
  state: TranslationKey;
  hint: TranslationKey;
  /** Label for the on-screen fallback control, when one makes sense. */
  action?: TranslationKey;
}

const READOUTS: Record<string, Readout> = {
  idle: {
    state: "dictate.state.ready",
    hint: "dictate.hint.ready",
    action: "dictate.action.start",
  },
  recording: {
    state: "dictate.state.recording",
    hint: "dictate.hint.recording",
    action: "dictate.action.stop",
  },
  cancelling: { state: "dictate.state.cancelling", hint: "dictate.hint.cancelling", action: "overlay.undo" },
  processing: { state: "dictate.state.processing", hint: "dictate.hint.processing" },
  pasting: { state: "dictate.state.pasting", hint: "dictate.hint.pasting" },
  failed: {
    state: "dictate.state.failed",
    hint: "dictate.hint.failed",
    action: "dictate.action.retry",
  },
};

export function DictatePage({
  adapter,
  snapshot,
  recordings,
  modelReady = true,
  onSnapshot,
  onHistory,
  onSettings,
  onToast,
}: {
  adapter: AppAdapter;
  snapshot: AppSnapshot;
  recordings: Recording[];
  /** False until the selected model exists on this machine. */
  modelReady?: boolean;
  onSnapshot: (snapshot: AppSnapshot) => void;
  onHistory: (selectedId?: string) => void;
  onSettings?: () => void;
  onToast: (message: string, kind: ToastKind) => void;
}) {
  const { t, lang } = useI18n();
  const [busy, setBusy] = useState(false);
  const [seconds, setSeconds] = useState(0);
  const [expandedAudio, setExpandedAudio] = useState<string[]>([]);
  const micLevel = useRef(0);
  const state = snapshot.dictation;
  const status = state.status;
  const holdShortcut = snapshot.settings.activationMode === "push_to_talk";
  const startedAt = snapshot.recordingStartedAt ?? null;
  // With no engine there is nothing to be ready for, and saying "Ready" would
  // send the user to press a shortcut that can only fail.
  const cloudMissing = status === "idle" && !modelReady && snapshot.model?.source === "cloud";
  const readout = status === "idle" && !modelReady
    ? cloudMissing
      ? { state: "dictate.state.noKey", hint: "dictate.hint.noKey", action: "dictate.action.openSettings" } as Readout
      : { state: "dictate.state.noModel", hint: "dictate.hint.noModel", action: "dictate.action.getModel" } as Readout
    : READOUTS[status] ?? READOUTS.idle;
  const loadingModel = status === "processing" && snapshot.modelLoading;
  const model = snapshot.model;
  const setup = status === "idle" && !modelReady && !cloudMissing;
  const download = useModelDownload(adapter, model?.key, snapshot.download);

  useEffect(() => {
    if (status !== "recording" && status !== "cancelling") {
      setSeconds(0);
      return;
    }
    const started = startedAt ?? Date.now();
    const tick = () => setSeconds(Math.floor((Date.now() - started) / 1000));
    tick();
    const timer = window.setInterval(tick, 250);
    return () => window.clearInterval(timer);
  }, [status, startedAt]);

  // Levels only matter while this page is showing a live meter. Subscribing
  // the rest of the time re-rendered the recent list ~25 times a second and
  // kept a second copy of the overlay's work.
  useEffect(() => {
    if (status !== "recording") {
      micLevel.current = 0;
      return;
    }
    let active = true;
    let dispose: (() => void) | undefined;
    void adapter.onLevel((next) => {
      if (active) micLevel.current = Math.max(0, Math.min(1, next));
    }).then((unlisten) => { if (active) dispose = unlisten; else unlisten(); });
    return () => { active = false; dispose?.(); };
  }, [adapter, status]);

  const live = matchingLiveTranscript(snapshot);
  const { transcriptRef, onTranscriptScroll } = useTranscriptFollow(live?.recordingId ?? null, live?.revision);
  const recent = recordings.filter((item) => item.status === "completed" && item.text?.trim()).slice(0, 2);
  const activeTake = ["recording", "cancelling", "processing", "pasting"].includes(status);
  const transcript = live?.text || (status === "pasting" ? state.transcript : "");
  const emptyCopy: TranslationKey = status === "processing" || live?.status === "finalizing"
    ? "studio.finalizing"
    : status !== "recording" ? "studio.emptyTitle"
    : live?.status === "fallback" ? "studio.afterStop"
    : live?.status === "connecting" ? "studio.connecting"
    : live ? "studio.waitingLive" : "studio.afterStop";
  const copyTranscript = async (text: string) => {
    try {
      if (!navigator.clipboard?.writeText) throw new Error(t("history.error.clipboard"));
      await navigator.clipboard.writeText(text);
      onToast(t("studio.clipboardReady"), "success");
    } catch (error) {
      onToast(t("common.error.action", { error: normalizeError(error) }), "error");
    }
  };
  const cancel = async () => {
    if (busy) return;
    setBusy(true);
    try { onSnapshot(await adapter.cancelRecording()); }
    catch (error) { onToast(t("common.error.action", { error: normalizeError(error) }), "error"); }
    finally { setBusy(false); }
  };

  const act = async () => {
    if (busy) return;
    setBusy(true);
    try {
      const next = status === "recording"
        ? await adapter.stopRecording()
        : status === "cancelling"
          ? await adapter.retryTranscription(state.recordingId)
          : status === "failed"
            ? await adapter.retryTranscription(state.recovery.recordingId)
            : await adapter.startRecording();
      onSnapshot(next);
    } catch (error) {
      onToast(t("common.error.action", { error: normalizeError(error) }), "error");
    } finally {
      setBusy(false);
    }
  };

  return (
    <section className={`page dictate-page${activeTake || status === "failed" ? " dictate-page--active" : ""}`}>
      <header className="home-welcome">
        <h1 aria-live="polite">{loadingModel ? t("dictate.state.loadingModel") : t(readout.state)}</h1>
        <p>{loadingModel ? t("dictate.hint.loadingModel") : status === "idle" && modelReady ? t(holdShortcut ? "studio.holdReadyHint" : "studio.readyHint") : status === "recording" && holdShortcut ? t("dictate.hint.holdRecording") : t(readout.hint)}</p>
        <div className={`home-action${activeTake || status === "failed" ? " home-action--active" : ""}`}>
          {status === "recording" && <div className="studio-level"><LevelMeter level={0} levelRef={micLevel} height={24} barWidth={2} gap={2} colorToken="--recording" quietToken="--muted" /><time aria-label={t("dictate.elapsed")}>{formatClock(seconds)}</time></div>}
          {["processing", "pasting"].includes(status) && <span className="spinner" aria-hidden="true" />}
          {(status === "recording" || status === "processing") && <button className="text-button studio-cancel" disabled={busy} onClick={() => void cancel()}><X size={14} />{t("studio.cancel")}</button>}
          {readout.action && !setup && <button className={status === "idle" ? "shortcut-action" : "primary-button"} aria-label={busy ? t("dictate.action.working") : status === "cancelling" ? t("overlay.undoAria") : t(readout.action)} disabled={busy} onClick={() => cloudMissing ? onSettings?.() : void act()}>
            <span className="shortcut-action__label">{status === "recording" ? <Square size={16} /> : status === "cancelling" || status === "failed" ? <RotateCcw size={16} /> : <Mic size={17} />}{busy ? t("dictate.action.working") : t(readout.action)}</span>
            {status === "idle" && modelReady && <ShortcutKeys shortcut={snapshot.settings.shortcut} />}
          </button>}
        </div>
        {status === "idle" && modelReady && <span className="home-welcome__note">{t("dictate.shortcut.outside")}</span>}
        {setup && model && (
          <div className="model-setup">
            <p className="model-setup__meta">
              {t("dictate.model.meta", {
                model: model.display,
                size: formatBytes(model.totalBytes),
                provider: model.provider,
              })}
            </p>
            {download.running ? (
              <div className="model-setup__progress" role="status">
                <div className="model-setup__track">
                  <span style={{ width: `${Math.round(download.fraction * 100)}%` }} />
                </div>
                <p className="model-setup__numbers">
                  <strong>{t("dictate.model.progress", {
                    done: formatBytes(download.done),
                    total: formatBytes(download.total || model.totalBytes),
                  })}</strong>
                  <span>
                    {download.rate
                      ? download.eta
                        ? t("dictate.model.rate", { rate: formatBytes(download.rate), eta: formatClock(download.eta) })
                        : t("dictate.model.rateUnknown", { rate: formatBytes(download.rate) })
                      : t("dictate.model.downloading")}
                  </span>
                </p>
                <button className="text-button" onClick={() => void download.cancel()}>
                  {t("dictate.model.cancel")}
                </button>
              </div>
            ) : (
              <div className="dictate__actions">
                <button className="primary-button" onClick={() => void download.start()}>
                  {t(download.error ? "dictate.model.retry" : "dictate.action.getModel")}
                </button>
              </div>
            )}
            {download.error && (
              <p className="model-setup__error" role="alert">
                {t("dictate.model.failed", { error: download.error })}
              </p>
            )}
            <p className="model-setup__note">{t("dictate.model.soon")}</p>
          </div>
        )}
      </header>
      {(activeTake || status === "failed") ? <div className={`active-dictation active-dictation--${status}`}>
        <div className="section-heading">
          <h2>{t("studio.draft")}</h2>
          {live && <span className={`clipboard-state ${live.clipboardReady ? "clipboard-state--ready" : ""}`}>{live.clipboardReady && <Check size={14} />}{t(live.clipboardReady ? "studio.clipboardReady" : "studio.clipboardPending")}</span>}
        </div>
        <div ref={transcriptRef} onScroll={onTranscriptScroll} className="transcript-canvas" role="region" aria-label={t("studio.draft")}>
          {transcript ? <p className="transcript-canvas__text">{transcript}</p> : <div className="transcript-canvas__empty"><p>{t(emptyCopy)}</p></div>}
          {live?.status === "fallback" && <p className="studio-fallback" role="status">{t("studio.fallback")}</p>}
          {status === "failed" && <p className="error-note" role="alert">{normalizeError(state.error)}</p>}
        </div>
      </div> : <section className="home-recent" aria-label={t("studio.last")}>
        <div className="section-heading">
          <h2>{t("dictate.recentHeading")}</h2>
          <button className="text-button" onClick={() => onHistory()}>{t("dictate.viewHistory")}<ArrowRight size={14} /></button>
        </div>
        {recent.length ? recent.map((item) => <article className="home-excerpt" key={item.id}>
          <div className="home-excerpt__meta">
            <time dateTime={new Date(item.createdAt).toISOString()}>{formatTime(item.createdAt, lang)}</time>
            <span>{new Intl.DateTimeFormat(dateLocale(lang), { day: "numeric", month: "short" }).format(item.createdAt)}</span>
            {item.sourceApp && <span>{item.sourceApp}</span>}
            <span>{formatDuration(item.durationMs)}</span>
          </div>
          <button className="home-excerpt__text" onClick={() => onHistory(item.id)} title={t("studio.review")}>{item.text}</button>
          <button className="icon-button home-excerpt__copy" onClick={() => void copyTranscript(item.text!)} aria-label={t("history.action.copy")} title={t("history.action.copy")}><Copy size={16} /></button>
          {item.audioPath && <div className={`home-excerpt__audio${expandedAudio.includes(item.id) ? " home-excerpt__audio--expanded" : ""}`} onClickCapture={(event) => {
            if ((event.target as HTMLElement).closest(".player__btn")) setExpandedAudio((ids) => ids.includes(item.id) ? ids : [...ids, item.id]);
          }}><AudioPlayer adapter={adapter} recording={item} /></div>}
        </article>) : <div className="home-empty"><p>{t("dictate.emptyTitle")}</p><span>{t("dictate.empty")}</span></div>}
      </section>}
        {status === "idle" && snapshot.modelLoading && (
          <p className="dictate__note" role="status">
            <span className="spinner spinner--small" aria-hidden="true" />
            {t("dictate.note.warmingUp")}
          </p>
        )}
    </section>
  );
}
