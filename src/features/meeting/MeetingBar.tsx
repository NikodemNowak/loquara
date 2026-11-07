import { useEffect, useState } from "react";

import { ConfirmDialog } from "../../components/ConfirmDialog";
import { Square, X } from "../../components/Icons";
import { useI18n } from "../../lib/i18n";
import type { MeetingController } from "./useMeeting";

export const formatClock = (ms: number) => {
  const total = Math.max(0, Math.floor(ms / 1000));
  const hours = Math.floor(total / 3600);
  const minutes = String(Math.floor(total / 60) % 60).padStart(2, "0");
  const seconds = String(total % 60).padStart(2, "0");
  return hours ? `${hours}:${minutes}:${seconds}` : `${minutes}:${seconds}`;
};

/**
 * Stays at the top of every page while a meeting is recorded or transcribed,
 * so the recording can never run unnoticed.
 */
export function MeetingBar({ meeting }: { meeting: MeetingController }) {
  const { t } = useI18n();
  const { status } = meeting;
  const [now, setNow] = useState(() => Date.now());
  const [confirmDiscard, setConfirmDiscard] = useState(false);

  useEffect(() => {
    if (status.phase !== "recording") return;
    // The clock has been still since the last meeting; catch it up first.
    setNow(Date.now());
    const timer = window.setInterval(() => setNow(Date.now()), 1000);
    return () => window.clearInterval(timer);
  }, [status.phase]);

  return (
    <>
      {status.phase === "recording" && (
        <div className="meeting-bar" role="status" aria-live="polite">
          <span className="meeting-bar__dot" aria-hidden="true" />
          <div className="meeting-bar__copy">
            <strong>{t("meeting.recording")}</strong>
            <span className="meeting-bar__clock">{formatClock(now - (status.startedAt ?? now))}</span>
            <p>{status.othersSilent ? t("meeting.othersSilent") : t("meeting.headphones")}</p>
          </div>
          <button className="text-button" disabled={meeting.busy} onClick={() => setConfirmDiscard(true)}>
            <X size={14} />{t("meeting.discard")}
          </button>
          <button className="primary-button" disabled={meeting.busy} onClick={meeting.stop}>
            <Square size={12} />{t("meeting.stop")}
          </button>
        </div>
      )}
      {status.phase === "processing" && (
        <div className="meeting-bar meeting-bar--processing" role="status" aria-live="polite">
          <div className="meeting-bar__copy">
            <strong>{t("meeting.processing", { percent: Math.round((status.progress ?? 0) * 100) })}</strong>
          </div>
          <div className="meeting-bar__progress" aria-hidden="true">
            <i style={{ width: `${Math.round((status.progress ?? 0) * 100)}%` }} />
          </div>
          <button className="text-button" disabled={meeting.busy} onClick={meeting.cancel}>
            {t("meeting.cancelProcessing")}
          </button>
        </div>
      )}
      <ConfirmDialog
        open={meeting.consentOpen}
        title={t("meeting.consent.title")}
        message={t("meeting.consent.message")}
        confirmLabel={t("meeting.consent.confirm")}
        cancelLabel={t("common.cancel")}
        onCancel={meeting.closeConsent}
        onConfirm={meeting.confirmConsent}
      />
      <ConfirmDialog
        open={confirmDiscard}
        title={t("meeting.discardConfirm.title")}
        message={t("meeting.discardConfirm.message")}
        confirmLabel={t("meeting.discard")}
        cancelLabel={t("common.cancel")}
        danger
        onCancel={() => setConfirmDiscard(false)}
        onConfirm={() => {
          setConfirmDiscard(false);
          meeting.cancel();
        }}
      />
    </>
  );
}

/** Starts a meeting; lives in the status bar so it is reachable from anywhere. */
export function MeetingStartButton({ meeting }: { meeting: MeetingController }) {
  const { t } = useI18n();
  if (meeting.status.phase !== "idle") return null;
  return (
    <button className="meeting-start" disabled={meeting.busy} onClick={meeting.start}>
      <i aria-hidden="true" />{t("meeting.start")}
    </button>
  );
}
