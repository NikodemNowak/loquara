import { useCallback, useEffect, useState } from "react";

import type { AppAdapter } from "../../lib/tauri";
import type { MeetingStatus } from "../../lib/types";
import { normalizeError } from "../../lib/errors";

export const idleMeetingStatus: MeetingStatus = {
  phase: "idle",
  meetingId: null,
  startedAt: null,
  progress: null,
  othersSilent: false,
  consentAcknowledged: false,
};

/**
 * The meeting recorder as the window sees it: its status, kept current by the
 * backend's events, and the actions that change it. Starting goes through the
 * consent notice the first time.
 */
export function useMeeting(
  adapter: AppAdapter,
  onError: (message: string) => void,
  onFinished: (meetingId: string) => void,
) {
  const [status, setStatus] = useState<MeetingStatus>(idleMeetingStatus);
  const [consentOpen, setConsentOpen] = useState(false);
  const [busy, setBusy] = useState(false);

  useEffect(() => {
    let active = true;
    const unlisteners: Array<() => void> = [];
    const keep = (unlisten: () => void) => {
      if (active) unlisteners.push(unlisten);
      else unlisten();
    };
    void adapter.getMeetingStatus().then((next) => active && setStatus(next)).catch(() => undefined);
    void adapter.onMeetingStatus((next) => active && setStatus(next)).then(keep);
    void adapter.onMeetingFinished((id) => active && onFinished(id)).then(keep);
    return () => {
      active = false;
      unlisteners.forEach((unlisten) => unlisten());
    };
  }, [adapter, onFinished]);

  const act = useCallback(async (action: () => Promise<MeetingStatus>) => {
    setBusy(true);
    try {
      setStatus(await action());
    } catch (error) {
      const message = normalizeError(error);
      if (message === "consent_required") setConsentOpen(true);
      else onError(message);
    } finally {
      setBusy(false);
    }
  }, [onError]);

  const start = useCallback(() => {
    if (!status.consentAcknowledged) {
      setConsentOpen(true);
      return;
    }
    void act(() => adapter.startMeeting());
  }, [act, adapter, status.consentAcknowledged]);

  const confirmConsent = useCallback(() => {
    setConsentOpen(false);
    void act(async () => {
      await adapter.acknowledgeMeetingConsent();
      return adapter.startMeeting();
    });
  }, [act, adapter]);

  return {
    status,
    busy,
    consentOpen,
    closeConsent: useCallback(() => setConsentOpen(false), []),
    confirmConsent,
    start,
    stop: useCallback(() => void act(() => adapter.stopMeeting()), [act, adapter]),
    cancel: useCallback(() => void act(() => adapter.cancelMeeting()), [act, adapter]),
    retry: useCallback((id: string) => act(() => adapter.retryMeeting(id)), [act, adapter]),
  };
}

export type MeetingController = ReturnType<typeof useMeeting>;
