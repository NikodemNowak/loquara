import { defineConfig } from "vitest/config";
import react from "@vitejs/plugin-react";

export default defineConfig({
  plugins: [react()],
  test: {
    environment: "jsdom",
    setupFiles: "./src/test/setup.ts",
    css: true,
    exclude: [
      "**/node_modules/**",
      "**/dist/**",
      "**/.claude/**",
      "**/.worktrees/**",
      "**/.commandcode/**",
      "**/.git/**",
    ],
  },
  // Two pages: the main window, and the dictation pill, which stays loaded
  // for the life of the app and so carries only its own code.
  build: {
    rollupOptions: {
      input: {
        // Relative to the project root, where Vite and Tauri run it.
        main: "index.html",
        overlay: "overlay.html",
      },
    },
  },
  clearScreen: false,
  server: {
    port: 1420,
    strictPort: true,
  },
});
