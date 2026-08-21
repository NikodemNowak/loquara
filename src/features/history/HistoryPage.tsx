import { useEffect, useMemo, useState } from "react";

import { Copy, FolderOpen, RotateCcw, Search, Trash2 } from "../../components/Icons";
import { EmptyState } from "../../components/EmptyState";
import { AudioPlayer } from "../../components/AudioPlayer";
import { ConfirmDialog } from "../../components/ConfirmDialog";
import { Waveform } from "../../components/Waveform";
import { Select } from "../../components/Select";
import type { AppAdapter } from "../../lib/tauri";
import type { Recording, RecordingStatus } from "../../lib/types";
import type { ToastKind } from "../../components/Toast";
import { useAsyncAction } from "../../lib/useAsyncAction";
import { dateLocale, useI18n } from "../../lib/i18n";
import { normalizeError } from "../../lib/errors";

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
  const [pendingClear, setPendingClear] = useState(false);
  const { busy, pendingKey, run } = useAsyncAction(onToast);

  const labels: Record<RecordingStatus, string> = {
    completed: t("history.status.completed"),
    failed: t("history.status.failed"),
    recording: t("history.status.recording"),
    processing: t("history.status.processing"),
    cancelled: t("history.status.cancelled"),
  };
  const formatTime = (timestamp: number) =>
    new Intl.DateTimeFormat(dateLocale(lang), { hour: "2-digit", minute: "2-digit" }).format(timestamp);

  const normalized = search.toLocaleLowerCase("pl");
  const filtered = useMemo(() => recordings.filter((item) => {
    const matchesStatus = filter === "all" || item.status === filter;
    const haystack = `${item.text ?? ""} ${item.error ?? ""} ${item.sourceApp ?? ""}`.toLocaleLowerCase("pl");
    return matchesStatus && haystack.includes(normalized);
  }), [filter, normalized, recordings]);
  const selected = filtered.find((item) => item.id === selectedId) ?? filtered[0];
  const failedCount = recordings.filter((item) => item.status === "failed").length;
  const edited = Boolean(selected) && draft.trim() !== (selected?.text ?? "");

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
    if (!navigator.clipboard?.writeText) throw new Error(t("history.error.clipboard"));
    await navigator.clipboard.writeText(text);
  }, "history.error.copy");

  return (
    <section className="page history-page">
      <header className="page-header">
        <h1>{t("history.title")}</h1>
        <p>{t("history.subtitle")}</p>
      </header>

      <div className="history-toolbar">
        <label className="search-field">
          <Search size={16} />
          <span className="sr-only">{t("history.search.label")}</span>
          <input type="search" value={search} onChange={(event) => setSearch(event.target.value)} placeholder={t("history.search.label")} />
        </label>
        <Select
          label={t("history.filter.label")}
          value={filter}
          onChange={(next) => setFilter(next as RecordingStatus | "all")}
          options={[
            { value: "all", label: t("history.filter.all") },
            { value: "completed", label: t("history.filter.completed") },
            { value: "failed", label: t("history.filter.failed") },
            { value: "recording", label: t("history.filter.recording") },
          ]}
        />
        <button
          className="toolbar-button"
          onClick={() => void action("folder", () => adapter.openRecordingsFolder())}
          title={t("history.action.openFolder")}
        >
          <FolderOpen size={15} />{t("history.action.openFolder")}
        </button>
        {failedCount > 0 && (
          <button
            className="toolbar-button toolbar-button--danger"
            onClick={() => setPendingClear(true)}
            title={t("history.action.clearFailed")}
          >
            <Trash2 size={14} />{t("history.action.clearFailed")}
          </button>
        )}
      </div>

      <div className="history-workspace">
        <div className="history-list" aria-label={t("history.list.label")}>
          {filtered.map((item) => (
            <button
              key={item.id}
              className={`history-row ${selected?.id === item.id ? "history-row--selected" : ""}`}
              onClick={() => setSelectedId(item.id)}
              aria-current={selected?.id === item.id ? "true" : undefined}
              aria-label={item.text ?? item.error ?? labels[item.status]}
            >
              <span className="history-row__copy">
                <span className={`history-row__text ${item.text ? "" : "history-row__text--empty"}`}>
                  {item.text ?? item.error ?? labels[item.status]}
                </span>
                <span className="history-row__meta">
                  {item.status !== "completed" && (
                    <span className={`record-status record-status--${item.status}`} title={labels[item.status]} />
                  )}
                  <time dateTime={new Date(item.createdAt).toISOString()}>{formatTime(item.createdAt)}</time>
                  <span>{formatDuration(item.durationMs)}</span>
                  {item.sourceApp && <span>{item.sourceApp}</span>}
                </span>
              </span>
              <Waveform peaks={item.peaks} />
            </button>
          ))}
          {!filtered.length && <EmptyState icon={<Search size={18} />} title={t("history.empty.title")} description={t("history.empty.description")} />}
        </div>

        <aside className="record-inspector" aria-label={t("history.inspector.label")}>
          {selected ? <>
            {selected.status === "completed" ? (
              <div className="inspector-transcript">
                <div className="inspector-transcript__head">
                  <span>{t("history.transcript.title")}</span>
                  {edited && (
                    <button
                      className="primary-button"
                      disabled={busy || !draft.trim()}
                      onClick={() => void action("learn", async () => {
                        const learned = await adapter.correctTranscript(selected.id, draft.trim());
                        if (learned > 0) {
                          onToast(learned === 1
                            ? t("history.transcript.savedOne")
                            : t("history.transcript.savedMany", { count: learned }), "success");
                        }
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
            ) : (
              <p className="inspector-facts"><span>{labels[selected.status]}</span></p>
            )}

            {selected.error && <p className="error-note">{normalizeError(selected.error)}</p>}

            {selected.audioPath && (
              <AudioPlayer
                adapter={adapter}
                recording={selected}
                onReveal={() => void action("reveal", () => adapter.revealRecording(selected.id))}
              />
            )}

            <dl className="inspector-facts">
              <div><dt>{t("history.inspector.duration")}</dt><dd>{formatDuration(selected.durationMs)}</dd></div>
              <div><dt>{t("history.inspector.model")}</dt><dd>{selected.model ?? "—"}</dd></div>
              <div>
                <dt>{t("history.inspector.audioSaved")}</dt>
                <dd>{selected.audioPath ? t("history.inspector.audioYesLocal") : t("history.inspector.audioNo")}</dd>
              </div>
            </dl>

            <div className="inspector-actions">
              <button className="text-button" disabled={!selected.text || busy} onClick={() => void copy(selected.text ?? "")}>
                <Copy size={14} />{pendingKey === "copy" ? t("history.action.copying") : t("history.action.copy")}
              </button>
              <button className="text-button" disabled={!selected.text || busy} onClick={() => void action("paste", () => adapter.pasteTranscript(selected.id))}>
                {pendingKey === "paste" ? t("common.pasting") : t("history.action.paste")}
              </button>
              {selected.text && (
                <button className="text-button" disabled={busy} onClick={() => void action("export", async () => {
                  const path = await adapter.exportTranscript(selected.id);
                  onToast(t("history.action.exported", { path }), "success");
                })}>
                  {pendingKey === "export" ? t("history.action.exporting") : t("history.action.export")}
                </button>
              )}
              {selected.audioPath && (
                <button className="text-button" disabled={busy} onClick={() => void action("copyPath", async () => {
                  if (!navigator.clipboard?.writeText) throw new Error(t("history.error.clipboard"));
                  await navigator.clipboard.writeText(selected.audioPath ?? "");
                  onToast(t("history.action.copyPathDone"), "success");
                })}>
                  {t("history.action.copyPath")}
                </button>
              )}
              {selected.status === "failed" && (
                selected.audioPath
                  ? <button className="text-button" disabled={busy} onClick={() => void action("retry", () => adapter.retryTranscription(selected.id))}>
                      <RotateCcw size={14} />{pendingKey === "retry" ? t("history.action.retrying") : t("common.retry")}
                    </button>
                  : <span className="retry-unavailable">{t("history.action.retryUnavailable")}</span>
              )}
              <button
                className="danger-button"
                disabled={["recording", "processing"].includes(selected.status) || busy}
                onClick={() => void action("delete", () => adapter.deleteHistory(selected.id))}
              >
                <Trash2 size={15} />{pendingKey === "delete" ? t("common.deleting") : t("history.action.delete")}
              </button>
            </div>
          </> : <EmptyState title={t("history.inspector.emptyTitle")} description={t("history.inspector.emptyDescription")} />}
        </aside>
      </div>

      <ConfirmDialog
        open={pendingClear}
        title={t("history.action.clearFailedConfirm")}
        message={t("history.action.clearFailedMessage", { count: failedCount })}
        confirmLabel={t("history.action.clearFailed")}
        cancelLabel={t("common.cancel")}
        danger
        busy={pendingKey === "clearFailed"}
        onCancel={() => setPendingClear(false)}
        onConfirm={() => {
          setPendingClear(false);
          void action("clearFailed", async () => {
            const deleted = await adapter.clearFailedRecordings();
            if (deleted > 0) onToast(t("history.action.clearFailedDone", { count: deleted }), "success");
          });
        }}
      />
    </section>
  );
}
