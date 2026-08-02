import { Fragment, useEffect, useState } from "react";

import { Clock3, Cpu, Languages, Mic } from "../../components/Icons";
import type { ToastKind } from "../../components/Toast";
import { Waveform } from "../../components/Waveform";
import type { AppAdapter } from "../../lib/tauri";
import type { InputDeviceInfo } from "../../lib/types";
import type { AppSnapshot, Recording } from "../../lib/types";
import { normalizeError } from "../../lib/errors";
import { dateLocale, useI18n } from "../../lib/i18n";

function formatTime(timestamp: number, lang: "pl" | "en") {
  return new Intl.DateTimeFormat(dateLocale(lang), { hour: "2-digit", minute: "2-digit" }).format(timestamp);
}

function ShortcutKeys({ shortcut, inline = false }: { shortcut: string; inline?: boolean }) {
  const { t } = useI18n();
  const keys = shortcut.split("+").map((key) => key.trim()).filter(Boolean);
  const accessibleShortcut = keys.join(" + ");
  return (
    <span
      className={`shortcut ${inline ? "shortcut--inline" : ""}`}
      aria-label={t("today.shortcut.label", { shortcut: accessibleShortcut })}
    >
      {keys.map((key, index) => (
        <Fragment key={`${key}-${index}`}>
          {index > 0 && <b aria-hidden="true">+</b>}
          <kbd>{key}</kbd>
        </Fragment>
      ))}
    </span>
  );
}

const MODEL_LABELS: Record<string, string> = {
  parakeet: "Parakeet TDT 0.6B v3",
  "whisper-turbo": "Whisper Large v3 Turbo",
  "whisper-small": "Whisper Small",
  cohere: "Cohere Transcribe 2B",
};

