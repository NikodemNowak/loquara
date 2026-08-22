import { useCallback, useEffect, useRef, useState } from "react";
import type { AppAdapter } from "../lib/tauri";
import type { AppSettings, AppSnapshot, Recording } from "../lib/types";
import { normalizeError } from "../lib/errors";
import { translate } from "../lib/i18n/lang";
const fallbackSettings: AppSettings = {
  inputDevice: null,
  shortcut: "Ctrl+Space",
  autoPaste: true,
  retentionDays: 30,
  launchOnLogin: false,
  startMinimized: false,
  activeMode: "clean",
  showOverlay: true,
  model: "parakeet",
  streaming: true,
  language: "system",
  modelKeepAliveSecs: 0,
};
export function useAppModel(adapter: AppAdapter, onError: (message: string) => void) {
  const [snapshot, setSnapshot] = useState<AppSnapshot>({
    dictation: { status: "idle" },
    settings: fallbackSettings,
    modelLoading: false,
  });
  const [history, setHistory] = useState<Recording[]>([]);
  const [loading, setLoading] = useState(true);
  const previousStatus = useRef(snapshot.dictation.status);
  const refreshHistory = useCallback(async () => {
    try {
      setHistory(await adapter.listHistory({}));
    } catch (error) {
      onError(translate("useAppModel.loadHistoryError", { error: normalizeError(error) }));
    }
  }, [adapter, onError]);
  useEffect(() => {
    let active = true;
    let stateEventSeen = false;
    const unlisteners: Array<() => void> = [];
    const keepOrDispose = (unlisten: () => void) => {
      if (active) unlisteners.push(unlisten);
      else unlisten();
    };
    Promise.all([adapter.getAppSnapshot(), adapter.listHistory({})])
      .then(([nextSnapshot, nextHistory]) => {
        if (!active) return;
        if (!stateEventSeen) {
          previousStatus.current = nextSnapshot.dictation.status;
          setSnapshot(nextSnapshot);
        }
        setHistory(nextHistory);
      })
      .catch((error) => onError(translate("useAppModel.initError", { error: normalizeError(error) })))
      .finally(() => active && setLoading(false));
    adapter.onState((next) => {
      if (!active) return;
      stateEventSeen = true;
      const wasTerminal =
        ["processing", "pasting"].includes(previousStatus.current) &&
        ["idle", "failed"].includes(next.dictation.status);
      previousStatus.current = next.dictation.status;
      setSnapshot(next);
      if (wasTerminal) void refreshHistory();
    }).then(keepOrDispose);
    adapter.onError(onError).then(keepOrDispose);
    return () => {
      active = false;
      unlisteners.forEach((unlisten) => unlisten());
    };
  }, [adapter, onError, refreshHistory]);
  return { snapshot, setSnapshot, history, refreshHistory, loading };
}
