import { StrictMode } from "react";
import { createRoot } from "react-dom/client";

import { App } from "./app/App";
import { ErrorBoundary } from "./components/ErrorBoundary";
import { RecorderOverlay } from "./features/dictation/RecorderOverlay";
import { I18nProvider } from "./lib/i18n";
import { translate } from "./lib/i18n";
import { getAdapter } from "./lib/tauri";
import "./app/theme.css";
import "./app/app.css";

const root = document.getElementById("root");

if (!root) {
  throw new Error(translate("errors.noRoot"));
}

/* The webview offers its own context menu — Save as, Reload, Print — which
 * belongs to a browser, not to a dictation app. Editable fields keep theirs,
 * because cut/copy/paste there is exactly what a native text field offers. */
document.addEventListener("contextmenu", (event) => {
  const target = event.target as HTMLElement | null;
  if (target?.closest("input, textarea, [contenteditable='true']")) return;
  event.preventDefault();
});

/* The same reasoning for browser-level shortcuts that have no meaning here:
 * reloading or opening dev tools is not something this app offers. */
document.addEventListener("keydown", (event) => {
  const key = event.key.toLowerCase();
  const reload = key === "f5" || ((event.ctrlKey || event.metaKey) && key === "r");
  const find = (event.ctrlKey || event.metaKey) && key === "f";
  const print = (event.ctrlKey || event.metaKey) && key === "p";
  if (reload || find || print) event.preventDefault();
});

const isOverlay = new URLSearchParams(window.location.search).get("window") === "overlay";
document.body.classList.toggle("overlay-window", isOverlay);

createRoot(root).render(
  <StrictMode>
    <I18nProvider>
      <ErrorBoundary>
        {isOverlay ? <RecorderOverlay adapter={getAdapter()} /> : <App />}
      </ErrorBoundary>
    </I18nProvider>
  </StrictMode>,
);
