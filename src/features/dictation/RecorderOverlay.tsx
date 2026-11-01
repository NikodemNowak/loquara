import { useEffect, useRef, useState } from "react";

import { ErrorCircleRegular } from "@fluentui/react-icons";
import { Check, RotateCcw, Square, X } from "../../components/Icons";
import type { AppAdapter } from "../../lib/tauri";
import type { AppSnapshot, OverlaySize } from "../../lib/types";
import "./RecorderOverlay.css";
import { normalizeError } from "../../lib/errors";
import { Logo } from "../../components/Logo";
import { LevelMeter } from "../../components/LevelMeter";
import { useI18n } from "../../lib/i18n";

const isTauri = "__TAURI_INTERNALS__" in window;

/* Pill geometry. Mirrored in tauri.conf.json, which sets the initial size.
 * The window is not resizable by the user; only this code changes its size,
 * which needs core:window:allow-set-size in the capability file. */
const MINI_WIDTH = 136;
const MINI_HEIGHT = 36;
const DRAG_IGNORE = "button, input, a, select, textarea, [role='button']";

/** Legacy expanded preferences never change the single compact surface. */
export function overlayWindowSize(
  _status: string | undefined,
  _overlaySize: OverlaySize,
  _initError: boolean,
) {
  return { width: MINI_WIDTH, height: MINI_HEIGHT };
}

/** Coordinates and dimensions here are physical pixels, including work-area bounds. */
export function clampOverlayPosition(
  position: { x: number; y: number },
  size: { width: number; height: number },
  area: { position: { x: number; y: number }; size: { width: number; height: number } },
) {
  return {
    x: Math.max(area.position.x, Math.min(position.x, area.position.x + area.size.width - size.width)),
    y: Math.max(area.position.y, Math.min(position.y, area.position.y + area.size.height - size.height)),
  };
}

