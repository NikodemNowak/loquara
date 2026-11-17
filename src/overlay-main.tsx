/* The dictation pill's own entry point. It runs in a window of its own for as
 * long as the app runs, so it loads the overlay and nothing of the main
 * window: no pages, no settings, no history. */
import { StrictMode } from "react";
import { createRoot } from "react-dom/client";

import { ErrorBoundary } from "./components/ErrorBoundary";
import { RecorderOverlay } from "./features/dictation/RecorderOverlay";
import { I18nProvider, translate } from "./lib/i18n";
import { getAdapter } from "./lib/tauri";
import "./app/theme.css";
import "./app/app.css";

const root = document.getElementById("root");

if (!root) {
  throw new Error(translate("errors.noRoot"));
}

/* No browser context menu or browser shortcuts on the pill either. */
document.addEventListener("contextmenu", (event) => {
  const target = event.target as HTMLElement | null;
  if (target?.closest("input, textarea, [contenteditable='true']")) return;
  event.preventDefault();
});
document.addEventListener("keydown", (event) => {
  const key = event.key.toLowerCase();
  const reload = key === "f5" || ((event.ctrlKey || event.metaKey) && key === "r");
  const find = (event.ctrlKey || event.metaKey) && key === "f";
  const print = (event.ctrlKey || event.metaKey) && key === "p";
  if (reload || find || print) event.preventDefault();
});

createRoot(root).render(
  <StrictMode>
    <I18nProvider>
      <ErrorBoundary>
        <RecorderOverlay adapter={getAdapter()} />
      </ErrorBoundary>
    </I18nProvider>
  </StrictMode>,
);
