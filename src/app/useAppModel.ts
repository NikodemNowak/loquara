import { useCallback, useEffect, useRef, useState } from "react";

import type { AppAdapter } from "../lib/tauri";
import type { AppSettings, AppSnapshot, Recording } from "../lib/types";
import { normalizeError } from "../lib/errors";

const fallbackSettings: AppSettings = {
  inputDevice: null,
  shortcut: "Ctrl+Space",
  autoPaste: true,
  retentionDays: 30,
  launchOnLogin: true,
  activeMode: "clean",
};

export function useAppModel(adapter: AppAdapter, onError: (message: string) => void) {
  const [snapshot, setSnapshot] = useState<AppSnapshot>({
    dictation: { status: "idle" },
    settings: fallbackSettings,
  });
  const [history, setHistory] = useState<Recording[]>([]);
  const [loading, setLoading] = useState(true);
  const previousStatus = useRef(snapshot.dictation.status);

  const refreshHistory = useCallback(async () => {
    try {
      setHistory(await adapter.listHistory({}));
    } catch (error) {
      onError(`Nie udało się wczytać historii: ${normalizeError(error)}`);
    }
  }, [adapter, onError]);

  useEffect(() => {
    let active = true;
    const unlisteners: Array<() => void> = [];
    const keepOrDispose = (unlisten: () => void) => {
      if (active) unlisteners.push(unlisten);
      else unlisten();
    };
    Promise.all([adapter.getAppSnapshot(), adapter.listHistory({})])
      .then(([nextSnapshot, nextHistory]) => {
        if (!active) return;
        setSnapshot(nextSnapshot);
        setHistory(nextHistory);
      })
      .catch((error) => onError(`Nie udało się uruchomić widoku: ${normalizeError(error)}`))
      .finally(() => active && setLoading(false));
    adapter.onState((next) => {
      if (!active) return;
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