export function RecorderOverlay({ adapter }: { adapter: AppAdapter }) {
  const { t } = useI18n();
  const [snapshot, setSnapshot] = useState<AppSnapshot>();
  const [level, setLevel] = useState(0);
  const [pending, setPending] = useState(false);
  const [actionError, setActionError] = useState("");
  const [initError, setInitError] = useState("");
  const [initAttempt, setInitAttempt] = useState(0);
  /** Set while the app moves its own window, so the move is not persisted. */
  const selfMoving = useRef(false);
  const statusRef = useRef<string | undefined>(undefined);
  const observedStateVersion = useRef(0);

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
    const expectRegistrations = 3;
    const showWhenReady = () => {
      if (active && !failed && snapshotReady && registrations === expectRegistrations && latestSnapshot) {
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
      observedStateVersion.current += 1;
      latestSnapshot = next;
      if (snapshotReady && registrations === expectRegistrations && active && !failed) setSnapshot(next);
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
    void adapter.onError((message) => {
      if (!active || failed) return;
      // Draft clipboard errors are nonfatal; never replace the live meter.
      setActionError(message);
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
  const recordingId = snapshot?.dictation.status === "recording" ? snapshot.dictation.recordingId : null;
  useEffect(() => { setLevel(0); setActionError(""); }, [recordingId]);

  const wanted = overlayWindowSize(status, "mini", Boolean(initError || actionError));
  const applied = useRef({ width: 0, height: 0 });
  const resizeQueue = useRef<Promise<void>>(Promise.resolve());
  useEffect(() => {
    if (!isTauri) return;
    if (applied.current.width === wanted.width && applied.current.height === wanted.height) return;
    let active = true;
    resizeQueue.current = resizeQueue.current.catch(() => {}).then(async () => {
      const { getCurrentWindow, LogicalSize, currentMonitor } = await import("@tauri-apps/api/window");
      if (!active) return;
      const overlay = getCurrentWindow();
      const position = await overlay.outerPosition();
      const monitor = await currentMonitor().catch(() => null);
      selfMoving.current = true;
      try {
        await overlay.setSize(new LogicalSize(wanted.width, wanted.height));
        applied.current = wanted;
        const after = await overlay.innerSize();
        if (monitor) {
          const safe = clampOverlayPosition(position, after, monitor.workArea);
          if (safe.x !== position.x || safe.y !== position.y) {
            position.x = safe.x;
            position.y = safe.y;
            await overlay.setPosition(position);
          }
        }
      } catch {
        // Native geometry is retried on the next mount; state actions stay available.
      } finally {
        window.setTimeout(() => { selfMoving.current = false; }, 400);
      }
    });
    return () => { active = false; };
  }, [wanted.width, wanted.height]);

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
    const version = observedStateVersion.current;
    try {
      const next = await command();
      // A state event observed while the command ran is newer than its reply.
      if (observedStateVersion.current === version) setSnapshot(next);
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

  const state = snapshot?.dictation;
  const notice = snapshot?.overlayNotice;
  // Keep the native window stable so the capsule can fold around its centre
  // without moving the saved desktop position or resizing WebView mid-animation.
  const solo = !initError && (!state || state.status === "processing" || state.status === "pasting"
    || (state.status === "idle" && (notice?.kind === "copied" || notice?.kind === "pasteRequested")));
  const cancel = () => state?.status === "cancelling" ? adapter.requestCancel() : adapter.cancelRecording();
  const cancelButton = <button type="button" className="overlay-icon-button overlay-icon-button--cancel" disabled={pending}
    aria-label={t("overlay.cancel")} title={t("overlay.cancel")} onClick={() => void action(cancel)}><X size={14} /></button>;
  const body = (() => {
    if (initError) return <div className="overlay-main">
      <span className="overlay-label overlay-label--error" role="alert" title={initError}
        aria-label={`${t("overlay.initError.title")}: ${initError}`}>{t("overlay.initError.compact")}</span>
      <button type="button" className="overlay-icon-button" aria-label={t("overlay.initError.retryAria")}
        title={t("overlay.initError.retryAria")} onClick={() => setInitAttempt((attempt) => attempt + 1)}><RotateCcw size={15} /></button>
    </div>;
    if (!state) return <div className="overlay-main" role="status" aria-label={t("overlay.boot")}><Logo size={20} motion="working" /><span className="overlay-sr-only">{t("overlay.boot")}</span></div>;
    if (state.status === "cancelling") return <div className="overlay-main">
      <X size={14} /><span className="overlay-label">{t("overlay.cancelled")}</span>
      <button type="button" className="overlay-undo" disabled={pending} aria-label={t("overlay.undoAria")}
        title={t("overlay.undoAria")} onClick={() => void action(() => adapter.retryTranscription(state.recordingId))}>{t("overlay.undo")}</button></div>;
    if (state.status === "failed") return <div className="overlay-main">
      <ErrorCircleRegular className="overlay-failure-icon" aria-hidden="true" />
      <button type="button" className="overlay-text-button" disabled={pending} title={state.error}
        aria-label={t("overlay.failed.retryAria")}
        onClick={() => void action(() => adapter.retryTranscription(state.recovery.recordingId))}>{t("overlay.failed.compact")}</button>
      <span className="overlay-sr-only" role="alert">{state.error}</span></div>;
    if (state.status === "idle" && notice) {
      if (notice.kind === "deliveryFailed") return <div className="overlay-main overlay-main--interactive" role="alert">
        <div className="overlay-reading"><ErrorCircleRegular className="overlay-failure-icon" aria-hidden="true" /><span className="overlay-label overlay-label--error" title={notice.detail ?? undefined}>{t("overlay.deliveryFailed")}</span></div>
        <div className="overlay-actions">
          <button className="overlay-text-button" type="button" aria-label={t("overlay.openDetails")} title={notice.detail ?? t("overlay.openDetails")}
            onClick={() => void adapter.openOverlayDetails(notice.key, notice.generation).catch((error) => setActionError(normalizeError(error)))}>{t("overlay.deliveryFailed")}</button>
          <button className="overlay-icon-button" type="button" aria-label={t("overlay.dismiss")} title={t("overlay.dismiss")}
            onClick={() => void action(() => adapter.dismissOverlayNotice(notice.key, notice.generation))}><X size={14} /></button>
        </div></div>;
      const label = t(notice.kind === "copied" ? "overlay.copied" : notice.kind === "cancelled" ? "overlay.cancelled" : "overlay.ready");
      const detail = notice.kind === "pasteRequested" ? t("overlay.pasteRequestedDetail") : label;
      return <div className="overlay-main" role="status" aria-label={detail} title={detail}>
        {notice.kind === "cancelled" ? <X size={15} /> : <Check size={18} className="overlay-success" />}<span className={notice.kind === "cancelled" ? "overlay-label" : "overlay-sr-only"}>{label}</span></div>;
    }
    if (state.status === "idle") return null;
    const recording = state.status === "recording";
    // Pasting remains busy until the native delivery result arrives. It is not
    // an acknowledgement that another application accepted the transcript.
    return <div className={`overlay-main overlay-main--interactive ${recording ? "overlay-main--recording" : "overlay-main--processing"}`}>
      <div className="overlay-reading">
        <Logo size={20} motion={recording ? undefined : "working"} />
        {recording ? <LevelMeter level={level} variant="compact" height={18} barWidth={2} gap={2}
          className="overlay-wave" label={t("overlay.micLevel")} colorToken="--pill-accent" />
          : <span className="overlay-sr-only" role="status" aria-label={t("overlay.transcribing")}>{t("overlay.transcribing")}</span>}
      </div>
      {state.status !== "pasting" ? <div className="overlay-actions">
        {recording ? <button type="button" className="overlay-icon-button overlay-icon-button--stop" disabled={pending}
          aria-label={t("overlay.stop")} title={t("overlay.stop")} onClick={() => void action(() => adapter.stopRecording())}><Square size={11} /></button> : null}
        {cancelButton}
      </div> : null}
    </div>;
  })();

  return <main className={`recorder-overlay recorder-overlay--${initError ? "init-error" : state?.status ?? "initializing"}`}
    style={!isTauri ? { width: wanted.width, height: wanted.height } : undefined}
    aria-live="polite" title={actionError || undefined} data-tauri-drag-region onMouseDown={startDrag} onContextMenu={(event) => event.preventDefault()}>
    <div className={`overlay-pill${solo ? " overlay-pill--solo" : state?.status === "recording" && !initError ? " overlay-pill--recording" : ""}`}>{body}</div>
    {actionError ? <span className="overlay-sr-only" role="alert">{actionError}</span> : null}
  </main>;
}
