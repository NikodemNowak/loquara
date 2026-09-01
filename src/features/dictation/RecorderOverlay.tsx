import { useEffect, useRef, useState } from "react";

import { Check, RotateCcw, Square, X } from "../../components/Icons";
import type { AppAdapter } from "../../lib/tauri";
import type { AppSnapshot, OverlaySize } from "../../lib/types";
import { normalizeError } from "../../lib/errors";
import { LevelMeter } from "../../components/LevelMeter";
import { useI18n } from "../../lib/i18n";

const isTauri = "__TAURI_INTERNALS__" in window;

/* Pill geometry. Mirrored in tauri.conf.json, which sets the initial size.
 * The window is not resizable by the user; only this code changes its size,
 * which needs core:window:allow-set-size in the capability file. */
const MINI_WIDTH = 68;
const MINI_HEIGHT = 36;
/** Width for the states that carry words rather than a meter. */
const WORD_WIDTH = 168;
const LARGE_WIDTH = 288;
const LARGE_HEIGHT = 88;
/** How long the undo chip waits before the cancelled take stays discarded. */
const UNDO_TIMEOUT_MS = 5_000;
const WAVE_HEIGHT_MINI = 26;
const WAVE_HEIGHT_LARGE = 36;
const DRAG_IGNORE = "button, input, a, select, textarea, [role='button'], [role='menuitem']";

/**
 * The window size each state needs.
 *
 * Metering is what the pill does almost all of the time, so mini stays as
 * small as it can be. The large recording window is a different object and
 * keeps that size across transient states so it does not jump.
 */
export function overlayWindowSize(
  status: string | undefined,
  overlaySize: OverlaySize,
  initError: boolean,
) {
  if (overlaySize === "large") {
    return { width: LARGE_WIDTH, height: LARGE_HEIGHT };
  }
  if (initError) return { width: WORD_WIDTH, height: MINI_HEIGHT };
  switch (status) {
    case "cancelling":
    case "pasting":
    case "failed":
      return { width: WORD_WIDTH, height: MINI_HEIGHT };
    default:
      return { width: MINI_WIDTH, height: MINI_HEIGHT };
  }
}

function formatElapsed(seconds: number) {
  const safe = Math.max(0, Math.floor(seconds));
  const minutes = Math.floor(safe / 60);
  return `${minutes}:${String(safe % 60).padStart(2, "0")}`;
}

function modeLabel(modeId: string, translate: (key: "overlay.mode.clean") => string) {
  return modeId === "clean" ? translate("overlay.mode.clean") : modeId;
}

