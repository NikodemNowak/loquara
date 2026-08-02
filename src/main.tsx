import { StrictMode } from "react";
import { createRoot } from "react-dom/client";

import { App } from "./app/App";
import { applyTheme, initialTheme } from "./app/theme";
import { RecorderOverlay } from "./features/dictation/RecorderOverlay";
import { I18nProvider } from "./lib/i18n";
import { translate } from "./lib/i18n";
import { getAdapter } from "./lib/tauri";
import "@fontsource-variable/inter";
import "./app/theme.css";
import "./app/app.css";

const root = document.getElementById("root");

if (!root) {
  throw new Error(translate("errors.noRoot"));
}

const isOverlay = new URLSearchParams(window.location.search).get("window") === "overlay";
document.body.classList.toggle("overlay-window", isOverlay);
if (isOverlay) {
  document.body.setAttribute("data-tauri-drag-region", "");
}
applyTheme(initialTheme());

createRoot(root).render(
  <StrictMode>
    <I18nProvider>
      {isOverlay ? <RecorderOverlay adapter={getAdapter()} /> : <App />}
    </I18nProvider>
  </StrictMode>,
);
