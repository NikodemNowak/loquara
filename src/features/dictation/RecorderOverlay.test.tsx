import { act, fireEvent, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, test, vi } from "vitest";

import { RecorderOverlay } from "./RecorderOverlay";
import { adapterStub } from "../../test/fixtures";
import { renderWithI18n } from "../../test/renderWithI18n";
import type { AppSnapshot, DictationState } from "../../lib/types";

function overlay(state: DictationState) {
  let stateListener: ((snapshot: AppSnapshot) => void) | undefined;
  let levelListener: ((level: number) => void) | undefined;
  const adapter = adapterStub({
    getAppSnapshot: async () => ({ dictation: state, settings: { inputDevice: null, shortcut: "Ctrl+Space", autoPaste: true, retentionDays: 30, launchOnLogin: true, startMinimized: false, activeMode: "clean", showOverlay: true, model: "parakeet", streaming: true, language: "system", modelKeepAliveSecs: 0, pasteMode: "auto" }, modelLoading: false }),
    onState: async (listener) => {
      stateListener = listener;
      return () => undefined;
    },
    onLevel: async (listener) => {
      levelListener = listener;
      return () => undefined;
    },
  });
  const view = renderWithI18n(<RecorderOverlay adapter={adapter} />);
  return { ...view, adapter, emitState: (next: DictationState) => stateListener?.({ dictation: next, settings: { inputDevice: null, shortcut: "Ctrl+Space", autoPaste: true, retentionDays: 30, launchOnLogin: true, startMinimized: false, activeMode: "clean", showOverlay: true, model: "parakeet", streaming: true, language: "system", modelKeepAliveSecs: 0, pasteMode: "auto" }, modelLoading: false }), emitLevel: (level: number) => levelListener?.(level) };
}

