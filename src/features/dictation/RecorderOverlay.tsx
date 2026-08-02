import { useEffect, useRef, useState } from "react";

import { Check, RotateCcw } from "../../components/Icons";
import type { AppAdapter } from "../../lib/tauri";
import type { AppSnapshot } from "../../lib/types";
import { normalizeError } from "../../lib/errors";
import { useI18n } from "../../lib/i18n";

function timeLabel(seconds: number) {
  return `${String(Math.floor(seconds / 60)).padStart(2, "0")}:${String(seconds % 60).padStart(2, "0")}`;
}

const WAVE_BARS = 28;
const WAVE_HEIGHT = 30;

function LiveWaveform({ level, mode = "live" }: { level: number; mode?: "live" | "thinking" }) {
  const { t } = useI18n();
  const canvasRef = useRef<HTMLCanvasElement>(null);
  const levelRef = useRef(level);
  levelRef.current = level;

  useEffect(() => {
    const canvas = canvasRef.current;
    if (!canvas) return;
    const context = canvas.getContext("2d");
    if (!context) return;
    const cssWidth = canvas.clientWidth || 180;
    const dpr = window.devicePixelRatio || 1;
    canvas.width = cssWidth * dpr;
    canvas.height = WAVE_HEIGHT * dpr;
    context.scale(dpr, dpr);
    const gap = 3;
    const barWidth = Math.max(3, (cssWidth - gap * (WAVE_BARS - 1)) / WAVE_BARS);
    const envelopes = new Array<number>(WAVE_BARS).fill(0);
    const phases = Array.from({ length: WAVE_BARS }, (_, index) => index * 1.37);
    const reduceMotion = window.matchMedia("(prefers-reduced-motion: reduce)").matches;
    let raf = 0;

    const draw = (now: number) => {
      const time = reduceMotion ? 0 : now / 1000;
      const target = Math.max(0, Math.min(1, levelRef.current));
      context.clearRect(0, 0, cssWidth, WAVE_HEIGHT);
      context.fillStyle = "#eef2f8";
      for (let index = 0; index < WAVE_BARS; index += 1) {
        let energy: number;
        if (mode === "thinking") {
          const center = (Math.sin(time * 1.6) * 0.5 + 0.5) * (WAVE_BARS - 1);
          const dist = Math.abs(index - center);
          energy = 0.16 + 0.72 * Math.exp(-(dist * dist) / 10);
        } else {
          const wave = 0.5 + 0.5 * Math.sin(time * (2.4 + (index % 5) * 0.35) + phases[index]);
          const centerBias = 1 - Math.abs(index - (WAVE_BARS - 1) / 2) / WAVE_BARS;
          energy = 0.1 + target * (0.25 + 0.75 * wave) * (0.5 + centerBias);
        }
        envelopes[index] += (energy - envelopes[index]) * 0.28;
        const height = Math.min(WAVE_HEIGHT, 5 + (WAVE_HEIGHT - 5) * Math.min(1, envelopes[index]));
        const x = index * (barWidth + gap);
        const y = (WAVE_HEIGHT - height) / 2;
        context.globalAlpha = 0.5 + 0.5 * (height / WAVE_HEIGHT);
        context.beginPath();
        context.roundRect(x, y, barWidth, height, barWidth / 2);
        context.fill();
      }
      context.globalAlpha = 1;
      raf = requestAnimationFrame(draw);
    };
    raf = requestAnimationFrame(draw);
    return () => cancelAnimationFrame(raf);
  }, [mode]);

  return <canvas ref={canvasRef} className="live-waveform" aria-label={t("overlay.micLevel")} data-level={level.toFixed(2)} />;
}

const DRAG_IGNORE = "button, input, a, select, textarea, [role='button']";

