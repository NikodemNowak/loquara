import { invoke } from "@tauri-apps/api/core";

import type { LanguageChoice } from "./types";

export function primaryLanguage(locale: string): string {
  return locale.split("-")[0].toLowerCase();
}

export async function systemLocale(): Promise<string> {
  try {
    return await invoke<string>("system_locale");
  } catch {
    return primaryLanguage(navigator.language || "en");
  }
}

export async function resolveLanguage(pick: LanguageChoice): Promise<"pl" | "en"> {
  if (pick === "system") {
    const locale = await systemLocale();
    return locale === "pl" ? "pl" : "en";
  }
  return pick;
}
