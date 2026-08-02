import { render, type RenderResult } from "@testing-library/react";
import type { ReactElement, ReactNode } from "react";

import { I18nProvider, type I18nLanguage } from "../lib/i18n";

export interface RenderWithI18nOptions {
  language?: I18nLanguage;
}

export interface RenderWithI18nResult extends RenderResult {
  rerender: (ui: ReactNode, options?: RenderWithI18nOptions) => void;
}

export function renderWithI18n(
  ui: ReactElement,
  options: RenderWithI18nOptions = {},
): RenderWithI18nResult {
  const { language = "pl" } = options;
  const wrap = (element: ReactElement, lang = language) => (
    <I18nProvider language={lang}>{element}</I18nProvider>
  );
  const view = render(wrap(ui, language));
  const { rerender } = view;
  return {
    ...view,
    rerender: (nextUi: ReactNode, nextOptions?: RenderWithI18nOptions) =>
      rerender(wrap(nextUi as ReactElement, nextOptions?.language ?? language)),
  };
}
