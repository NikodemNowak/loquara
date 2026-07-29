import type { ThemeChoice } from "../lib/types";

export function applyTheme(choice: ThemeChoice) {
  const dark = choice === "dark" ||
    (choice === "system" && window.matchMedia?.("(prefers-color-scheme: dark)").matches);
  document.documentElement.dataset.theme = dark ? "dark" : "light";
  document.documentElement.dataset.themeChoice = choice;
  localStorage.setItem("mow-theme", choice);
}

export function initialTheme(): ThemeChoice {
  const stored = localStorage.getItem("mow-theme");
  return stored === "light" || stored === "dark" ? stored : "system";
}
