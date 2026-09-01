import { act, fireEvent, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, test, vi } from "vitest";

import { overlayWindowSize, RecorderOverlay } from "./RecorderOverlay";
import { adapterStub, settings } from "../../test/fixtures";
import { renderWithI18n } from "../../test/renderWithI18n";
import type { AppSnapshot, AppSettings, DictationState, OverlaySize } from "../../lib/types";

function snapshotFor(state: DictationState, overlaySize: OverlaySize = "mini"): AppSnapshot {
  return {
    dictation: state,
    settings: { ...settings, overlaySize } satisfies AppSettings,
    modelLoading: false,
  };
}

function overlay(state: DictationState, overlaySize: OverlaySize = "mini") {
  let stateListener: ((snapshot: AppSnapshot) => void) | undefined;
  let levelListener: ((level: number) => void) | undefined;
  const adapter = adapterStub({
    getAppSnapshot: async () => snapshotFor(state, overlaySize),
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
  return {
    ...view,
    adapter,
    emitState: (next: DictationState, size: OverlaySize = overlaySize) =>
      stateListener?.(snapshotFor(next, size)),
    emitLevel: (level: number) => levelListener?.(level),
  };
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

  test("mini zostaje mała podczas nagrywania i nie otwiera karty pytania", async () => {
    overlay({ status: "recording", recordingId: "a", audioPath: "a.wav" });
    expect(await screen.findByLabelText("Poziom mikrofonu")).toBeInTheDocument();
    expect(overlayWindowSize("recording", "mini", false)).toEqual({ width: 68, height: 36 });
    expect(screen.queryByRole("alertdialog")).not.toBeInTheDocument();
    expect(screen.queryByText("Przerwać dyktowanie?")).not.toBeInTheDocument();
  });

  test("mini pokazuje stop i anuluj bez rozpychania pigułki", async () => {
    const stopRecording = vi.fn(async () => snapshotFor({ status: "processing", recordingId: "a", audioPath: "a.wav" }));
    const cancelRecording = vi.fn(async () => snapshotFor({ status: "idle" }));
    const view = overlay({ status: "recording", recordingId: "a", audioPath: "a.wav" });
    Object.assign(view.adapter, { stopRecording, cancelRecording });
    await userEvent.hover(document.querySelector(".overlay-pill") as Element);
    await userEvent.click(await screen.findByRole("button", { name: "Zakończ" }));
    expect(stopRecording).toHaveBeenCalledTimes(1);
    view.unmount();

    const again = overlay({ status: "recording", recordingId: "a", audioPath: "a.wav" });
    Object.assign(again.adapter, { stopRecording, cancelRecording });
    await userEvent.hover(document.querySelector(".overlay-pill") as Element);
    await userEvent.click(await screen.findByRole("button", { name: "Anuluj" }));
    expect(cancelRecording).toHaveBeenCalledTimes(1);
  });

  test("przetwarzanie da się anulować z pigułki", async () => {
    const cancelRecording = vi.fn(async () => snapshotFor({ status: "idle" }));
    const view = overlay({ status: "processing", recordingId: "a", audioPath: "a.wav" });
    Object.assign(view.adapter, { cancelRecording });
    await userEvent.hover(document.querySelector(".overlay-pill") as Element);
    await userEvent.click(await screen.findByRole("button", { name: "Anuluj" }));
    expect(cancelRecording).toHaveBeenCalledTimes(1);
  });

  test("długie nagranie pokazuje chip cofnięcia na nakładce, bez karty pytania", async () => {
    overlay({ status: "cancelling", recordingId: "a", audioPath: "a.wav" });

    expect(await screen.findByRole("button", { name: "Przywróć nagranie i przepisz" })).toBeVisible();
    expect(screen.getByText("Cofnij")).toBeVisible();
    expect(screen.queryByRole("alertdialog")).not.toBeInTheDocument();
    expect(screen.queryByText("Przerwać dyktowanie?")).not.toBeInTheDocument();
    expect(overlayWindowSize("cancelling", "mini", false)).toEqual({ width: 168, height: 36 });
  });

  test("chip cofnięcia znika po pięciu sekundach i zamyka anulowanie", async () => {
    vi.useFakeTimers();
    const requestCancel = vi.fn(async () => snapshotFor({ status: "idle" }));
    const view = overlay({ status: "cancelling", recordingId: "a", audioPath: "a.wav" });
    Object.assign(view.adapter, { requestCancel });
    await act(async () => { await Promise.resolve(); });

    act(() => { vi.advanceTimersByTime(5_000); });

    expect(requestCancel).toHaveBeenCalledTimes(1);
    vi.clearAllTimers();
    vi.useRealTimers();
  });

  test("cofnięcie przywraca transkrypcję anulowanego nagrania", async () => {
    const retryTranscription = vi.fn(async () => snapshotFor({ status: "processing", recordingId: "a", audioPath: "a.wav" }));
    const view = overlay({ status: "cancelling", recordingId: "a", audioPath: "a.wav" });
    Object.assign(view.adapter, { retryTranscription });
    await userEvent.click(await screen.findByRole("button", { name: "Przywróć nagranie i przepisz" }));
    expect(retryTranscription).toHaveBeenCalledWith("a");
  });

  test("nieudane zamknięcie cofnięcia pokazuje błąd i zostawia chip", async () => {
    vi.useFakeTimers();
    const requestCancel = vi.fn(async () => { throw new Error("Mikrofon nie odpowiada"); });
    const view = overlay({ status: "cancelling", recordingId: "a", audioPath: "a.wav" });
    Object.assign(view.adapter, { requestCancel });
    await act(async () => { await Promise.resolve(); });

    await act(async () => { vi.advanceTimersByTime(5_100); });
    vi.useRealTimers();

    expect(await screen.findByRole("alert")).toHaveTextContent("Mikrofon nie odpowiada");
    expect(screen.getByText("Cofnij")).toBeVisible();
  });

  test("duża nakładka pokazuje falę, czas, tryb, stop i anuluj", async () => {
    overlay({ status: "recording", recordingId: "a", audioPath: "a.wav" }, "large");

    expect(await screen.findByLabelText("Poziom mikrofonu")).toBeInTheDocument();
    expect(screen.getByLabelText("Czas nagrania")).toBeVisible();
    expect(screen.getByText("Nagrywam")).toBeVisible();
    expect(screen.getByText("Czysty")).toBeVisible();
    expect(screen.getByRole("button", { name: "Zakończ" })).toBeVisible();
    expect(screen.getByRole("button", { name: "Anuluj" })).toBeVisible();
    expect(overlayWindowSize("recording", "large", false)).toEqual({ width: 288, height: 88 });
    expect(document.querySelector(".recorder-overlay--large")).toBeTruthy();
  });

  test("prawy przycisk przełącza rozmiar nakładki od razu", async () => {
    const updateSettings = vi.fn(async (next: AppSettings) => ({ settings: next, warning: null }));
    const view = overlay({ status: "recording", recordingId: "a", audioPath: "a.wav" });
    Object.assign(view.adapter, { updateSettings, getSettings: async () => ({ ...settings, overlaySize: "mini" as const }) });

    fireEvent.contextMenu(await screen.findByLabelText("Poziom mikrofonu"));
    await userEvent.click(await screen.findByRole("menuitem", { name: "Rozszerzona" }));

    expect(updateSettings).toHaveBeenCalledWith(expect.objectContaining({ overlaySize: "large" }));
  });

  test("ponawia zachowane audio po błędzie", async () => {
    const retryTranscription = vi.fn(async () => snapshotFor({ status: "idle" }));
    const view = overlay({ status: "failed", recovery: { recordingId: "failed-1", audioPath: "f.wav" }, error: "Błąd" });
    Object.assign(view.adapter, { retryTranscription });
    await userEvent.click(await screen.findByRole("button", { name: "Ponów transkrypcję" }));
    expect(retryTranscription).toHaveBeenCalledWith("failed-1");
    expect(screen.getByText("Nie udało się")).toBeVisible();
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
      .mockResolvedValueOnce(snapshotFor({ status: "idle" }));
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

    act(() => emitState(snapshotFor({ status: "recording", recordingId: "new", audioPath: "new.wav" })));
    resolveSnapshot(snapshotFor({ status: "idle" }));

    expect(await screen.findByLabelText("Poziom mikrofonu")).toBeVisible();
  });
});