export function RecorderOverlay({ adapter }: { adapter: AppAdapter }) {
  const { t } = useI18n();
  const [snapshot, setSnapshot] = useState<AppSnapshot>();
  const [level, setLevel] = useState(0);
  const [pending, setPending] = useState(false);
  const [actionError, setActionError] = useState("");
  const [initError, setInitError] = useState("");
  const [initAttempt, setInitAttempt] = useState(0);
  const [elapsed, setElapsed] = useState(0);
  const [menuOpen, setMenuOpen] = useState(false);
  /** Set while the app moves its own window, so the move is not persisted. */
  const selfMoving = useRef(false);
  const statusRef = useRef<string | undefined>(undefined);

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
    void adapter.onLevel((next) => {
      if (statusRef.current !== "recording") return;
      setLevel(Math.max(0, Math.min(1, next)));
    })
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

  const status = snapshot?.dictation.status;
  statusRef.current = status;
  const overlaySize: OverlaySize = snapshot?.settings.overlaySize === "large" ? "large" : "mini";
  const large = overlaySize === "large";
  const cancelling = status === "cancelling";

  useEffect(() => {
    if (!cancelling) return;
    const timer = window.setTimeout(() => {
      void action(() => adapter.requestCancel());
    }, UNDO_TIMEOUT_MS);
    return () => window.clearTimeout(timer);
  }, [adapter, cancelling]);

  const wanted = menuOpen && overlaySize === "mini"
    ? { width: WORD_WIDTH, height: 80 }
    : overlayWindowSize(status, overlaySize, Boolean(initError));
  const applied = useRef({ width: 0, height: 0 });
  useEffect(() => {
    if (!isTauri) return;
    if (applied.current.width === wanted.width && applied.current.height === wanted.height) return;
    applied.current = wanted;
    let active = true;
    void import("@tauri-apps/api/window").then(async ({ getCurrentWindow, LogicalSize, currentMonitor }) => {
      if (!active) return;
      const overlay = getCurrentWindow();
      const position = await overlay.outerPosition();
      const before = await overlay.innerSize();
      const monitor = await currentMonitor().catch(() => null);
      const scale = monitor?.scaleFactor ?? 1;
      const growth = (wanted.height - MINI_HEIGHT) * scale;
      let openBelow = false;
      if (growth > 0) {
        const roomAbove = position.y - (monitor?.position.y ?? 0);
        openBelow = Boolean(monitor) && roomAbove < growth + 8;
      }
      selfMoving.current = true;
      try {
        await overlay.setSize(new LogicalSize(wanted.width, wanted.height));
        const after = await overlay.innerSize();
        const taller = after.height - before.height;
        const wider = after.width - before.width;
        if (taller !== 0 || wider !== 0) {
          if (!openBelow) {
            position.y -= taller;
          }
          position.x -= Math.round(wider / 2);
          await overlay.setPosition(position);
        }
      } catch {
        // The pill keeps its previous size; the prompt still reads correctly,
        // so there is nothing here the user needs to be told about.
      } finally {
        window.setTimeout(() => { selfMoving.current = false; }, 400);
      }
    });
    return () => { active = false; };
  }, [wanted.width, wanted.height]);

  useEffect(() => {
    if (status === "idle") void adapter.hideOverlay();
  }, [adapter, status]);

  useEffect(() => {
    if (status !== "recording" && status !== "processing") {
      setElapsed(0);
      return;
    }
    const started = snapshot?.recordingStartedAt ?? Date.now();
    const tick = () => setElapsed(Math.max(0, Math.floor((Date.now() - started) / 1000)));
    tick();
    const timer = window.setInterval(tick, 250);
    return () => window.clearInterval(timer);
  }, [status, snapshot?.recordingStartedAt]);

  useEffect(() => {
    if (!isTauri) return;
    let active = true;
    let dispose: (() => void) | undefined;
    let latest: { x: number; y: number } | undefined;
    let timer: number | undefined;
    void import("@tauri-apps/api/window").then(({ getCurrentWindow }) => {
      if (!active) return;
      getCurrentWindow().onMoved(({ payload }) => {
        if (!active || selfMoving.current) return;
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
    if (!isTauri) return;
    void import("@tauri-apps/api/window").then(({ getCurrentWindow }) => {
      void getCurrentWindow().setTheme("dark");
    });
  }, []);

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
    if (!isTauri) return;
    void import("@tauri-apps/api/window").then(({ getCurrentWindow }) => {
      void getCurrentWindow().startDragging();
    });
  };

  const switchSize = (next: OverlaySize) => {
    setMenuOpen(false);
    if (!snapshot || snapshot.settings.overlaySize === next) return;
    void action(async () => {
      const result = await adapter.updateSettings({ ...snapshot.settings, overlaySize: next });
      return { ...snapshot, settings: result.settings };
    });
  };

  const state = snapshot?.dictation;
  const meterVariant = large ? "expanded" : "compact";
  const waveHeight = large ? WAVE_HEIGHT_LARGE : WAVE_HEIGHT_MINI;
  const waveGap = large ? 2.5 : 2;
  const waveBar = large ? 3 : 3;

  const actions = (opts: { stop?: boolean; cancel?: boolean }) => (
    <div className="overlay-actions">
      {opts.stop && state && (state.status === "recording" || state.status === "cancelling") ? (
        <button
          type="button"
          className="overlay-icon-button overlay-icon-button--stop"
          disabled={pending}
          aria-label={t("overlay.stop")}
          onClick={() => void action(() => adapter.stopRecording())}
        >
          <Square size={11} />
        </button>
      ) : null}
      {opts.cancel ? (
        <button
          type="button"
          className="overlay-icon-button overlay-icon-button--cancel"
          disabled={pending}
          aria-label={t("overlay.cancel")}
          onClick={() => {
            if (state?.status === "cancelling") {
              void action(() => adapter.requestCancel());
              return;
            }
            void action(() => adapter.cancelRecording());
          }}
        >
          <X size={12} />
        </button>
      ) : null}
    </div>
  );

  const undoChip = state?.status === "cancelling" ? (
    <div className="overlay-undo">
      <span>{t("overlay.cancelling.title")}</span>
      <button
        type="button"
        className="overlay-undo__button"
        disabled={pending}
        aria-label={t("overlay.undoAria")}
        onClick={() => void action(() => adapter.retryTranscription(state.recordingId))}
      >
        {t("overlay.undo")}
      </button>
    </div>
  ) : null;

  const body = (() => {
    if (initError) {
      return (
        <div className="overlay-main" title={initError}>
          <div className="overlay-copy overlay-copy--alert" role="alert">
            <strong>{t("overlay.initError.title")}</strong>
          </div>
          <button
            disabled={pending}
            className="text-button"
            aria-label={t("overlay.initError.retryAria")}
            onClick={() => setInitAttempt((attempt) => attempt + 1)}
          >
            <RotateCcw size={13} />
          </button>
        </div>
      );
    }
    if (!state) {
      return (
        <div className="overlay-main overlay-main--center" role="status" aria-label={t("overlay.boot")}>
          <span className="spinner" aria-hidden="true" />
        </div>
      );
    }
    switch (state.status) {
      case "idle":
        return null;
      case "cancelling":
        return (
          <div className={`overlay-main ${large ? "overlay-main--large" : ""}`}>
            {undoChip}
          </div>
        );
      case "recording":
        return (
          <div className={`overlay-main ${large ? "overlay-main--large" : ""}`}>
            <LevelMeter
              level={level}
              variant={meterVariant}
              height={waveHeight}
              barWidth={waveBar}
              gap={waveGap}
              className="overlay-wave"
              label={t("overlay.micLevel")}
              colorToken="--pill-accent"
            />
            {large ? (
              <div className="overlay-chrome">
                <div className="overlay-meta">
                  <strong>{t("overlay.recording")}</strong>
                  <span aria-label={t("overlay.elapsed")}>{formatElapsed(elapsed)}</span>
                  <span>{modeLabel(snapshot?.settings.activeMode ?? "clean", t)}</span>
                </div>
                {actions({ stop: true, cancel: true })}
              </div>
            ) : (
              actions({ stop: true, cancel: true })
            )}
          </div>
        );
      case "processing":
        return (
          <div className={`overlay-main ${large ? "overlay-main--large" : ""}`}>
            <LevelMeter
              level={0}
              mode="thinking"
              variant={meterVariant}
              height={waveHeight}
              barWidth={waveBar}
              gap={waveGap}
              className="overlay-wave"
              label={t("common.processing")}
              colorToken="--pill-accent"
            />
            {large ? (
              <div className="overlay-chrome">
                <div className="overlay-meta">
                  <strong>{t("common.processing")}</strong>
                  <span aria-label={t("overlay.elapsed")}>{formatElapsed(elapsed)}</span>
                </div>
                {actions({ cancel: true })}
              </div>
            ) : (
              actions({ cancel: true })
            )}
          </div>
        );
      case "pasting":
        return (
          <div className="overlay-main">
            <span className="overlay-mark overlay-mark--success"><Check size={13} /></span>
            <div className="overlay-copy"><strong>{t("common.pasting")}</strong></div>
          </div>
        );
      case "failed":
        return (
          <div className="overlay-main">
            <div className="overlay-copy overlay-copy--alert">
              <strong>{t("overlay.failed.title")}</strong>
            </div>
            <button
              disabled={pending}
              className="text-button"
              aria-label={t("overlay.failed.retryAria")}
              onClick={() => void action(() => adapter.retryTranscription(state.recovery.recordingId))}
            >
              <RotateCcw size={13} />
            </button>
          </div>
        );
      default: {
        const _exhaustive: never = state;
        return _exhaustive;
      }
    }
  })();

  return (
    <main
      className={`recorder-overlay recorder-overlay--${initError ? "init-error" : state?.status ?? "initializing"} recorder-overlay--${overlaySize}`}
      aria-live="polite"
      data-tauri-drag-region
      onMouseDown={startDrag}
      onContextMenu={(event) => {
        event.preventDefault();
        setMenuOpen(true);
      }}
    >
      {menuOpen && (
        <div className="overlay-menu" role="menu" aria-label={t("overlay.menu.size")}>
          <button
            type="button"
            role="menuitem"
            aria-checked={overlaySize === "mini"}
            className={overlaySize === "mini" ? "is-active" : ""}
            onClick={() => switchSize("mini")}
          >
            {t("overlay.menu.mini")}
          </button>
          <button
            type="button"
            role="menuitem"
            aria-checked={overlaySize === "large"}
            className={overlaySize === "large" ? "is-active" : ""}
            onClick={() => switchSize("large")}
          >
            {t("overlay.menu.large")}
          </button>
        </div>
      )}
      <div className="overlay-pill">
        {body}
        {actionError && <span className="overlay-note overlay-note--error" role="alert">{actionError}</span>}
      </div>
    </main>
  );
}
