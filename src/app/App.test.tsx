import { act, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, test, vi } from "vitest";

import { App } from "./App";
import { adapterStub, snapshot } from "../test/fixtures";
import { renderWithI18n } from "../test/renderWithI18n";
import type { AppSnapshot } from "../lib/types";

function renderApp(adapter = adapterStub()) {
  return renderWithI18n(<App adapter={adapter} />);
}

describe("główna nawigacja", () => {
  test("przechodzi klawiaturą między wszystkimi sekcjami", async () => {
    const user = userEvent.setup();
    renderApp();

    expect(await screen.findByRole("heading", { name: /dyktowanie/i })).toBeVisible();
    await user.click(screen.getByRole("button", { name: "Historia" }));
    expect(await screen.findByRole("heading", { name: "Historia" })).toBeVisible();
    await user.click(screen.getByRole("button", { name: "Ustawienia" }));
    expect(await screen.findByRole("heading", { name: "Ustawienia" })).toBeVisible();
  });

  test("sprząta listenery, które zarejestrowały się już po odmontowaniu", async () => {
    const unlistenState = vi.fn();
    const unlistenError = vi.fn();
    let resolveState!: (value: () => void) => void;
    let resolveError!: (value: () => void) => void;
    const adapter = adapterStub({
      onState: () => new Promise((resolve) => { resolveState = resolve; }),
      onError: () => new Promise((resolve) => { resolveError = resolve; }),
    });

    const view = renderApp(adapter);
    view.unmount();
    resolveState(unlistenState);
    resolveError(unlistenError);

    await waitFor(() => {
      expect(unlistenState).toHaveBeenCalledOnce();
      expect(unlistenError).toHaveBeenCalledOnce();
    });
  });

  test("nie nadpisuje nowszego zdarzenia spóźnionym snapshotem startowym", async () => {
    let resolveSnapshot!: (value: AppSnapshot) => void;
    let emitState!: (value: AppSnapshot) => void;
    const getAppSnapshot = vi.fn(() => new Promise<AppSnapshot>((resolve) => {
      resolveSnapshot = resolve;
    }));
    const adapter = adapterStub({
      getAppSnapshot,
      listHistory: async () => [],
      onState: async (listener) => {
        emitState = listener;
        return () => undefined;
      },
    });
    renderApp(adapter);
    await waitFor(() => expect(emitState).toBeTypeOf("function"));

    act(() => emitState({
      ...snapshot,
      dictation: { status: "recording", recordingId: "new", audioPath: "new.wav" },
    }));
    resolveSnapshot(snapshot);

    expect(await screen.findByRole("button", { name: /Zatrzymaj nagrywanie/ })).toBeVisible();
    expect(getAppSnapshot).toHaveBeenCalledOnce();
  });

  test("renderuje UI po angielsku i ustawia atrybut lang dokumentu", async () => {
    const adapter = adapterStub({
      getAppSnapshot: async () => ({
        ...snapshot,
        settings: { ...snapshot.settings, language: "en" },
      }),
    });
    renderWithI18n(<App adapter={adapter} />, { language: "en" });

    expect(await screen.findByRole("heading", { name: "Dictation" })).toBeVisible();
    expect(document.documentElement.lang).toBe("en");
  });
});
