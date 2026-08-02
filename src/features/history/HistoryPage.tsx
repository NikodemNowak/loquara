import { useEffect, useMemo, useState } from "react";

import { Copy, Play, RotateCcw, Search, Trash2 } from "../../components/Icons";
import { EmptyState } from "../../components/EmptyState";
import { Waveform } from "../../components/Waveform";
import type { AppAdapter } from "../../lib/tauri";
import type { Recording, RecordingStatus } from "../../lib/types";
import type { ToastKind } from "../../components/Toast";
import { useAsyncAction } from "../../lib/useAsyncAction";
import { dateLocale, useI18n, type TranslationKey } from "../../lib/i18n";
import { normalizeError } from "../../lib/errors";

const statusLabels: Record<RecordingStatus, TranslationKey> = {
  completed: "history.status.completed",
  failed: "history.status.failed",
  recording: "history.status.recording",
  processing: "history.status.processing",
  cancelled: "history.status.cancelled",
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
  const { t, lang } = useI18n();
  const [search, setSearch] = useState("");
  const [filter, setFilter] = useState<RecordingStatus | "all">("all");
  const [selectedId, setSelectedId] = useState(recordings[0]?.id);
  const [draft, setDraft] = useState("");
  const { busy, pendingKey, run } = useAsyncAction(onToast);
  const labels: Record<RecordingStatus, string> = {
    completed: t("history.status.completed"),
    failed: t("history.status.failed"),
    recording: t("history.status.recording"),
    processing: t("history.status.processing"),
    cancelled: t("history.status.cancelled"),
  };
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

  useEffect(() => {
    setDraft(selected?.text ?? "");
  }, [selected?.id, selected?.text]);

  const action = (key: string, fn: () => Promise<unknown>) =>
    run(key, async () => {
      await fn();
      await onRefresh();
    });
  const copy = (text: string) => run("copy", async () => {
    if (!navigator.clipboard?.writeText) {
      throw new Error(t("history.error.clipboard"));
    }
    await navigator.clipboard.writeText(text);
  }, "history.error.copy");

  return (
    <section className="page history-page">
      <header className="page-header">
        <div><p className="eyebrow">{t("history.eyebrow")}</p><h1>{t("history.title")}</h1><p>{t("history.subtitle")}</p></div>
      </header>
      <div className="history-toolbar">
        <label className="search-field"><Search size={17} /><span className="sr-only">{t("history.search.label")}</span><input type="search" value={search} onChange={(event) => setSearch(event.target.value)} placeholder={t("history.search.label")} /></label>
        <select aria-label={t("history.filter.label")} value={filter} onChange={(event) => setFilter(event.target.value as RecordingStatus | "all")}>
          <option value="all">{t("history.filter.all")}</option>
          <option value="completed">{t("history.filter.completed")}</option>
          <option value="failed">{t("history.filter.failed")}</option>
          <option value="recording">{t("history.filter.recording")}</option>
        </select>
      </div>
      <div className="history-workspace">
        <div className="history-list" aria-label={t("history.list.label")}>
          {filtered.map((item) => (
            <button
              key={item.id}
              className={`history-row ${selected?.id === item.id ? "history-row--selected" : ""}`}
              onClick={() => setSelectedId(item.id)}
              aria-label={item.text ?? item.error ?? labels[item.status]}
            >
              <span className={`record-status record-status--${item.status}`} title={labels[item.status]} />
              <time>{new Intl.DateTimeFormat(dateLocale(lang), { hour: "2-digit", minute: "2-digit" }).format(item.createdAt)}</time>
              <Waveform seed={item.id} active={item.status === "recording"} />
              <span className="history-row__copy"><strong>{item.text ?? item.error ?? labels[item.status]}</strong><small>{item.sourceApp ?? labels[item.status]} · {formatDuration(item.durationMs)}</small></span>
            </button>
          ))}
          {!filtered.length && <EmptyState icon={<Search size={18} />} title={t("history.empty.title")} description={t("history.empty.description")} />}
        </div>
        <aside className="record-inspector" aria-label={t("history.inspector.label")}>
          {selected ? <>
            <div className="inspector-heading"><span className={`record-status record-status--${selected.status}`} /><div><span>{t("history.inspector.status")}</span><strong>{labels[selected.status]}</strong></div></div>
            <dl>
              <div><dt>{t("history.inspector.duration")}</dt><dd>{formatDuration(selected.durationMs)}</dd></div>
              <div><dt>{t("history.inspector.model")}</dt><dd>{selected.model ?? "—"}</dd></div>
              <div><dt>{t("history.inspector.audioSaved")}</dt><dd>{selected.audioPath ? t("history.inspector.audioYesLocal") : t("history.inspector.audioNo")}</dd></div>
            </dl>
            {selected.status === "completed" && <>
              <div className="transcript-card">
                <div className="transcript-card__head">
                  <span>{t("history.transcript.title")}</span>
                  {draft.trim() !== (selected.text ?? "") && (
                    <button
                      className="primary-button"
                      disabled={busy || !draft.trim()}
                      onClick={() => void action("learn", async () => {
                        const learned = await adapter.correctTranscript(selected.id, draft.trim());
                        if (learned > 0) onToast(learned === 1 ? t("history.transcript.savedOne") : t("history.transcript.savedMany", { count: learned }), "success");
                      })}
                    >
                      {t("history.action.saveFix")}
                    </button>
                  )}
                </div>
                <textarea
                  className="transcript-edit"
                  value={draft}
                  aria-label={t("history.transcript.title")}
                  onChange={(event) => setDraft(event.target.value)}
                />
              </div>
            </>}
            {selected.error && <p className="error-note">{normalizeError(selected.error)}</p>}
            <div className="inspector-actions">
              <button disabled={!selected.text || busy} onClick={() => void copy(selected.text ?? "")}><Copy size={15} />{pendingKey === "copy" ? t("history.action.copying") : t("history.action.copy")}</button>
              <button disabled={!selected.text || busy} onClick={() => void action("paste", () => adapter.pasteTranscript(selected.id))}>{pendingKey === "paste" ? t("common.pasting") : t("history.action.paste")}</button>
              <button disabled={!selected.audioPath || ["recording", "processing"].includes(selected.status) || busy} onClick={() => void action("play", () => adapter.playRecording(selected.id))}><Play size={15} />{pendingKey === "play" ? t("history.action.playing") : t("history.action.play")}</button>
              {selected.status === "failed" && !selected.audioPath
                ? <span className="retry-unavailable">{t("history.action.retryUnavailable")}</span>
                : <button disabled={selected.status !== "failed" || !selected.audioPath || busy} onClick={() => void action("retry", () => adapter.retryTranscription(selected.id))}><RotateCcw size={15} />{pendingKey === "retry" ? t("history.action.retrying") : t("common.retry")}</button>}
              <button className="danger-button" disabled={["recording", "processing"].includes(selected.status) || busy} onClick={() => void action("delete", () => adapter.deleteHistory(selected.id))}><Trash2 size={15} />{pendingKey === "delete" ? t("common.deleting") : t("history.action.delete")}</button>
            </div>
          </> : <EmptyState title={t("history.inspector.emptyTitle")} description={t("history.inspector.emptyDescription")} />}
        </aside>
      </div>
    </section>
  );
}
