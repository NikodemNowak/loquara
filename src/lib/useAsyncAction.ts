import { useCallback, useState } from "react";

import type { ToastKind } from "../components/Toast";
import { normalizeError } from "./errors";

type Toast = (message: string, kind: ToastKind) => void;

export function useAsyncAction(onToast: Toast) {
  const [pendingKey, setPendingKey] = useState("");

  const run = useCallback(async <T,>(
    key: string,
    action: () => Promise<T>,
    failureLabel = "Nie udało się wykonać akcji",
  ): Promise<T | undefined> => {
    setPendingKey(key);
    try {
      return await action();
    } catch (error) {
      onToast(`${failureLabel}: ${normalizeError(error)}`, "error");
      return undefined;
    } finally {
      setPendingKey("");
    }
  }, [onToast]);

  return {
    pendingKey,
    busy: Boolean(pendingKey),
    run,
  };
}
