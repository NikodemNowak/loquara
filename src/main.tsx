import { StrictMode } from "react";
import { createRoot } from "react-dom/client";

const root = document.getElementById("root");

if (!root) {
  throw new Error("Nie znaleziono elementu głównego aplikacji.");
}

createRoot(root).render(
  <StrictMode>
    <main>Mów</main>
  </StrictMode>,
);
