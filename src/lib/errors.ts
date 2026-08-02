import { translate, type TranslationKey } from "./i18n/lang";

/** Known backend sentinel messages that the UI should display localized. */
const backendMessages: Record<string, TranslationKey> = {
  "Previous dictation was interrupted before audio finalization.":
    "errors.interruptedBeforeFinalize",
};

export function normalizeError(error: unknown, fallback = translate("errors.unknown")): string {
  const message = extractMessage(error, fallback);
  const localized = backendMessages[message];
  return localized ? translate(localized) : message;
}

function extractMessage(error: unknown, fallback: string): string {
  if (typeof error === "string" && error.trim()) return error;
  if (error instanceof Error && error.message.trim()) return error.message;
  if (error && typeof error === "object") {
    const direct = (error as { message?: unknown }).message;
    if (typeof direct === "string" && direct.trim()) return direct;
    const nested = (error as { error?: { message?: unknown } }).error?.message;
    if (typeof nested === "string" && nested.trim()) return nested;
    try {
      const serialized = JSON.stringify(error);
      if (serialized && serialized !== "{}") return serialized;
    } catch {
      return fallback;
    }
  }
  if (error !== null && error !== undefined) {
    const primitive = String(error);
    if (primitive && primitive !== "[object Object]") return primitive;
  }
  return fallback;
}
