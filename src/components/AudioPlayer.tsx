import { useCallback, useEffect, useRef, useState } from "react";
import { convertFileSrc } from "@tauri-apps/api/core";

import { FolderOpen, Pause, Play } from "./Icons";
import type { AppAdapter } from "../lib/tauri";
import type { Recording } from "../lib/types";
import { normalizeError } from "../lib/errors";
import { cssColor, drawPeaks, prepareCanvas } from "../lib/waveform";
import { useI18n } from "../lib/i18n";

const isTauri = "__TAURI_INTERNALS__" in window;
const WAVE_HEIGHT = 32;
/** Arrow-key step, in seconds. */
const NUDGE = 5;

function formatTime(seconds: number) {
  const safe = Number.isFinite(seconds) && seconds > 0 ? Math.floor(seconds) : 0;
  return `${String(Math.floor(safe / 60)).padStart(2, "0")}:${String(safe % 60).padStart(2, "0")}`;
}

/**
 * Plays one recording, using its own waveform as the scrub track.
 *
 * There is no separate slider: the boundary between played and unplayed
 * colour is the playhead, so the control and the content are the same object.
 */
export function AudioPlayer({
  adapter,
  recording,
  onReveal,
}: {
  adapter: AppAdapter;
  recording: Recording;
  onReveal?: () => void;
}) {
  const { t } = useI18n();
  const audioRef = useRef<HTMLAudioElement>(null);
  const canvasRef = useRef<HTMLCanvasElement>(null);
  const [url, setUrl] = useState<string>();
  const [loading, setLoading] = useState(false);
  const [playing, setPlaying] = useState(false);
  const [current, setCurrent] = useState(0);
  const [duration, setDuration] = useState(0);
  const [error, setError] = useState("");

  // Fall back to the stored duration so the total is right before the browser
  // has parsed the file's own metadata.
  const total = duration || recording.durationMs / 1000;
  const progress = total > 0 ? Math.min(1, current / total) : 0;

  useEffect(() => () => {
    if (url?.startsWith("blob:")) URL.revokeObjectURL(url);
  }, [url]);

  useEffect(() => {
    setPlaying(false);
    setCurrent(0);
    setDuration(0);
    setError("");
    setUrl(isTauri && recording.audioPath ? convertFileSrc(recording.audioPath) : undefined);
  }, [recording.id, recording.audioPath]);

  useEffect(() => {
    const canvas = canvasRef.current;
    if (!canvas) return;
    const prepared = prepareCanvas(canvas, WAVE_HEIGHT);
    if (!prepared) return;
    drawPeaks(prepared.context, recording.peaks ?? [], prepared.width, prepared.height, {
      played: cssColor("--accent", "#4c8dff"),
      pending: cssColor("--border-strong", "#2f343a"),
      progress,
    });
  }, [recording.peaks, progress]);

  const load = useCallback(async (): Promise<string | undefined> => {
    if (url) return url;
    if (loading) return undefined;
    setLoading(true);
    setError("");
    try {
      const bytes = await adapter.getRecordingAudio(recording.id);
      const copy = new Uint8Array(bytes.byteLength);
      copy.set(bytes);
      const objectUrl = URL.createObjectURL(new Blob([copy], { type: "audio/wav" }));
      setUrl(objectUrl);
      return objectUrl;
    } catch (failure) {
      setError(normalizeError(failure));
      return undefined;
    } finally {
      setLoading(false);
    }
  }, [url, loading, recording.id, adapter]);

  const toggle = useCallback(async () => {
    const audio = audioRef.current;
    if (!audio) return;
    if (!url) {
      if (!(await load())) return;
      await audio.play().catch(() => undefined);
      return;
    }
    if (audio.paused) await audio.play().catch(() => undefined);
    else audio.pause();
  }, [url, load]);

  const seekTo = useCallback((seconds: number) => {
    const bounded = Math.max(0, Math.min(total, seconds));
    const audio = audioRef.current;
    if (audio) audio.currentTime = bounded;
    setCurrent(bounded);
  }, [total]);

  const seekToPointer = useCallback((event: React.PointerEvent<HTMLDivElement>) => {
    const bounds = event.currentTarget.getBoundingClientRect();
    if (bounds.width <= 0) return;
    seekTo(((event.clientX - bounds.left) / bounds.width) * total);
  }, [seekTo, total]);

  const onKeyDown = useCallback((event: React.KeyboardEvent) => {
    if (event.key === "ArrowLeft") { event.preventDefault(); seekTo(current - NUDGE); }
    else if (event.key === "ArrowRight") { event.preventDefault(); seekTo(current + NUDGE); }
    else if (event.key === "Home") { event.preventDefault(); seekTo(0); }
    else if (event.key === "End") { event.preventDefault(); seekTo(total); }
    else if (event.key === " " || event.key === "Enter") { event.preventDefault(); void toggle(); }
  }, [current, total, seekTo, toggle]);

  if (!recording.audioPath) return null;

  const seekable = Boolean(url) && total > 0;

  return (
    <div className="player">
      <audio
        ref={audioRef}
        src={url}
        preload="metadata"
        onPlay={() => setPlaying(true)}
        onPause={() => setPlaying(false)}
        onEnded={() => { setPlaying(false); setCurrent(0); }}
        onTimeUpdate={(event) => setCurrent(event.currentTarget.currentTime)}
        onLoadedMetadata={(event) => {
          const value = event.currentTarget.duration;
          if (Number.isFinite(value)) setDuration(value);
        }}
      />
      <button
        type="button"
        className="player__btn"
        onClick={() => void toggle()}
        disabled={loading}
        aria-label={playing ? t("history.audioPlayer.pause") : t("history.audioPlayer.play")}
      >
        {playing ? <Pause size={14} /> : <Play size={14} />}
      </button>
      <div
        className="player__scrub"
        role="slider"
        tabIndex={0}
        aria-label={t("history.audioPlayer.seek")}
        aria-valuemin={0}
        aria-valuemax={Math.round(total)}
        aria-valuenow={Math.round(current)}
        aria-valuetext={formatTime(current)}
        aria-disabled={seekable ? undefined : true}
        onKeyDown={onKeyDown}
        onPointerDown={(event) => {
          if (!seekable) return;
          event.currentTarget.setPointerCapture(event.pointerId);
          seekToPointer(event);
        }}
        onPointerMove={(event) => {
          if (!seekable || !event.currentTarget.hasPointerCapture(event.pointerId)) return;
          seekToPointer(event);
        }}
        onPointerUp={(event) => event.currentTarget.releasePointerCapture(event.pointerId)}
      >
        <canvas ref={canvasRef} className="wave" aria-hidden="true" />
      </div>
      <span className="player__time">
        {loading ? t("history.audioPlayer.loading") : `${formatTime(current)} / ${formatTime(total)}`}
      </span>
      {onReveal && (
        <button
          type="button"
          className="player__reveal"
          onClick={onReveal}
          aria-label={t("history.action.openLocation")}
          title={t("history.action.openLocation")}
        >
          <FolderOpen size={14} />
        </button>
      )}
      {error && <span className="player__error" role="alert">{error}</span>}
    </div>
  );
}