export function RecorderOverlay({ adapter }: { adapter: AppAdapter }) {
  const { t } = useI18n();
  const [snapshot, setSnapshot] = useState<AppSnapshot>();
  const [level, setLevel] = useState(0);
  const [seconds, setSeconds] = useState(0);
  const [pending, setPending] = useState(false);
  const [actionError, setActionError] = useState("");
  const [initError, setInitError] = useState("");
  const [initAttempt, setInitAttempt] = useState(0);

  useEffect(() => {
    let active = true;
    let failed = false;
    let snapshotReady = false;
    let stateEventSeen = false;
    let registrations = 0;
    let latestSnapshot: AppSnapshot | undefined;
    const unlisteners: Array<() => void> = [];
    setSnapshot(undefined);
    setInitError("");
    const disposeListeners = () => {
      unlisteners.splice(0).forEach((unlisten) => unlisten());
    };
    const fail = (error: unknown) => {
      if (!active || failed) return;
      failed = true;
      disposeListeners();
      setInitError(normalizeError(error));
    };
    const showWhenReady = () => {
      if (active && !failed && snapshotReady && registrations === 2 && latestSnapshot) {
        setSnapshot(latestSnapshot);
      }
    };
    void adapter.getAppSnapshot()
      .then((next) => {
        if (!stateEventSeen) latestSnapshot = next;
        snapshotReady = true;
        showWhenReady();
      })
      .catch(fail);
    void adapter.onState((next) => {
      stateEventSeen = true;
      latestSnapshot = next;
      if (snapshotReady && registrations === 2 && active && !failed) setSnapshot(next);
    }).then((unlisten) => {
      if (!active || failed) unlisten();
      else {
        unlisteners.push(unlisten);
        registrations += 1;
        showWhenReady();
      }
    }).catch(fail);
    void adapter.onLevel((next) => setLevel(Math.max(0, Math.min(1, next))))
      .then((unlisten) => {
        if (!active || failed) unlisten();
        else {
          unlisteners.push(unlisten);
          registrations += 1;
          showWhenReady();
        }
      })
      .catch(fail);
    return () => {
      active = false;
      disposeListeners();
    };
  }, [adapter, initAttempt]);
  useEffect(() => {
    setSeconds(0);
    if (snapshot?.dictation.status !== "recording") return;
    const started = Date.now();
    const timer = window.setInterval(() => setSeconds(Math.floor((Date.now() - started) / 1000)), 250);
    return () => window.clearInterval(timer);
  }, [snapshot?.dictation.status]);

  useEffect(() => {
    if (snapshot?.dictation.status !== "recording") return;
    const armOnEscape = (event: KeyboardEvent) => {
      if (event.key === "Escape") void action(() => adapter.requestCancel());
    };
    window.addEventListener("keydown", armOnEscape);
    return () => window.removeEventListener("keydown", armOnEscape);
  }, [adapter, snapshot?.dictation.status]);

  useEffect(() => {
    if (snapshot?.dictation.status !== "cancelling") return;
    const onKeyDown = (event: KeyboardEvent) => {
      if (event.key === "Escape") void action(() => adapter.cancelRecording());
      else if (event.key === "Enter") void action(() => adapter.requestCancel());
    };
    window.addEventListener("keydown", onKeyDown);
    return () => window.removeEventListener("keydown", onKeyDown);
  }, [adapter, snapshot?.dictation.status]);

  useEffect(() => {
    if (snapshot?.dictation.status !== "cancelling") return;
    const timer = window.setTimeout(() => {
      void action(() => adapter.requestCancel());
    }, 10_000);
    return () => window.clearTimeout(timer);
  }, [adapter, snapshot?.dictation.status]);

  useEffect(() => {
    if (snapshot?.dictation.status === "idle") void adapter.hideOverlay();
  }, [adapter, snapshot?.dictation.status]);

  useEffect(() => {
    if (!("__TAURI_INTERNALS__" in window)) return;
    let active = true;
    let dispose: (() => void) | undefined;
    let latest: { x: number; y: number } | undefined;
    let timer: number | undefined;
    void import("@tauri-apps/api/window").then(({ getCurrentWindow }) => {
      if (!active) return;
      getCurrentWindow().onMoved(({ payload }) => {
        if (!active) return;
        latest = { x: payload.x, y: payload.y };
        window.clearTimeout(timer);
        timer = window.setTimeout(() => {
          if (latest) void adapter.saveOverlayPosition(latest.x, latest.y);
        }, 250);
      }).then((unlisten) => { if (active) dispose = unlisten; else unlisten(); });
    });
    return () => {
      active = false;
      window.clearTimeout(timer);
      dispose?.();
    };
  }, [adapter]);

  const action = async (command: () => Promise<AppSnapshot>) => {
    setPending(true);
    setActionError("");
    try {
      await command();
    } catch (error) {
      setActionError(normalizeError(error));
    } finally {
      setPending(false);
    }
  };

  const startDrag = (event: React.MouseEvent) => {
    if (event.button !== 0) return;
    if ((event.target as HTMLElement).closest(DRAG_IGNORE)) return;
    if (!("__TAURI_INTERNALS__" in window)) return;
    void import("@tauri-apps/api/window").then(({ getCurrentWindow }) => {
      void getCurrentWindow().startDragging();
    });
  };

  const state = snapshot?.dictation;

  return (
    <main className={`recorder-overlay recorder-overlay--${initError ? "init-error" : state?.status ?? "initializing"}`} aria-live="polite" data-tauri-drag-region onMouseDown={startDrag}>
      {initError ? <>
        <span className="failure-mark" aria-hidden="true">!</span>
        <div className="overlay-failure-copy" role="alert">
          <strong>{t("overlay.initError.title")}</strong>
          <span>{initError}</span>
        </div>
        <button disabled={pending} className="text-button danger-text" aria-label={t("overlay.initError.retryAria")} onClick={() => setInitAttempt((attempt) => attempt + 1)}>
          <RotateCcw size={14} /> {t("common.retry")}
        </button>
      </> : !state && <div className="overlay-content overlay-content--boot">
        <span className="spinner" aria-hidden="true" />
        <strong>{t("overlay.boot")}</strong>
      </div>}
      {!initError && state?.status === "recording" && <div className="overlay-content overlay-content--recording">
        <time className="overlay-timer">{timeLabel(seconds)}</time>
        <LiveWaveform level={level} />
      </div>}
      {!initError && state?.status === "cancelling" && <div className="overlay-content overlay-content--cancelling">
        <div className="overlay-copy"><strong>{t("overlay.cancelling.title")}</strong><small>{t("overlay.cancelling.hint")}</small></div>
      </div>}
      {!initError && state?.status === "processing" && <div className="overlay-content overlay-content--processing">
        <LiveWaveform level={0} mode="thinking" />
      </div>}
      {!initError && state?.status === "pasting" && <div className="overlay-content overlay-content--complete">
        <span className="overlay-complete-mark"><Check size={15} /></span>
        <div className="overlay-copy"><strong>{t("common.pasting")}</strong></div>
      </div>}
      {!initError && state?.status === "failed" && <div className="overlay-content overlay-content--failed">
        <span className="failure-mark" aria-hidden="true">!</span>
        <div className="overlay-copy overlay-failure-copy"><strong>{t("overlay.failed.title")}</strong><small>{t("today.cta.retryDetail")}</small></div>
        <button disabled={pending} className="text-button danger-text" aria-label={t("overlay.failed.retryAria")} onClick={() => void action(() => adapter.retryTranscription(state.recovery.recordingId))}>
          <RotateCcw size={14} /> {t("common.retry")}
        </button>
      </div>}
      {actionError && <span className="overlay-error" role="alert">{actionError}</span>}
    </main>
  );
}