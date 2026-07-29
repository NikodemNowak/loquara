import { useEffect, useState } from "react";

import { Check, RotateCcw, Square, X } from "../../components/Icons";
import type { AppAdapter } from "../../lib/tauri";
import type { AppSnapshot } from "../../lib/types";
import { errorMessage } from "../../lib/useAsyncAction";

function timeLabel(seconds: number) {
  return `${String(Math.floor(seconds / 60)).padStart(2, "0")}:${String(seconds % 60).padStart(2, "0")}`;
}

export function RecorderOverlay({ adapter }: { adapter: AppAdapter }) {
  const [snapshot, setSnapshot] = useState<AppSnapshot>();
  const [level, setLevel] = useState(0);
  const [seconds, setSeconds] = useState(0);
  const [pending, setPending] = useState(false);
  const [actionError, setActionError] = useState("");

  useEffect(() => {
    let active = true;
    const unlisteners: Array<() => void> = [];
    const keepOrDispose = (unlisten: () => void) => {
      if (active) unlisteners.push(unlisten);
      else unlisten();
    };
    void adapter.getAppSnapshot().then((next) => {
      if (active) setSnapshot(next);
    });
    void adapter.onState(setSnapshot).then(keepOrDispose);
    void adapter.onLevel((next) => setLevel(Math.max(0, Math.min(1, next)))).then(keepOrDispose);
    return () => {
      active = false;
      unlisteners.forEach((fn) => fn());
    };
  }, [adapter]);

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
      setActionError(errorMessage(error));
    } finally {
      setPending(false);
    }
  };

  const state = snapshot?.dictation ?? { status: "idle" as const };
  const bars = Array.from({ length: 14 }, (_, index) =>
    Math.max(3, Math.round((0.2 + ((index * 7) % 9) / 12) * (7 + level * 24))),
  );

  return (
    <main className={`recorder-overlay recorder-overlay--${state.status}`} aria-live="polite">
      {state.status === "idle" && <>
        <span className="status-dot" />
        <strong>Gotowy</strong>
      </>}
      {state.status === "recording" && <>
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
      {state.status === "processing" && <>
        <span className="spinner" aria-hidden="true" />
        <strong>Przepisuję…</strong>
      </>}
      {state.status === "pasting" && <>
        <Check size={18} className="success-icon" />
        <strong>Wklejam…</strong>
      </>}
      {state.status === "failed" && <>
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
