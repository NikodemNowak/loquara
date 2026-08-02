import {
  createContext,
  useCallback,
  useContext,
  useEffect,
  useMemo,
  useState,
  type ReactNode,
} from "react";

import { resolveLanguage } from "../locale";
import type { LanguageChoice } from "../types";
import { messages, type TranslationKey } from "./messages";
import {
  cachedPref,
  format,
  resolveLangNow,
  setCurrentLang,
  type I18nLanguage,
} from "./lang";

interface I18nContextValue {
  lang: I18nLanguage;
  setLang: (lang: I18nLanguage) => void;
  t: (key: TranslationKey, params?: Record<string, string | number>) => string;
}

interface I18nProviderProps {
  /** When provided, auto-resolution is skipped entirely and this language is used. */
  language?: I18nLanguage;
  children: ReactNode;
}

const I18nContext = createContext<I18nContextValue | null>(null);

export function I18nProvider({ language, children }: I18nProviderProps) {
  const [lang, setLangState] = useState<I18nLanguage>(
    () => language ?? resolveLangNow(),
  );

  useEffect(() => {
    setCurrentLang(lang);
  }, [lang]);

  useEffect(() => {
    if (language) return;
    let active = true;
    void (async () => {
      const resolved = await resolveLanguage(cachedPref());
      if (active) setLangState(resolved);
    })();
    return () => {
      active = false;
    };
  }, [language]);

  const setLang = useCallback((next: I18nLanguage) => {
    if (language) return;
    setCurrentLang(next);
    setLangState(next);
  }, [language]);

  const t = useCallback(
    (key: TranslationKey, params?: Record<string, string | number>) =>
      format(messages[lang]?.[key] ?? messages.pl[key] ?? key, params),
    [lang],
  );

  const value = useMemo(() => ({ lang, setLang, t }), [lang, setLang, t]);

  return <I18nContext.Provider value={value}>{children}</I18nContext.Provider>;
}

export function useI18n(): { t: I18nContextValue["t"]; lang: I18nLanguage; applyPreference: (pick: LanguageChoice) => void } {
  const context = useContext(I18nContext);
  if (!context) {
    throw new Error("useI18n must be used within an I18nProvider");
  }
  const { lang, setLang, t } = context;
  const applyPreference = useCallback(
    (pick: LanguageChoice) => {
      localStorage.setItem("mow-lang-pref", pick);
      void resolveLanguage(pick).then((resolved) => setLang(resolved));
    },
    [setLang],
  );
  return { t, lang, applyPreference };
}
