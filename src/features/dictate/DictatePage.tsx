import { Fragment, useEffect, useState } from "react";

import { LevelMeter } from "../../components/LevelMeter";
import { Waveform } from "../../components/Waveform";
import type { ToastKind } from "../../components/Toast";
import type { AppAdapter } from "../../lib/tauri";
import type { AppSnapshot, Recording } from "../../lib/types";
import { normalizeError } from "../../lib/errors";
import { dateLocale, useI18n, type TranslationKey } from "../../lib/i18n";

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
  /** What the keys row says beneath the shortcut. */
  keys?: TranslationKey;
}

const READOUTS: Record<string, Readout> = {
  idle: {
    state: "dictate.state.ready",
    hint: "dictate.hint.ready",
    action: "dictate.action.start",
    keys: "dictate.keys.start",
  },
  recording: {
    state: "dictate.state.recording",
    hint: "dictate.hint.recording",
    action: "dictate.action.stop",
    keys: "dictate.keys.stop",
  },
  cancelling: { state: "dictate.state.recording", hint: "dictate.hint.cancelling" },
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
  onSnapshot,
  onHistory,
  onSettings,
  onToast,
}: {
  adapter: AppAdapter;
  snapshot: AppSnapshot;
  recordings: Recording[];
  onSnapshot: (snapshot: AppSnapshot) => void;
  onHistory: () => void;
  onSettings?: () => void;
  onToast: (message: string, kind: ToastKind) => void;
}) {
  const { t, lang } = useI18n();
  const [busy, setBusy] = useState(false);
  const [seconds, setSeconds] = useState(0);
  const [level, setLevel] = useState(0);
  const state = snapshot.dictation;
  const status = state.status;
  const startedAt = snapshot.recordingStartedAt ?? null;
  const readout = READOUTS[status] ?? READOUTS.idle;
  const loadingModel = status === "processing" && snapshot.modelLoading;

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

  // The meter is the point of this screen while dictating, so the window
  // listens for levels itself rather than mirroring whatever the pill shows.
  useEffect(() => {
    let active = true;
    let dispose: (() => void) | undefined;
    void adapter.onLevel((next) => {
      if (active) setLevel(Math.max(0, Math.min(1, next)));
    }).then((unlisten) => { if (active) dispose = unlisten; else unlisten(); });
    return () => { active = false; dispose?.(); };
  }, [adapter]);

  const act = async () => {
    if (busy) return;
    setBusy(true);
    try {
      const next = status === "recording"
        ? await adapter.stopRecording()
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
    <section className="page dictate-page">
      <div className="dictate">
        <h1 className={`dictate__state dictate__state--${status}`} aria-live="polite">
          {loadingModel ? t("dictate.state.loadingModel") : t(readout.state)}
        </h1>
        <p className="dictate__hint">
          {loadingModel ? t("dictate.hint.loadingModel") : t(readout.hint)}
        </p>

        {(status === "recording" || status === "cancelling") && (
          <div className="dictate__meter">
            <LevelMeter level={status === "cancelling" ? 0 : level} height={72} barWidth={4} gap={4} colorToken="--recording" />
            <p className="dictate__timer" aria-label={t("dictate.elapsed")}>{formatClock(seconds)}</p>
          </div>
        )}

        {status === "processing" && (
          <div className="dictate__meter dictate__meter--thinking">
            <LevelMeter level={0} mode="thinking" height={72} barWidth={4} gap={4} />
          </div>
        )}

        {readout.keys && (
          <div className="dictate__keys">
            <ShortcutKeys shortcut={snapshot.settings.shortcut} size="large" />
            <span>{t(readout.keys)}</span>
          </div>
        )}

        {status === "idle" && snapshot.modelLoading && (
          <p className="dictate__note" role="status">
            <span className="spinner spinner--small" aria-hidden="true" />
            {t("dictate.note.warmingUp")}
          </p>
        )}

        {readout.action && (
          <div className="dictate__actions">
            <button className="secondary-button" disabled={busy} onClick={() => void act()}>
              {busy ? t("dictate.action.working") : t(readout.action)}
            </button>
          </div>
        )}
      </div>

      <div className="section-heading">
        <h2>{t("dictate.recent.title")}</h2>
        <button className="text-button" onClick={onHistory}>{t("dictate.recent.all")}</button>
      </div>

      {recordings.length ? (
        <div className="recent">
          {recordings.slice(0, 5).map((item) => (
            <button key={item.id} className="recent-item" onClick={onHistory}>
              <span className={`recent-item__text ${item.text ? "" : "recent-item__text--empty"}`}>
                {item.text ?? t("dictate.recent.noTranscript")}
              </span>
              <span className="recent-item__meta">
                <time dateTime={new Date(item.createdAt).toISOString()}>{formatTime(item.createdAt, lang)}</time>
                <span>{formatDuration(item.durationMs)}</span>
                <Waveform peaks={item.peaks} />
                {item.status === "failed" && <b>{t("common.error")}</b>}
              </span>
            </button>
          ))}
        </div>
      ) : (
        <p className="dictate-empty">
          {t("dictate.empty")}
          {onSettings && (
            <button onClick={onSettings}>{t("dictate.empty.settings")}</button>
          )}
        </p>
      )}
    </section>
  );
}
