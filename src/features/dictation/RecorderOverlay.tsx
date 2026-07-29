import { useEffect, useState } from "react";

import { Check, RotateCcw, Square, X } from "../../components/Icons";
import type { AppAdapter } from "../../lib/tauri";
import type { AppSnapshot } from "../../lib/types";
import { normalizeError } from "../../lib/errors";

function timeLabel(seconds: number) {
  return `${String(Math.floor(seconds / 60)).padStart(2, "0")}:${String(seconds % 60).padStart(2, "0")}`;
}

export function RecorderOverlay({ adapter }: { adapter: AppAdapter }) {
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
        latestSnapshot = next;
        snapshotReady = true;
        showWhenReady();
      })
      .catch(fail);
    void adapter.onState((next) => {
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
    const cancelOnEscape = (event: KeyboardEvent) => {
      if (event.key === "Escape") void action(() => adapter.cancelRecording());
    };
    window.addEventListener("keydown", cancelOnEscape);
    return () => window.removeEventListener("keydown", cancelOnEscape);
  }, [adapter, snapshot?.dictation.status]);

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

  const state = snapshot?.dictation;
  const bars = Array.from({ length: 14 }, (_, index) =>
    Math.max(3, Math.round((0.2 + ((index * 7) % 9) / 12) * (7 + level * 24))),
  );

  return (
    <main className={`recorder-overlay recorder-overlay--${initError ? "init-error" : state?.status ?? "initializing"}`} aria-live="polite">
      {initError ? <>
        <span className="failure-mark" aria-hidden="true">!</span>
        <div className="overlay-failure-copy" role="alert">
          <strong>Nie udało się uruchomić</strong>
          <span>{initError}</span>
        </div>
        <button className="text-button danger-text" aria-label="Ponów inicjalizację" onClick={() => setInitAttempt((attempt) => attempt + 1)}>
          <RotateCcw size={14} /> Ponów
        </button>
      </> : !state ? <>
        <span className="spinner" aria-hidden="true" />
        <strong>Uruchamiam…</strong>
      </> : state.status === "idle" && <>
        <span className="status-dot" />
        <strong>Gotowy</strong>
      </>}
      {!initError && state?.status === "recording" && <>
        <span className="record-dot" aria-hidden="true" />
        <div className="level-bars" aria-label="Poziom mikrofonu" data-level={level.toFixed(2)}>
          {bars.map((height, index) => <i key={index} style={{ height }} />)}
        </div>
        <time>{timeLabel(seconds)}</time>
        <button disabled={pending} className="overlay-action overlay-action--stop" aria-label="Zatrzymaj nagrywanie" onClick={() => void action(() => adapter.stopRecording())}>
          <Square size={13} fill="currentColor" />
        </button>
        <button disabled={pending} className="overlay-cancel" aria-label="Anuluj nagrywanie" title="Anuluj (Esc)" onClick={() => void action(() => adapter.cancelRecording())}>
          <X size={14} /><span>Esc</span>
        </button>
      </>}
      {!initError && state?.status === "processing" && <>
        <span className="spinner" aria-hidden="true" />
        <strong>Przepisuję…</strong>
      </>}
      {!initError && state?.status === "pasting" && <>
        <Check size={18} className="success-icon" />
        <strong>Wklejam…</strong>
      </>}
      {!initError && state?.status === "failed" && <>
        <span className="failure-mark" aria-hidden="true">!</span>
        <div className="overlay-failure-copy">
          <strong>Nie udało się</strong>
          <span>Audio jest bezpiecznie zapisane</span>
        </div>
        <button disabled={pending} className="text-button danger-text" aria-label="Ponów transkrypcję" onClick={() => void action(() => adapter.retryTranscription(state.recovery.recordingId))}>
          <RotateCcw size={14} /> Ponów
        </button>
      </>}
      {actionError && <span className="overlay-error" role="alert">{actionError}</span>}
    </main>
  );
}
