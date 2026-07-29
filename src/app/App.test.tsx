import { act, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, test, vi } from "vitest";

import { App } from "./App";
import { adapterStub, snapshot } from "../test/fixtures";
import type { AppSnapshot } from "../lib/types";

describe("główna nawigacja", () => {
  test("przechodzi klawiaturą między wszystkimi sekcjami", async () => {
    const user = userEvent.setup();
    render(<App adapter={adapterStub()} />);

    expect(await screen.findByRole("heading", { name: /dzień dobry/i })).toBeVisible();
    await user.click(screen.getByRole("button", { name: "Historia" }));
    expect(await screen.findByRole("heading", { name: "Historia" })).toBeVisible();
    await user.click(screen.getByRole("button", { name: "Słownik" }));
    expect(await screen.findByRole("heading", { name: "Słownik" })).toBeVisible();
    await user.click(screen.getByRole("button", { name: "Tryby" }));
    expect(await screen.findByRole("heading", { name: "Tryby" })).toBeVisible();
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

    const view = render(<App adapter={adapter} />);
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
    render(<App adapter={adapter} />);
    await waitFor(() => expect(emitState).toBeTypeOf("function"));

    act(() => emitState({
      ...snapshot,
      dictation: { status: "recording", recordingId: "new", audioPath: "new.wav" },
    }));
    resolveSnapshot(snapshot);

    expect(await screen.findByRole("button", { name: /Zatrzymaj nagrywanie/ })).toBeVisible();
    expect(getAppSnapshot).toHaveBeenCalledOnce();
  });
});
