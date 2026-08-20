import { useEffect, useRef, useState } from "react";

import { Check, RotateCcw } from "../../components/Icons";
import { applyTheme } from "../../app/theme";
import type { AppAdapter } from "../../lib/tauri";
import type { AppSnapshot, ThemeChoice } from "../../lib/types";
import { normalizeError } from "../../lib/errors";
import { useI18n } from "../../lib/i18n";

const isTauri = "__TAURI_INTERNALS__" in window;

function effectiveTheme(choice: ThemeChoice): "light" | "dark" {
  if (choice === "light" || choice === "dark") return choice;
  return window.matchMedia?.("(prefers-color-scheme: dark)").matches ? "dark" : "light";
}

const WAVE_BARS = 18;
const WAVE_HEIGHT = 18;

// Deterministic pseudo-random phases so the bars bob in place like an
// equalizer instead of forming a wave that travels left-to-right.
const BAR_PHASES = Array.from({ length: WAVE_BARS }, (_, index) => {
  const value = Math.sin(index * 127.1 + 311.7) * 43758.5453;
  return (value - Math.floor(value)) * Math.PI * 2;
});

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
    const cssWidth = canvas.clientWidth || 104;
    const dpr = window.devicePixelRatio || 1;
    canvas.width = cssWidth * dpr;
    canvas.height = WAVE_HEIGHT * dpr;
    context.scale(dpr, dpr);
    const barWidth = 2.4;
    const gap = 3.0;
    const step = barWidth + gap;
    const envelopes = new Array<number>(WAVE_BARS).fill(0);
    const reduceMotion = window.matchMedia("(prefers-reduced-motion: reduce)").matches;
    let raf = 0;

    const draw = (now: number) => {
      const time = reduceMotion ? 0 : now / 1000;
      const target = Math.max(0, Math.min(1, levelRef.current));
      context.clearRect(0, 0, cssWidth, WAVE_HEIGHT);
      const barColor =
        getComputedStyle(document.documentElement).getPropertyValue("--pill-accent").trim() ||
        "#78a9ff";
      context.fillStyle = barColor;
      const mid = (WAVE_BARS - 1) / 2;
      const contentWidth = (WAVE_BARS - 1) * step + barWidth;
      const offset = Math.max(0, (cssWidth - contentWidth) / 2);
      for (let index = 0; index < WAVE_BARS; index += 1) {
        let energy: number;
        if (mode === "thinking") {
          const center = (Math.sin(time * 1.4) * 0.5 + 0.5) * (WAVE_BARS - 1);
          const dist = Math.abs(index - center);
          energy = 0.18 + 0.74 * Math.exp(-(dist * dist) / 14);
        } else {
          const phase = BAR_PHASES[index];
          const speed = 2.0 + (index % 3) * 0.4;
          const wave =
            0.5 +
            0.5 *
              Math.sin(time * speed + phase) *
              (0.65 + 0.35 * Math.sin(time * 1.4 + phase * 1.7));
          const falloff = 0.6 + 0.4 * (1 - Math.abs(index - mid) / (mid + 1));
          const amp = 0.35 + 0.65 * target;
          energy = Math.max(0.06, amp * wave * falloff);
        }
        envelopes[index] += (energy - envelopes[index]) * 0.4;
        const height = Math.max(1.5, WAVE_HEIGHT * Math.min(1, envelopes[index]));
        const x = offset + index * step;
        const y = (WAVE_HEIGHT - height) / 2;
        context.globalAlpha = 0.45 + 0.55 * (height / WAVE_HEIGHT);
        context.fillRect(x, y, barWidth, height);
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
      if (event.key === "Enter") void action(() => adapter.cancelRecording());
      else if (event.key === "Escape") void action(() => adapter.requestCancel());
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

  useEffect(() => {
    if (!snapshot) return;
    const resolved = effectiveTheme(snapshot.settings.theme);
    applyTheme(snapshot.settings.theme);
    if (isTauri) {
      void import("@tauri-apps/api/window").then(({ getCurrentWindow }) => {
        void getCurrentWindow().setTheme(resolved);
      });
    }
  }, [snapshot]);

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