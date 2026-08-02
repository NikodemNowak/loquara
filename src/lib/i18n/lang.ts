import { messages, type TranslationKey } from "./messages";

export type { TranslationKey } from "./messages";

export type I18nLanguage = "pl" | "en";

let currentLang: I18nLanguage = "pl";

export function translate(
  key: TranslationKey,
  params?: Record<string, string | number>,
): string {
  const template = messages[currentLang]?.[key] ?? messages.pl[key] ?? key;
  if (!params) return template;
  return template.replace(/\{(\w+)\}/g, (match, name: string) =>
    params[name] !== undefined ? String(params[name]) : match,
  );
}

export function format(
  template: string,
  params?: Record<string, string | number>,
): string {
  if (!params) return template;
  return template.replace(/\{(\w+)\}/g, (match, name: string) =>
    params[name] !== undefined ? String(params[name]) : match,
  );
}

export function setCurrentLang(lang: I18nLanguage): void {
  currentLang = lang;
  document.documentElement.lang = lang;
  try {
    localStorage.setItem("mow-lang", lang);
  } catch {
    // cache only — ignore storage failures
  }
}

export function cachedLang(): I18nLanguage {
  const raw = localStorage.getItem("mow-lang");
  if (raw === "pl" || raw === "en") return raw;
  const system = (typeof navigator === "object" && navigator.language ? navigator.language : "en")
    .split("-")[0]
    .toLowerCase();
  return system === "pl" ? "pl" : "en";
}

export function cachedPref(): "system" | "pl" | "en" {
  const raw = localStorage.getItem("mow-lang-pref");
  return raw === "pl" || raw === "en" || raw === "system" ? raw : "system";
}

export function resolveLangNow(): I18nLanguage {
  return cachedLang();
}

export function dateLocale(lang: I18nLanguage): string {
  return lang === "en" ? "en-US" : "pl-PL";
}
