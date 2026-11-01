import { act, screen, waitFor, within } from "@testing-library/react";
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
  test("stopka śledzi wszystkie fazy dyktowania bez fałszywej gotowości", async () => {
    let emitState!: (value: AppSnapshot) => void;
    renderApp(adapterStub({ onState: async (listener) => { emitState = listener; return () => undefined; } }));
    const engine = await screen.findByRole("button", { name: /Silnik: Parakeet/ });
    expect(engine).not.toHaveTextContent("Gotowy");
    expect(engine).toHaveAttribute("title", expect.stringContaining("Gotowy"));
    const cases: Array<[AppSnapshot["dictation"], string]> = [
      [{ status: "recording", recordingId: "r", audioPath: "r.wav" }, "Nagrywam"],
      [{ status: "cancelling", recordingId: "r", audioPath: "r.wav" }, "Anulowanie"],
      [{ status: "processing", recordingId: "r", audioPath: "r.wav" }, "Przetwarzam"],
      [{ status: "pasting", recordingId: "r", audioPath: "r.wav", transcript: "Tekst" }, "Wklejam"],
      [{ status: "failed", recovery: { recordingId: "r", audioPath: "r.wav" }, error: "Błąd" }, "Nie udało się"],
    ];
    for (const [dictation, label] of cases) {
      await act(async () => emitState({ ...snapshot, dictation, download: { model: "other", downloadedBytes: 50, totalBytes: 100 } }));
      expect(engine).toHaveAttribute("title", expect.stringContaining(label));
      expect(engine).toHaveAccessibleName(expect.stringContaining(label));
      expect(engine).not.toHaveTextContent(label);
      expect(engine).not.toHaveTextContent("Gotowy");
      expect(engine).not.toHaveTextContent("Pobieram");
    }
    await act(async () => emitState(snapshot));
    expect(engine).not.toHaveTextContent("Gotowy");
    expect(engine).toHaveAttribute("title", expect.stringContaining("Gotowy"));
  });

  test("nazwa i gotowość modelu przychodzą ze snapshotu, nie z osobnego zapytania", async () => {
    // Osobne zapytanie potrafi nie dojść — np. gdy okno zdąży o nie poprosić,
    // zanim backend jest gotowy. Wtedy panel twierdził, że modelu nie ma,
    // choć model był na dysku i dyktowanie działało.
    const adapter = adapterStub({
      getModelStatus: async () => { throw new Error("zapytanie nie doszło"); },
      listModels: async () => { throw new Error("zapytanie nie doszło"); },
    });

    renderApp(adapter);

    expect((await screen.findAllByText("Parakeet TDT 0.6B v3"))[0]).toBeVisible();
    expect(screen.queryByText("Gotowy")).not.toBeInTheDocument();
    expect(await screen.findByRole("heading", { name: "Gotowe do dyktowania" })).toBeVisible();
    expect(screen.queryByRole("heading", { name: "Brak modelu" })).toBeNull();
  });

  test("przechodzi klawiaturą między wszystkimi sekcjami", async () => {
    const user = userEvent.setup();
    renderApp();

    expect(await screen.findByRole("heading", { name: "Gotowe do dyktowania" })).toBeVisible();
    const historyTab = screen.getByRole("button", { name: "Historia" });
    historyTab.focus();
    await user.keyboard("{Enter}");
    expect(await screen.findByRole("heading", { name: "Historia" })).toBeVisible();
    expect(historyTab).toHaveAttribute("aria-current", "page");
    await user.click(screen.getByRole("button", { name: "Ustawienia" }));
    expect(await screen.findByRole("heading", { name: "Ustawienia" })).toBeVisible();
    await user.click(screen.getByRole("button", { name: "Słownik" }));
    expect(await screen.findByRole("heading", { name: "Słownik" })).toBeVisible();
    expect(await screen.findByRole("button", { name: "Dodaj zamianę" })).toBeVisible();
    await user.click(screen.getByRole("button", { name: "Tryby" }));
    expect(await screen.findByRole("heading", { name: "Tryby" })).toBeVisible();
  });

  test("błąd dostarczenia zostaje widoczny przy zmianie strony i jest zamykany przez właściciela", async () => {
    const user = userEvent.setup();
    const dismissOverlayNotice = vi.fn(async () => snapshot);
    renderApp(adapterStub({ getAppSnapshot: async () => ({ ...snapshot, overlayNotice: { key: "notice-key", generation: 7, recordingId: "r", kind: "deliveryFailed", detail: "Schowek jest zajęty." } }), dismissOverlayNotice }));
    const alert = await screen.findByRole("alert");
    expect(alert).toHaveTextContent("Schowek jest zajęty.");
    await user.click(screen.getByRole("button", { name: "Historia" }));
    expect(alert).toBeVisible();
    await user.click(within(alert).getByRole("button"));
    expect(dismissOverlayNotice).toHaveBeenCalledWith("notice-key", 7);
    await waitFor(() => expect(screen.queryByText("Schowek jest zajęty.")).not.toBeInTheDocument());
  });

  test("spóźnione zamknięcie powiadomienia nie nadpisuje nowego nagrania", async () => {
    const user = userEvent.setup();
    let emitState!: (value: AppSnapshot) => void;
    let resolveDismiss!: (value: AppSnapshot) => void;
    renderApp(adapterStub({
      getAppSnapshot: async () => ({ ...snapshot, overlayNotice: { key: "old", generation: 1, recordingId: "r", kind: "deliveryFailed", detail: "Stary błąd" } }),
      onState: async (listener) => { emitState = listener; return () => undefined; },
      dismissOverlayNotice: () => new Promise((resolve) => { resolveDismiss = resolve; }),
    }));
    await user.click(within(await screen.findByRole("alert")).getByRole("button"));
    await act(async () => emitState({ ...snapshot, dictation: { status: "recording", recordingId: "new", audioPath: "new.wav" } }));
    await act(async () => resolveDismiss(snapshot));
    expect(screen.getByRole("heading", { name: "Nagrywam" })).toBeVisible();
    expect(screen.queryByText("Stary błąd")).not.toBeInTheDocument();
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

    expect(await screen.findByRole("heading", { name: "Nagrywam" })).toBeVisible();
    expect(getAppSnapshot).toHaveBeenCalledOnce();
  });

  test("kliknięcie ostatniego nagrania od razu otwiera tę transkrypcję", async () => {
    const user = userEvent.setup();
    renderApp();

    expect(await screen.findByRole("heading", { name: "Gotowe do dyktowania" })).toBeVisible();
    await user.click(screen.getByRole("button", { name: /Przygotuj proszę podsumowanie/ }));

    expect(screen.getByRole("heading", { name: "Historia" })).toBeVisible();
    const inspector = screen.getByRole("complementary", { name: "Szczegóły nagrania" });
    expect(inspector).toHaveTextContent("Przygotuj proszę podsumowanie spotkania.");
    expect(screen.getByRole("button", { name: /Przygotuj proszę/ })).toHaveAttribute("aria-current", "true");
  });

  test("renderuje UI po angielsku i ustawia atrybut lang dokumentu", async () => {
    const adapter = adapterStub({
      getAppSnapshot: async () => ({
        ...snapshot,
        settings: { ...snapshot.settings, language: "en" },
      }),
    });
    renderWithI18n(<App adapter={adapter} />, { language: "en" });

    expect(await screen.findByRole("heading", { name: "Ready to dictate" })).toBeVisible();
    expect(document.documentElement.lang).toBe("en");
  });
});
