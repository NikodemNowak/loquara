import { useCallback, useState } from "react";

import type { ToastKind } from "../components/Toast";
import { normalizeError } from "./errors";
import { translate, type TranslationKey } from "./i18n/lang";

type Toast = (message: string, kind: ToastKind) => void;

export function useAsyncAction(onToast: Toast) {
  const [pendingKey, setPendingKey] = useState("");

  const run = useCallback(async <T,>(
    key: string,
    action: () => Promise<T>,
    failureLabel: TranslationKey = "common.error.action",
  ): Promise<T | undefined> => {
    setPendingKey(key);
    try {
      return await action();
    } catch (error) {
      onToast(translate(failureLabel, { error: normalizeError(error) }), "error");
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
