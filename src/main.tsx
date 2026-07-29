import { StrictMode } from "react";
import { createRoot } from "react-dom/client";

import { App } from "./app/App";
import { applyTheme, initialTheme } from "./app/theme";
import { RecorderOverlay } from "./features/dictation/RecorderOverlay";
import { getAdapter } from "./lib/tauri";
import "./app/theme.css";
import "./app/app.css";

const root = document.getElementById("root");

if (!root) {
  throw new Error("Nie znaleziono elementu głównego aplikacji.");
}

const isOverlay = new URLSearchParams(window.location.search).get("window") === "overlay";
document.body.classList.toggle("overlay-window", isOverlay);
applyTheme(initialTheme());

createRoot(root).render(
  <StrictMode>
    {isOverlay ? <RecorderOverlay adapter={getAdapter()} /> : <App />}
  </StrictMode>,
);