describe("nakładka dyktowania", () => {
  test.each([
    [{ status: "pasting", recordingId: "a", audioPath: "a.wav", transcript: "tekst" }, "Wklejam…"],
    [{ status: "failed", recovery: { recordingId: "a", audioPath: "a.wav" }, error: "Błąd" }, "Nie udało się"],
  ] satisfies Array<[DictationState, string]>)("pokazuje stan %s", async (state, label) => {
    overlay(state);
    expect(await screen.findByText(label)).toBeVisible();
  });

  test("przetwarzanie pokazuje sam wskaźnik, bez tekstu", async () => {
    overlay({ status: "processing", recordingId: "a", audioPath: "a.wav" });
    // Nazwany jako przetwarzanie, nie jako poziom mikrofonu - w tym stanie
    // mikrofon nie jest źródłem tego, co widać.
    expect(await screen.findByLabelText("Przepisuję…")).toBeInTheDocument();
    expect(screen.queryByText("Przepisuję…")).not.toBeInTheDocument();
  });

  test("ukrywa okno po powrocie do stanu idle", async () => {
    const hideOverlay = vi.fn(async () => undefined);
    const view = overlay({ status: "idle" });
    Object.assign(view.adapter, { hideOverlay });
    await waitFor(() => expect(hideOverlay).toHaveBeenCalled());
    expect(screen.queryByText("Gotowy")).not.toBeInTheDocument();
  });

  test("nie pokazuje surowego wyniku transkrypcji w overlayu", async () => {
    overlay({ status: "pasting", recordingId: "a", audioPath: "a.wav", transcript: "tekst z modelu" });

    expect(await screen.findByText("Wklejam…")).toBeVisible();
    expect(screen.queryByText("tekst z modelu")).not.toBeInTheDocument();
  });

  test("reaguje na poziom audio i pokazuje falę", async () => {
    vi.useFakeTimers();
    const view = overlay({ status: "recording", recordingId: "a", audioPath: "a.wav" });
    await act(async () => { await Promise.resolve(); });
    expect(screen.getByLabelText("Poziom mikrofonu")).toBeInTheDocument();
    act(() => {
      view.emitLevel(0.8);
      vi.advanceTimersByTime(2100);
    });
    expect(screen.getByLabelText("Poziom mikrofonu")).toHaveAttribute("data-level", "0.80");
    vi.clearAllTimers();
    vi.useRealTimers();
  });

  test("potwierdzenie znika po upływie czasu i wraca do nagrywania", async () => {
    vi.useFakeTimers();
    const requestCancel = vi.fn(async () => ({ dictation: { status: "cancelling", recordingId: "a", audioPath: "a.wav" }, settings: { inputDevice: null, shortcut: "Ctrl+Space", autoPaste: true, retentionDays: 30, launchOnLogin: true, startMinimized: false, activeMode: "clean", showOverlay: true, model: "parakeet", streaming: true, language: "system", modelKeepAliveSecs: 0, pasteMode: "auto" }, modelLoading: false }));
    const view = overlay({ status: "cancelling", recordingId: "a", audioPath: "a.wav" });
    Object.assign(view.adapter, { requestCancel });
    await act(async () => { await Promise.resolve(); });

    act(() => { vi.advanceTimersByTime(10_000); });

    expect(requestCancel).toHaveBeenCalledTimes(1);
    vi.clearAllTimers();
    vi.useRealTimers();
  });

  test("ponawia zachowane audio po błędzie", async () => {
    const retryTranscription = vi.fn(async () => ({ dictation: { status: "idle" }, settings: { inputDevice: null, shortcut: "Ctrl+Space", autoPaste: true, retentionDays: 30, launchOnLogin: true, startMinimized: false, activeMode: "clean", showOverlay: true, model: "parakeet", streaming: true, language: "system", modelKeepAliveSecs: 0, pasteMode: "auto" }, modelLoading: false }));
    const view = overlay({ status: "failed", recovery: { recordingId: "failed-1", audioPath: "f.wav" }, error: "Błąd" });
    Object.assign(view.adapter, { retryTranscription });
    await userEvent.click(await screen.findByRole("button", { name: "Ponów transkrypcję" }));
    expect(retryTranscription).toHaveBeenCalledWith("failed-1");
    expect(screen.getByText("Nie udało się")).toBeVisible();
  });

  test("nieudany powrót z potwierdzenia pokazuje błąd i nie chowa pytania", async () => {
    // Klawisze obsługuje globalny skrót w backendzie, bo pigułka nie ma
    // fokusu. Nakładka wciąż sama wycofuje pytanie po czasie i to jedyna
    // akcja, którą inicjuje - jej błąd musi być widoczny.
    vi.useFakeTimers();
    const requestCancel = vi.fn(async () => { throw new Error("Mikrofon nie odpowiada"); });
    const view = overlay({ status: "cancelling", recordingId: "a", audioPath: "a.wav" });
    Object.assign(view.adapter, { requestCancel });
    await act(async () => { await Promise.resolve(); });

    await act(async () => { vi.advanceTimersByTime(10_100); });
    vi.useRealTimers();

    expect(await screen.findByRole("alert")).toHaveTextContent("Mikrofon nie odpowiada");
    expect(screen.getByText("Przerwać dyktowanie?")).toBeVisible();
  });

  test("pytanie o przerwanie pokazuje obie odpowiedzi z ich klawiszami", async () => {
    // Enter jest jedynym klawiszem, który odrzuca nagranie; Escape, który
    // pytanie otworzył, musi umieć je zamknąć.
    overlay({ status: "cancelling", recordingId: "a", audioPath: "a.wav" });

    expect(await screen.findByText("Przerwać dyktowanie?")).toBeVisible();
    expect(screen.getByText("wyrzuć nagranie")).toBeVisible();
    expect(screen.getByText("nagrywaj dalej")).toBeVisible();
    expect(screen.getByText("Enter")).toBeVisible();
    expect(screen.getByText("Esc")).toBeVisible();
  });

  test("sprząta opóźnione listenery po odmontowaniu", async () => {
    const unlistenState = vi.fn();
    const unlistenLevel = vi.fn();
    let resolveState!: (value: () => void) => void;
    let resolveLevel!: (value: () => void) => void;
    const adapter = adapterStub({
      onState: () => new Promise((resolve) => { resolveState = resolve; }),
      onLevel: () => new Promise((resolve) => { resolveLevel = resolve; }),
    });
    const view = renderWithI18n(<RecorderOverlay adapter={adapter} />);
    view.unmount();
    resolveState(unlistenState);
    resolveLevel(unlistenLevel);
    await waitFor(() => {
      expect(unlistenState).toHaveBeenCalledOnce();
      expect(unlistenLevel).toHaveBeenCalledOnce();
    });
  });

  test.each(["snapshot", "state", "level"] as const)(
    "pokazuje uczciwy błąd, gdy inicjalizacja %s zostaje odrzucona",
    async (failure) => {
      const adapter = adapterStub({
        getAppSnapshot: failure === "snapshot"
          ? async () => { throw { error: { message: "Start niedostępny" } }; }
          : adapterStub().getAppSnapshot,
        onState: failure === "state"
          ? async () => { throw { message: "Listener stanu niedostępny" }; }
          : async () => () => undefined,
        onLevel: failure === "level"
          ? async () => { throw new Error("Listener poziomu niedostępny"); }
          : async () => () => undefined,
      });
      renderWithI18n(<RecorderOverlay adapter={adapter} />);

      expect(screen.queryByText("Gotowy")).not.toBeInTheDocument();
      expect(await screen.findByText("Nie udało się uruchomić")).toBeVisible();
      expect(screen.getByRole("alert")).not.toHaveTextContent("[object Object]");
      expect(screen.getByRole("button", { name: "Ponów inicjalizację" })).toBeEnabled();
    },
  );

  test("ponawia inicjalizację po błędzie", async () => {
    const getAppSnapshot = vi.fn()
      .mockRejectedValueOnce(new Error("Chwilowy błąd"))
      .mockResolvedValueOnce({ dictation: { status: "idle" }, settings: { inputDevice: null, shortcut: "Ctrl+Space", autoPaste: true, retentionDays: 30, launchOnLogin: true, startMinimized: false, activeMode: "clean", showOverlay: true, model: "parakeet", streaming: true, language: "system", modelKeepAliveSecs: 0, pasteMode: "auto" }, modelLoading: false });
    const hideOverlay = vi.fn(async () => undefined);
    renderWithI18n(<RecorderOverlay adapter={adapterStub({ getAppSnapshot, hideOverlay })} />);

    await userEvent.click(await screen.findByRole("button", { name: "Ponów inicjalizację" }));

    await waitFor(() => expect(hideOverlay).toHaveBeenCalled());
    expect(getAppSnapshot).toHaveBeenCalledTimes(2);
  });

  test("nie nadpisuje nowszego zdarzenia spóźnionym snapshotem startowym", async () => {
    let resolveSnapshot!: (value: AppSnapshot) => void;
    let emitState!: (value: AppSnapshot) => void;
    const adapter = adapterStub({
      getAppSnapshot: () => new Promise((resolve) => { resolveSnapshot = resolve; }),
      onState: async (listener) => {
        emitState = listener;
        return () => undefined;
      },
      onLevel: async () => () => undefined,
    });
    renderWithI18n(<RecorderOverlay adapter={adapter} />);
    await waitFor(() => expect(emitState).toBeTypeOf("function"));

    act(() => emitState({
      dictation: { status: "recording", recordingId: "new", audioPath: "new.wav" },
      settings: { inputDevice: null, shortcut: "Ctrl+Space", autoPaste: true, retentionDays: 30, launchOnLogin: true, startMinimized: false, activeMode: "clean", showOverlay: true, model: "parakeet", streaming: true, language: "system", modelKeepAliveSecs: 0, pasteMode: "auto" },
      modelLoading: false,
    }));
    resolveSnapshot({
      dictation: { status: "idle" },
      settings: { inputDevice: null, shortcut: "Ctrl+Space", autoPaste: true, retentionDays: 30, launchOnLogin: true, startMinimized: false, activeMode: "clean", showOverlay: true, model: "parakeet", streaming: true, language: "system", modelKeepAliveSecs: 0, pasteMode: "auto" },
      modelLoading: false,
    });

    expect(await screen.findByLabelText("Poziom mikrofonu")).toBeVisible();
  });
});
