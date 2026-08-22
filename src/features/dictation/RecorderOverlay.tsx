import { useEffect, useRef, useState } from "react";

import { Check, RotateCcw } from "../../components/Icons";
import type { AppAdapter } from "../../lib/tauri";
import type { AppSnapshot } from "../../lib/types";
import { normalizeError } from "../../lib/errors";
import { LevelMeter } from "../../components/LevelMeter";
import { useI18n } from "../../lib/i18n";

const isTauri = "__TAURI_INTERNALS__" in window;

/* Pill geometry. Mirrored in tauri.conf.json, which sets the initial size.
 * The window is not resizable by the user; only this code changes its size,
 * which needs core:window:allow-set-size in the capability file. */
const PILL_WIDTH = 68;
const PILL_HEIGHT = 36;
/** Width for the states that carry words rather than a meter. */
const WORD_WIDTH = 168;
/** The question is a card above the pill, so the window holds both plus the
 *  transparent gap between them. */
const ASK_WIDTH = 164;
const ASK_HEIGHT = 134;

/**
 * The window size each state needs.
 *
 * Metering is what the pill does almost all of the time, so that state is
 * kept as small as it can be. The transient states that have something to say
 * widen rather than truncate it — a clipped error is worse than a wider pill
 * for two seconds.
 */
function sizeFor(status: string | undefined, initError: boolean) {
  if (initError) return { width: WORD_WIDTH, height: PILL_HEIGHT };
  switch (status) {
    case "cancelling":
      return { width: ASK_WIDTH, height: ASK_HEIGHT };
    case "pasting":
    case "failed":
      return { width: WORD_WIDTH, height: PILL_HEIGHT };
    default:
      return { width: PILL_WIDTH, height: PILL_HEIGHT };
  }
}
/** How long the cancel prompt waits before withdrawing itself. */
const CONFIRM_TIMEOUT_MS = 10_000;

const WAVE_HEIGHT = 26;

