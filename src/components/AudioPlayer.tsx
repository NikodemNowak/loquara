import { useCallback, useEffect, useRef, useState } from "react";
import { convertFileSrc } from "@tauri-apps/api/core";

import { FolderOpen, Pause, Play, Square } from "./Icons";
import type { AppAdapter } from "../lib/tauri";
import type { Recording } from "../lib/types";
import { normalizeError } from "../lib/errors";
import { useI18n } from "../lib/i18n";

const isTauri = "__TAURI_INTERNALS__" in window;

function formatTime(seconds: number) {
  if (!Number.isFinite(seconds) || seconds < 0) seconds = 0;
  const total = Math.floor(seconds);
  return `${String(Math.floor(total / 60)).padStart(2, "0")}:${String(total % 60).padStart(2, "0")}`;
}

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
  const [url, setUrl] = useState<string>();
  const [loading, setLoading] = useState(false);
  const [playing, setPlaying] = useState(false);
  const [current, setCurrent] = useState(0);
  const [duration, setDuration] = useState(0);
  const [error, setError] = useState("");

  useEffect(() => {
    return () => {
      if (url && url.startsWith("blob:")) URL.revokeObjectURL(url);
    };
  }, [url]);

  useEffect(() => {
    setPlaying(false);
    setCurrent(0);
    setDuration(0);
    setError("");
    if (isTauri && recording.audioPath) {
      setUrl(convertFileSrc(recording.audioPath));
    } else {
      setUrl(undefined);
    }
  }, [recording.id, recording.audioPath]);

  const load = useCallback(async (): Promise<string | undefined> => {
    if (url) return url;
    if (loading) return undefined;
    setLoading(true);
    setError("");
    try {
      const bytes = await adapter.getRecordingAudio(recording.id);
      const copy = new Uint8Array(bytes.byteLength);
      copy.set(bytes);
      const blob = new Blob([copy], { type: "audio/wav" });
      const objectUrl = URL.createObjectURL(blob);
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
      const next = await load();
      if (!next) return;
      await audio.play().catch(() => undefined);
      return;
    }
    if (audio.paused) await audio.play().catch(() => undefined);
    else audio.pause();
  }, [url, load]);

  const stop = useCallback(() => {
    const audio = audioRef.current;
    if (!audio) return;
    audio.pause();
    audio.currentTime = 0;
  }, []);

  const seek = useCallback((value: number) => {
    const audio = audioRef.current;
    if (audio) audio.currentTime = value;
    setCurrent(value);
  }, []);

  const disabled = !recording.audioPath;

  if (disabled) return null;

  return (
    <div className="audio-player">
      <audio
        ref={audioRef}
        src={url}
        preload="metadata"
        onPlay={() => setPlaying(true)}
        onPause={() => setPlaying(false)}
        onEnded={() => { setPlaying(false); setCurrent(0); }}
        onTimeUpdate={(event) => setCurrent(event.currentTarget.currentTime)}
        onLoadedMetadata={(event) => setDuration(event.currentTarget.duration)}
      />
      <div className="audio-player__transport">
        <button
          type="button"
          className={`audio-player__btn ${playing ? "audio-player__btn--pause" : "audio-player__btn--play"}`}
          onClick={() => void toggle()}
          disabled={loading}
          aria-label={playing ? t("history.audioPlayer.pause") : t("history.audioPlayer.play")}
        >
          {playing ? <Pause size={14} /> : <Play size={14} />}
        </button>
        <button
          type="button"
          className="audio-player__btn audio-player__btn--stop"
          onClick={stop}
          disabled={!url}
          aria-label={t("history.audioPlayer.stop")}
        >
          <Square size={11} />
        </button>
      </div>
      <input
        className="audio-player__seek"
        type="range"
        min={0}
        max={duration || 0}
        step={0.01}
        value={Math.min(current, duration || 0)}
        onChange={(event) => seek(Number(event.target.value))}
        aria-label={t("history.audioPlayer.seek")}
        disabled={!url}
      />
      <span className="audio-player__time">
        {loading ? t("history.audioPlayer.loading") : `${formatTime(current)} / ${formatTime(duration)}`}
      </span>
      {onReveal && (
        <button
          type="button"
          className="audio-player__reveal"
          onClick={onReveal}
          aria-label={t("history.action.openLocation")}
          title={t("history.action.openLocation")}
        >
          <FolderOpen size={14} />
        </button>
      )}
      {error && <span className="audio-player__error" role="alert">{error}</span>}
    </div>
  );
}
