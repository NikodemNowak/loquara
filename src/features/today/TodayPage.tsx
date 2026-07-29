import { Clock3, Mic } from "../../components/Icons";
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
}: {
  adapter: AppAdapter;
  snapshot: AppSnapshot;
  recordings: Recording[];
  onSnapshot: (snapshot: AppSnapshot) => void;
  onHistory: () => void;
}) {
  const isRecording = snapshot.dictation.status === "recording";
  const completed = recordings.filter((item) => item.status === "completed");
  const words = completed.reduce((sum, item) => sum + (item.text?.trim().split(/\s+/).length ?? 0), 0);
  const toggle = async () => onSnapshot(isRecording ? await adapter.stopRecording() : await adapter.startRecording());

  return (
    <section className="page today-page">
      <header className="page-header today-header">
        <div>
          <p className="eyebrow">Mów swobodnie. Tekst zostaje na tym urządzeniu.</p>
          <h1>Dzień dobry</h1>
          <p className="status-line"><span className="status-dot" /> Gotowy · Parakeet lokalnie</p>
        </div>
      </header>

      <button className={`record-cta ${isRecording ? "record-cta--active" : ""}`} onClick={() => void toggle()}>
        <span className="record-cta__icon"><Mic size={25} /></span>
        <span><strong>{isRecording ? "Zatrzymaj nagrywanie" : "Zacznij mówić"}</strong><small>{isRecording ? "Nagrywanie trwa" : "Tekst pojawi się w aktywnym oknie"}</small></span>
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