const DRAG_IGNORE = "button, input, a, select, textarea, [role='button']";

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

  const status = snapshot?.dictation.status;
  const cancelling = status === "cancelling";

  // Withdrawing the question is the safe default, so it is the one thing the
  // pill still decides for itself. The deadline is shared with the meter,
  // which spends its bars against it.
  const [askDeadline, setAskDeadline] = useState(0);
  useEffect(() => {
    if (!cancelling) return;
    setAskDeadline(Date.now() + CONFIRM_TIMEOUT_MS);
    const timer = window.setTimeout(() => {
      void action(() => adapter.requestCancel());
    }, CONFIRM_TIMEOUT_MS);
    return () => window.clearTimeout(timer);
  }, [adapter, cancelling]);

  // The window is sized to whatever the current state needs, keeping its
  // bottom edge so it never slides off a screen where it already sits near the
  // taskbar. If the platform refuses to resize, nothing moves and `grown`
  // stays false, so the cancel prompt renders inside the pill instead.
  const wanted = sizeFor(status, Boolean(initError));
  // Which side of the pill the question stands on. Above by default, because
  // the pill normally lives at the bottom of the screen — but a pill dragged
  // to the top has nothing above it, and the card would open off-screen.
  const [below, setBelow] = useState(false);
  const placedBelow = useRef(false);
  // Nothing is assumed about the size the window was created at: the first
  // render applies the size this state wants. Assuming it already matched
  // left the recording pill at whatever width it happened to open with, and
  // the first prompt then shrank it — the same pill, two sizes.
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
      // Growing means the window needs room somewhere. Ask the screen whether
      // there is any above the pill before deciding to open upwards.
      const monitor = await currentMonitor().catch(() => null);
      const scale = monitor?.scaleFactor ?? 1;
      const growth = (wanted.height - PILL_HEIGHT) * scale;
      if (growth > 0) {
        const roomAbove = position.y - (monitor?.position.y ?? 0);
        placedBelow.current = Boolean(monitor) && roomAbove < growth + 8;
        setBelow(placedBelow.current);
      }
      selfMoving.current = true;
      try {
        await overlay.setSize(new LogicalSize(wanted.width, wanted.height));
        // Move by however much the window actually changed, not by how much
        // it was asked to: a platform that refuses the resize resolves the
        // call anyway, and moving on that promise would shift the pill for a
        // change that never happened.
        const after = await overlay.innerSize();
        const taller = after.height - before.height;
        const wider = after.width - before.width;
        if (taller !== 0 || wider !== 0) {
          // The pill is centred horizontally, so half of any width change has
          // to be given back — otherwise it slides sideways the moment the
          // question appears. Vertically it depends on which way the window
          // grew: opening upwards moves the top edge, opening downwards
          // leaves it exactly where it is.
          if (!placedBelow.current) {
            position.y -= taller;
          }
          position.x -= Math.round(wider / 2);
          await overlay.setPosition(position);
        }
        if (taller < 0) {
          placedBelow.current = false;
        }
      } catch {
        // The pill keeps its previous size; the prompt still reads correctly,
        // so there is nothing here the user needs to be told about.
      } finally {
        // Let the move event settle before re-arming the position writer.
        window.setTimeout(() => { selfMoving.current = false; }, 400);
      }
    });
    return () => { active = false; };
  }, [wanted.width, wanted.height]);

  useEffect(() => {
    if (status === "idle") void adapter.hideOverlay();
  }, [adapter, status]);

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

  const state = snapshot?.dictation;
  return (
    <main
      className={`recorder-overlay recorder-overlay--${initError ? "init-error" : state?.status ?? "initializing"}${cancelling ? " recorder-overlay--asking" : ""}${cancelling && below ? " recorder-overlay--below" : ""}`}
      aria-live="polite"
      data-tauri-drag-region
      onMouseDown={startDrag}
    >
      {cancelling && (
        <div className="ask-card" role="alertdialog" aria-label={t("overlay.cancelling.title")}>
          <strong className="ask-card__question">{t("overlay.cancelling.title")}</strong>
          <dl className="ask-card__answers">
            <dt><kbd>Enter</kbd></dt>
            <dd className="ask-card__discard">{t("overlay.cancelling.confirm")}</dd>
            <dt><kbd>Esc</kbd></dt>
            <dd>{t("overlay.cancelling.dismiss")}</dd>
          </dl>
        </div>
      )}
      <div className="overlay-pill">
      {initError ? (
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
      ) : !state ? (
        <div className="overlay-main overlay-main--center" role="status" aria-label={t("overlay.boot")}>
          <span className="spinner" aria-hidden="true" />
        </div>
      ) : cancelling ? (
        <div className="overlay-main">
          <LevelMeter
            level={level}
            mode="countdown"
            deadline={askDeadline}
            span={CONFIRM_TIMEOUT_MS}
            height={WAVE_HEIGHT}
            barWidth={3}
            gap={2}
            className="overlay-wave"
            label={t("overlay.cancelling.countdown")}
            colorToken="--pill-accent"
          />
        </div>
      ) : state.status === "recording" ? (
        <div className="overlay-main">
          <LevelMeter level={level} height={WAVE_HEIGHT} barWidth={3} gap={2} className="overlay-wave" label={t("overlay.micLevel")} colorToken="--pill-accent" />
        </div>
      ) : state.status === "processing" ? (
        <div className="overlay-main">
          <LevelMeter level={0} mode="thinking" height={WAVE_HEIGHT} barWidth={3} gap={2} className="overlay-wave" label={t("common.processing")} colorToken="--pill-accent" />
        </div>
      ) : state.status === "pasting" ? (
        <div className="overlay-main">
          <span className="overlay-mark overlay-mark--success"><Check size={13} /></span>
          <div className="overlay-copy"><strong>{t("common.pasting")}</strong></div>
        </div>
      ) : state.status === "failed" ? (
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
      ) : null}


      {actionError && <span className="overlay-note overlay-note--error" role="alert">{actionError}</span>}
      </div>
    </main>
  );
}