export function TodayPage({
  adapter,
  snapshot,
  recordings,
  onSnapshot,
  onHistory,
  onSettings,
  onToast,
}: {
  adapter: AppAdapter;
  snapshot: AppSnapshot;
  recordings: Recording[];
  onSnapshot: (snapshot: AppSnapshot) => void;
  onHistory: () => void;
  onSettings?: () => void;
  onToast: (message: string, kind: ToastKind) => void;
}) {
  const { t, lang } = useI18n();
  const [busy, setBusy] = useState(false);
  const [seconds, setSeconds] = useState(0);
  const [devices, setDevices] = useState<InputDeviceInfo[]>([]);
  const state = snapshot.dictation;
  const isRecording = state.status === "recording";

  useEffect(() => {
    let active = true;
    void adapter.listInputDevices()
      .then((loaded) => { if (active) setDevices(loaded); })
      .catch(() => {});
    return () => { active = false; };
  }, [adapter]);

  const changeDevice = async (deviceId: string) => {
    try {
      const next = { ...snapshot.settings, inputDevice: deviceId || null };
      const result = await adapter.updateSettings(next);
      onSnapshot({ ...snapshot, settings: result.settings });
    } catch (error) {
      onToast(t("today.error.device", { error: normalizeError(error) }), "error");
    }
  };

  useEffect(() => {
    if (state.status !== "recording") {
      setSeconds(0);
      return;
    }
    const started = Date.now();
    const timer = window.setInterval(() => setSeconds(Math.floor((Date.now() - started) / 1000)), 250);
    return () => window.clearInterval(timer);
  }, [state.status]);

  const timeLabel = (total: number) =>
    `${String(Math.floor(total / 60)).padStart(2, "0")}:${String(total % 60).padStart(2, "0")}`;

  const cta = state.status === "idle"
    ? { label: t("today.cta.start"), detail: t("today.cta.startDetail"), disabled: false }
    : state.status === "recording"
      ? { label: t("today.cta.stop"), detail: t("today.cta.stopDetail", { time: timeLabel(seconds) }), disabled: false }
      : state.status === "processing"
        ? snapshot.modelLoading
          ? { label: t("today.cta.loadingModel"), detail: t("today.cta.loadingModelDetail"), disabled: true }
          : { label: t("common.processing"), detail: t("today.cta.processingDetail"), disabled: true }
        : state.status === "pasting"
          ? { label: t("common.pasting"), detail: t("today.cta.pastingDetail"), disabled: true }
          : { label: t("common.retry"), detail: t("today.cta.retryDetail"), disabled: false };
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
      onToast(t("common.error.action", { error: normalizeError(error) }), "error");
    } finally {
      setBusy(false);
    }
  };

  return (
    <section className="page today-page">
      <header className="page-header today-header">
        <div>
          <h1>{t("today.title")}</h1>
          <p className="status-line">{state.status === "idle" ? t("today.statusReady") : cta.label}</p>
        </div>
      </header>

      <div className="dictate-hero">
        <button disabled={cta.disabled || busy} className={`record-cta record-cta--${state.status} ${isRecording ? "record-cta--active" : ""}`} onClick={() => void toggle()}>
          <span className="record-cta__icon"><Mic size={22} /></span>
          <span><strong>{busy ? t("today.busy") : cta.label}</strong><small>{cta.detail}</small></span>
          <ShortcutKeys shortcut={snapshot.settings.shortcut} />
        </button>
        <div className="hero-chips">
          <span className="hero-chip" aria-hidden="true"><Cpu size={12} />{t("today.chips.model")}: <strong>{MODEL_LABELS[snapshot.settings.model] ?? snapshot.settings.model}</strong></span>
          <span className="hero-chip" aria-hidden="true"><Languages size={12} />{t("today.chips.language")}: <strong>{lang.toUpperCase()}</strong></span>
        </div>
      </div>

      <label className="mic-quick">
        <Mic size={14} />
        <span>{t("today.microphone")}</span>
        <select value={snapshot.settings.inputDevice ?? ""} disabled={isRecording} onChange={(event) => void changeDevice(event.target.value)} aria-label={t("today.microphone")}>
          <option value="">{t("today.microphone.default")}</option>
          {devices.map((device) => <option value={device.id} key={device.id}>{device.name}</option>)}
        </select>
      </label>

      <div className="section-heading">
        <div><h2>{t("today.recent.title")}</h2><p>{t("today.recent.subtitle")}</p></div>
        <button className="text-button" onClick={onHistory}>{t("today.recent.showHistory")}</button>
      </div>
      {recordings.length ? (
        <div className="recent-list">
          {recordings.slice(0, 5).map((item) => (
            <button key={item.id} className="recent-row" onClick={onHistory}>
              <time><Clock3 size={14} />{formatTime(item.createdAt, lang)}</time>
              <Waveform seed={item.id} active={item.status === "recording"} />
              <span>{item.text ?? item.error ?? t("today.recent.noTranscript")}</span>
              <em className={`status-label status-label--${item.status}`}>{item.status === "completed" ? t("today.statusReady") : item.status === "failed" ? t("common.error") : t("today.recent.statusPending")}</em>
            </button>
          ))}
        </div>
      ) : (
        <div className="onboarding">
          <div className="steps">
            <div className="step"><span className="step-number">1</span><div><strong>{t("today.onboarding.press")} <ShortcutKeys shortcut={snapshot.settings.shortcut} inline /></strong><p>{t("today.onboarding.body")}</p></div></div>
            <div className="step"><span className="step-number">2</span><div><strong>{t("today.onboarding.modelTitle")}</strong><p>{t("today.onboarding.modelBody")}{onSettings ? <button className="text-button" onClick={onSettings}>{t("today.onboarding.open")}</button> : null}</p></div></div>
            <div className="step"><span className="step-number">3</span><div><strong>{t("today.onboarding.languageTitle")}</strong><p>{t("today.onboarding.languageBody")}{onSettings ? <button className="text-button" onClick={onSettings}>{t("today.onboarding.open")}</button> : null}</p></div></div>
          </div>
        </div>
      )}
    </section>
  );
}
