import { useState } from "react";

import { Clock3, Mic } from "../../components/Icons";
import type { ToastKind } from "../../components/Toast";
import { Waveform } from "../../components/Waveform";
import type { AppAdapter } from "../../lib/tauri";
import type { AppSnapshot, Recording } from "../../lib/types";

function formatTime(timestamp: number) {
  return new Intl.DateTimeFormat("pl-PL", { hour: "2-digit", minute: "2-digit" }).format(timestamp);
}

export function TodayPage({
  adapter,
  snapshot,
  recordings,
  onSnapshot,
  onHistory,
  onToast,
}: {
  adapter: AppAdapter;
  snapshot: AppSnapshot;
  recordings: Recording[];
  onSnapshot: (snapshot: AppSnapshot) => void;
  onHistory: () => void;
  onToast: (message: string, kind: ToastKind) => void;
}) {
  const [busy, setBusy] = useState(false);
  const state = snapshot.dictation;
  const isRecording = state.status === "recording";
  const completed = recordings.filter((item) => item.status === "completed");
  const words = completed.reduce((sum, item) => sum + (item.text?.trim().split(/\s+/).length ?? 0), 0);
  const cta = state.status === "idle"
    ? { label: "Zacznij mówić", detail: "Tekst pojawi się w aktywnym oknie", disabled: false }
    : state.status === "recording"
      ? { label: "Zatrzymaj nagrywanie", detail: "Nagrywanie trwa", disabled: false }
      : state.status === "processing"
        ? { label: "Przepisuję…", detail: "Lokalny model zamienia mowę na tekst", disabled: true }
        : state.status === "pasting"
          ? { label: "Wklejam…", detail: "Tekst trafia do aktywnego okna", disabled: true }
          : { label: "Ponów", detail: "Audio jest bezpiecznie zapisane", disabled: false };
  const toggle = async () => {
    if (cta.disabled || busy) return;
    setBusy(true);
    try {
      const next = state.status === "recording"
        ? await adapter.stopRecording()
        : state.status === "failed"
          ? await adapter.retryTranscription(state.recovery.recordingId)
          : await adapter.startRecording();
      onSnapshot(next);
    } catch (error) {
      onToast(`Nie udało się wykonać akcji: ${error instanceof Error ? error.message : String(error)}`, "error");
    } finally {
      setBusy(false);
    }
  };

  return (
    <section className="page today-page">
      <header className="page-header today-header">
        <div>
          <p className="eyebrow">Mów swobodnie. Tekst zostaje na tym urządzeniu.</p>
          <h1>Dzień dobry</h1>
          <p className="status-line"><span className="status-dot" /> {state.status === "idle" ? "Gotowy" : cta.label} · Parakeet lokalnie</p>
        </div>
      </header>

      <button disabled={cta.disabled || busy} className={`record-cta record-cta--${state.status} ${isRecording ? "record-cta--active" : ""}`} onClick={() => void toggle()}>
        <span className="record-cta__icon"><Mic size={25} /></span>
        <span><strong>{busy ? "Chwileczkę…" : cta.label}</strong><small>{cta.detail}</small></span>
        <span className="shortcut"><kbd>Ctrl</kbd><b>+</b><kbd>Spacja</kbd></span>
      </button>

      <div className="stats-strip" aria-label="Dzisiejsze statystyki">
        <div><strong>{words.toLocaleString("pl-PL")}</strong><span>słów dzisiaj</span></div>
        <div><strong>{recordings.length}</strong><span>nagrań</span></div>
        <div><strong>100%</strong><span>lokalnie i prywatnie</span></div>
      </div>

      <div className="section-heading">
        <div><h2>Ostatnie transkrypcje</h2><p>Dzisiejsza aktywność</p></div>
        <button className="text-button" onClick={onHistory}>Pokaż historię</button>
      </div>
      {recordings.length ? (
        <div className="recent-list">
          {recordings.slice(0, 5).map((item) => (
            <button key={item.id} className="recent-row" onClick={onHistory}>
              <time><Clock3 size={14} />{formatTime(item.createdAt)}</time>
              <Waveform seed={item.id} active={item.status === "recording"} />
              <span>{item.text ?? item.error ?? "Nagranie bez transkrypcji"}</span>
              <em className={`status-label status-label--${item.status}`}>{item.status === "completed" ? "Gotowe" : item.status === "failed" ? "Błąd" : "W toku"}</em>
            </button>
          ))}
        </div>
      ) : (
        <div className="onboarding">
          <span className="step-number">1</span>
          <div><strong>Naciśnij Ctrl + Spacja</strong><p>Powiedz pierwsze zdanie — Mów zapisze audio i wklei gotowy tekst.</p></div>
        </div>
      )}
    </section>
  );
}
