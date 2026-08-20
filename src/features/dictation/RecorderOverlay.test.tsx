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
    getAppSnapshot: async () => ({ dictation: state, settings: { inputDevice: null, shortcut: "Ctrl+Space", autoPaste: true, retentionDays: 30, launchOnLogin: true, activeMode: "clean", showOverlay: true, model: "parakeet", streaming: true, theme: "system", language: "system", dictationLanguage: "auto", modelKeepAliveSecs: 0 }, modelLoading: false }),
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
  return { ...view, adapter, emitState: (next: DictationState) => stateListener?.({ dictation: next, settings: { inputDevice: null, shortcut: "Ctrl+Space", autoPaste: true, retentionDays: 30, launchOnLogin: true, activeMode: "clean", showOverlay: true, model: "parakeet", streaming: true, theme: "system", language: "system", dictationLanguage: "auto", modelKeepAliveSecs: 0 }, modelLoading: false }), emitLevel: (level: number) => levelListener?.(level) };
}

describe("nakładka dyktowania", () => {
  test.each([
    [{ status: "pasting", recordingId: "a", audioPath: "a.wav", transcript: "tekst" }, "Wklejam…"],
    [{ status: "failed", recovery: { recordingId: "a", audioPath: "a.wav" }, error: "Błąd" }, "Nie udało się"],
  ] satisfies Array<[DictationState, string]>)("pokazuje stan %s", async (state, label) => {
    overlay(state);
    expect(await screen.findByText(label)).toBeVisible();
  });

  test("przetwarzanie pokazuje samą falę, bez tekstu", async () => {
    overlay({ status: "processing", recordingId: "a", audioPath: "a.wav" });
    expect(await screen.findByLabelText("Poziom mikrofonu")).toBeInTheDocument();
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

  test("Esc podczas nagrywania otwiera potwierdzenie zamiast anulować", async () => {
    const cancelRecording = vi.fn(async () => ({ dictation: { status: "idle" }, settings: { inputDevice: null, shortcut: "Ctrl+Space", autoPaste: true, retentionDays: 30, launchOnLogin: true, activeMode: "clean", showOverlay: true, model: "parakeet", streaming: true, theme: "system", language: "system", dictationLanguage: "auto", modelKeepAliveSecs: 0 }, modelLoading: false }));
    const requestCancel = vi.fn(async () => ({ dictation: { status: "cancelling", recordingId: "a", audioPath: "a.wav" }, settings: { inputDevice: null, shortcut: "Ctrl+Space", autoPaste: true, retentionDays: 30, launchOnLogin: true, activeMode: "clean", showOverlay: true, model: "parakeet", streaming: true, theme: "system", language: "system", dictationLanguage: "auto", modelKeepAliveSecs: 0 }, modelLoading: false }));
    const view = overlay({ status: "recording", recordingId: "a", audioPath: "a.wav" });
    Object.assign(view.adapter, { cancelRecording, requestCancel });
    await act(async () => { await Promise.resolve(); });

    fireEvent.keyDown(window, { key: "Escape" });

    expect(requestCancel).toHaveBeenCalledTimes(1);
    expect(cancelRecording).not.toHaveBeenCalled();
  });

  test("Enter w potwierdzeniu anuluje nagrywanie", async () => {
    const cancelRecording = vi.fn(async () => ({ dictation: { status: "idle" }, settings: { inputDevice: null, shortcut: "Ctrl+Space", autoPaste: true, retentionDays: 30, launchOnLogin: true, activeMode: "clean", showOverlay: true, model: "parakeet", streaming: true, theme: "system", language: "system", dictationLanguage: "auto", modelKeepAliveSecs: 0 }, modelLoading: false }));
    const requestCancel = vi.fn(async () => ({ dictation: { status: "cancelling", recordingId: "a", audioPath: "a.wav" }, settings: { inputDevice: null, shortcut: "Ctrl+Space", autoPaste: true, retentionDays: 30, launchOnLogin: true, activeMode: "clean", showOverlay: true, model: "parakeet", streaming: true, theme: "system", language: "system", dictationLanguage: "auto", modelKeepAliveSecs: 0 }, modelLoading: false }));
    const view = overlay({ status: "cancelling", recordingId: "a", audioPath: "a.wav" });
    Object.assign(view.adapter, { cancelRecording, requestCancel });
    await act(async () => { await Promise.resolve(); });

    fireEvent.keyDown(window, { key: "Enter" });

    expect(cancelRecording).toHaveBeenCalledTimes(1);
    expect(requestCancel).not.toHaveBeenCalled();
  });

  test("Esc w potwierdzeniu wraca do nagrywania", async () => {
    const cancelRecording = vi.fn(async () => ({ dictation: { status: "idle" }, settings: { inputDevice: null, shortcut: "Ctrl+Space", autoPaste: true, retentionDays: 30, launchOnLogin: true, activeMode: "clean", showOverlay: true, model: "parakeet", streaming: true, theme: "system", language: "system", dictationLanguage: "auto", modelKeepAliveSecs: 0 }, modelLoading: false }));
    const requestCancel = vi.fn(async () => ({ dictation: { status: "cancelling", recordingId: "a", audioPath: "a.wav" }, settings: { inputDevice: null, shortcut: "Ctrl+Space", autoPaste: true, retentionDays: 30, launchOnLogin: true, activeMode: "clean", showOverlay: true, model: "parakeet", streaming: true, theme: "system", language: "system", dictationLanguage: "auto", modelKeepAliveSecs: 0 }, modelLoading: false }));
    const view = overlay({ status: "cancelling", recordingId: "a", audioPath: "a.wav" });
    Object.assign(view.adapter, { cancelRecording, requestCancel });
    await act(async () => { await Promise.resolve(); });

    fireEvent.keyDown(window, { key: "Escape" });

    expect(requestCancel).toHaveBeenCalledTimes(1);
    expect(cancelRecording).not.toHaveBeenCalled();
  });

  test("potwierdzenie znika po upływie czasu i wraca do nagrywania", async () => {
    vi.useFakeTimers();
    const requestCancel = vi.fn(async () => ({ dictation: { status: "cancelling", recordingId: "a", audioPath: "a.wav" }, settings: { inputDevice: null, shortcut: "Ctrl+Space", autoPaste: true, retentionDays: 30, launchOnLogin: true, activeMode: "clean", showOverlay: true, model: "parakeet", streaming: true, theme: "system", language: "system", dictationLanguage: "auto", modelKeepAliveSecs: 0 }, modelLoading: false }));
    const view = overlay({ status: "cancelling", recordingId: "a", audioPath: "a.wav" });
    Object.assign(view.adapter, { requestCancel });
    await act(async () => { await Promise.resolve(); });

    act(() => { vi.advanceTimersByTime(10_000); });

    expect(requestCancel).toHaveBeenCalledTimes(1);
    vi.clearAllTimers();
    vi.useRealTimers();
  });

  test("ponawia zachowane audio po błędzie", async () => {
    const retryTranscription = vi.fn(async () => ({ dictation: { status: "idle" }, settings: { inputDevice: null, shortcut: "Ctrl+Space", autoPaste: true, retentionDays: 30, launchOnLogin: true, activeMode: "clean", showOverlay: true, model: "parakeet", streaming: true, theme: "system", language: "system", dictationLanguage: "auto", modelKeepAliveSecs: 0 }, modelLoading: false }));
    const view = overlay({ status: "failed", recovery: { recordingId: "failed-1", audioPath: "f.wav" }, error: "Błąd" });
    Object.assign(view.adapter, { retryTranscription });
    await userEvent.click(await screen.findByRole("button", { name: "Ponów transkrypcję" }));
    expect(retryTranscription).toHaveBeenCalledWith("failed-1");
    expect(screen.getByText(/audio jest bezpiecznie zapisane/i)).toBeVisible();
  });

  test("pokazuje błąd anulowania i zachowuje stan", async () => {
    const cancelRecording = vi.fn(async () => { throw new Error("Mikrofon nie odpowiada"); });
    const view = overlay({ status: "cancelling", recordingId: "a", audioPath: "a.wav" });
    Object.assign(view.adapter, { cancelRecording });

    await screen.findByText("Anulować nagrywanie?");
    fireEvent.keyDown(window, { key: "Enter" });

    expect(await screen.findByRole("alert")).toHaveTextContent("Mikrofon nie odpowiada");
    expect(screen.getByText("Anulować nagrywanie?")).toBeVisible();
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
      .mockResolvedValueOnce({ dictation: { status: "idle" }, settings: { inputDevice: null, shortcut: "Ctrl+Space", autoPaste: true, retentionDays: 30, launchOnLogin: true, activeMode: "clean", showOverlay: true, model: "parakeet", streaming: true, theme: "system", language: "system", dictationLanguage: "auto", modelKeepAliveSecs: 0 }, modelLoading: false });
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
      settings: { inputDevice: null, shortcut: "Ctrl+Space", autoPaste: true, retentionDays: 30, launchOnLogin: true, activeMode: "clean", showOverlay: true, model: "parakeet", streaming: true, theme: "system", language: "system", dictationLanguage: "auto", modelKeepAliveSecs: 0 },
      modelLoading: false,
    }));
    resolveSnapshot({
      dictation: { status: "idle" },
      settings: { inputDevice: null, shortcut: "Ctrl+Space", autoPaste: true, retentionDays: 30, launchOnLogin: true, activeMode: "clean", showOverlay: true, model: "parakeet", streaming: true, theme: "system", language: "system", dictationLanguage: "auto", modelKeepAliveSecs: 0 },
      modelLoading: false,
    });

    expect(await screen.findByLabelText("Poziom mikrofonu")).toBeVisible();
  });
});
