import { useEffect, useMemo, useState } from "react";

import { Copy, RotateCcw, Search, Trash2 } from "../../components/Icons";
import { EmptyState } from "../../components/EmptyState";
import { Waveform } from "../../components/Waveform";
import type { AppAdapter } from "../../lib/tauri";
import type { Recording, RecordingStatus } from "../../lib/types";
import type { ToastKind } from "../../components/Toast";
import { useAsyncAction } from "../../lib/useAsyncAction";

const labels: Record<RecordingStatus, string> = {
  completed: "Ukończono",
  failed: "Niepowodzenie",
  recording: "Aktywne nagranie",
  processing: "Przetwarzanie",
  cancelled: "Anulowano",
};

const formatDuration = (ms: number) =>
  `${String(Math.floor(ms / 60_000)).padStart(2, "0")}:${String(Math.floor(ms / 1000) % 60).padStart(2, "0")}`;

export function HistoryPage({
  adapter,
  recordings,
  onRefresh,
  onToast,
}: {
  adapter: AppAdapter;
  recordings: Recording[];
  onRefresh: () => Promise<void>;
  onToast: (message: string, kind: ToastKind) => void;
}) {
  const [search, setSearch] = useState("");
  const [filter, setFilter] = useState<RecordingStatus | "all">("all");
  const [selectedId, setSelectedId] = useState(recordings[0]?.id);
  const { busy, pendingKey, run } = useAsyncAction(onToast);
  const normalized = search.toLocaleLowerCase("pl");
  const filtered = useMemo(() => recordings.filter((item) => {
    const matchesStatus = filter === "all" || item.status === filter;
    const haystack = `${item.text ?? ""} ${item.error ?? ""} ${item.sourceApp ?? ""}`.toLocaleLowerCase("pl");
    return matchesStatus && haystack.includes(normalized);
  }), [filter, normalized, recordings]);
  const selected = filtered.find((item) => item.id === selectedId) ?? filtered[0];

  useEffect(() => {
    if (!selectedId && recordings[0]) setSelectedId(recordings[0].id);
  }, [recordings, selectedId]);

  const action = (key: string, fn: () => Promise<unknown>) =>
    run(key, async () => {
      await fn();
      await onRefresh();
    });
  const copy = (text: string) => run("copy", async () => {
    if (!navigator.clipboard?.writeText) {
      throw new Error("Schowek nie jest dostępny.");
    }
    await navigator.clipboard.writeText(text);
  }, "Nie udało się skopiować tekstu");

  return (
    <section className="page history-page">
      <header className="page-header">
        <div><p className="eyebrow">Twoje nagrania</p><h1>Historia</h1><p>Przeszukuj transkrypcje i odzyskuj nagrania po błędzie.</p></div>
      </header>
      <div className="history-toolbar">
        <label className="search-field"><Search size={17} /><span className="sr-only">Szukaj w historii</span><input type="search" value={search} onChange={(event) => setSearch(event.target.value)} placeholder="Szukaj w historii" /></label>
        <select aria-label="Filtruj według statusu" value={filter} onChange={(event) => setFilter(event.target.value as RecordingStatus | "all")}>
          <option value="all">Wszystkie statusy</option>
          <option value="completed">Ukończone</option>
          <option value="failed">Nieudane</option>
          <option value="recording">Aktywne</option>
        </select>
      </div>
      <div className="history-workspace">
        <div className="history-list" aria-label="Nagrania">
          {filtered.map((item) => (
            <button
              key={item.id}
              className={`history-row ${selected?.id === item.id ? "history-row--selected" : ""}`}
              onClick={() => setSelectedId(item.id)}
              aria-label={item.text ?? item.error ?? labels[item.status]}
            >
              <span className={`record-status record-status--${item.status}`} title={labels[item.status]} />
              <time>{new Intl.DateTimeFormat("pl-PL", { hour: "2-digit", minute: "2-digit" }).format(item.createdAt)}</time>
              <Waveform seed={item.id} active={item.status === "recording"} />
              <span className="history-row__copy"><strong>{item.text ?? item.error ?? labels[item.status]}</strong><small>{item.sourceApp ?? labels[item.status]} · {formatDuration(item.durationMs)}</small></span>
            </button>
          ))}
          {!filtered.length && <EmptyState title="Nic tu nie ma" description="Zmień wyszukiwanie lub filtr statusu." />}
        </div>
        <aside className="record-inspector" aria-label="Szczegóły nagrania">
          {selected ? <>
            <div className="inspector-heading"><span className={`record-status record-status--${selected.status}`} /><div><span>Status</span><strong>{labels[selected.status]}</strong></div></div>
            <dl>
              <div><dt>Czas trwania</dt><dd>{formatDuration(selected.durationMs)}</dd></div>
              <div><dt>Model</dt><dd>{selected.model ?? "—"}</dd></div>
              <div><dt>Zapisane audio</dt><dd>{selected.audioPath ? "Tak · lokalnie" : "Nie"}</dd></div>
            </dl>
            {selected.text && <blockquote>{selected.text}</blockquote>}
            {selected.error && <p className="error-note">{selected.error}</p>}
            <div className="inspector-actions">
              <button disabled={!selected.text || busy} onClick={() => void copy(selected.text ?? "")}><Copy size={15} />{pendingKey === "copy" ? "Kopiuję…" : "Kopiuj"}</button>
              <button disabled={!selected.text || busy} onClick={() => void action("paste", () => adapter.pasteTranscript(selected.id))}>{pendingKey === "paste" ? "Wklejam…" : "Wklej"}</button>
              <button disabled={selected.status !== "failed" || !selected.audioPath || busy} onClick={() => void action("retry", () => adapter.retryTranscription(selected.id))}><RotateCcw size={15} />{pendingKey === "retry" ? "Ponawiam…" : "Ponów"}</button>
              <button className="danger-button" disabled={["recording", "processing"].includes(selected.status) || busy} onClick={() => void action("delete", () => adapter.deleteHistory(selected.id))}><Trash2 size={15} />{pendingKey === "delete" ? "Usuwam…" : "Usuń"}</button>
            </div>
          </> : <EmptyState title="Wybierz nagranie" description="Szczegóły pojawią się w tym miejscu." />}
        </aside>
      </div>
    </section>
  );
}
