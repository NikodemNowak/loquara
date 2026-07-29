import { act, fireEvent, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, test, vi } from "vitest";

import { RecorderOverlay } from "./RecorderOverlay";
import { adapterStub } from "../../test/fixtures";
import type { AppSnapshot, DictationState } from "../../lib/types";

function overlay(state: DictationState) {
  let stateListener: ((snapshot: AppSnapshot) => void) | undefined;
  let levelListener: ((level: number) => void) | undefined;
  const adapter = adapterStub({
    getAppSnapshot: async () => ({ dictation: state, settings: { inputDevice: null, shortcut: "Ctrl+Space", autoPaste: true, retentionDays: 30, launchOnLogin: true, activeMode: "clean" } }),
    onState: async (listener) => {
      stateListener = listener;
      return () => undefined;
    },
    onLevel: async (listener) => {
      levelListener = listener;
      return () => undefined;
    },
  });
  const view = render(<RecorderOverlay adapter={adapter} />);
  return { ...view, adapter, emitState: (next: DictationState) => stateListener?.({ dictation: next, settings: { inputDevice: null, shortcut: "Ctrl+Space", autoPaste: true, retentionDays: 30, launchOnLogin: true, activeMode: "clean" } }), emitLevel: (level: number) => levelListener?.(level) };
}

describe("nakładka dyktowania", () => {
  test.each([
    [{ status: "idle" }, "Gotowy"],
    [{ status: "processing", recordingId: "a", audioPath: "a.wav" }, "Przepisuję…"],
    [{ status: "pasting", recordingId: "a", audioPath: "a.wav", transcript: "tekst" }, "Wklejam…"],
    [{ status: "failed", recovery: { recordingId: "a", audioPath: "a.wav" }, error: "Błąd" }, "Nie udało się"],
  ] satisfies Array<[DictationState, string]>)("pokazuje stan %s", async (state, label) => {
    overlay(state);
    expect(await screen.findByText(label)).toBeVisible();
  });

  test("reaguje na poziom audio, pokazuje licznik i zatrzymuje lub anuluje", async () => {
    vi.useFakeTimers();
    const stopRecording = vi.fn(async () => ({ dictation: { status: "idle" }, settings: { inputDevice: null, shortcut: "Ctrl+Space", autoPaste: true, retentionDays: 30, launchOnLogin: true, activeMode: "clean" } }));
    const cancelRecording = vi.fn(async () => ({ dictation: { status: "idle" }, settings: { inputDevice: null, shortcut: "Ctrl+Space", autoPaste: true, retentionDays: 30, launchOnLogin: true, activeMode: "clean" } }));
    const view = overlay({ status: "recording", recordingId: "a", audioPath: "a.wav" });
    Object.assign(view.adapter, { stopRecording, cancelRecording });
    await act(async () => { await Promise.resolve(); });
    expect(screen.getByText("00:00")).toBeVisible();
    act(() => {
      view.emitLevel(0.8);
      vi.advanceTimersByTime(2100);
    });
    expect(screen.getByText("00:02")).toBeVisible();
    expect(screen.getByLabelText("Poziom mikrofonu")).toHaveAttribute("data-level", "0.80");
    fireEvent.click(screen.getByRole("button", { name: "Zatrzymaj nagrywanie" }));
    await act(async () => { await Promise.resolve(); });
    fireEvent.click(screen.getByRole("button", { name: "Anuluj nagrywanie" }));
    expect(stopRecording).toHaveBeenCalled();
    expect(cancelRecording).toHaveBeenCalled();
    vi.clearAllTimers();
    vi.useRealTimers();
  });

  test("ponawia zachowane audio po błędzie", async () => {
    const retryTranscription = vi.fn(async () => ({ dictation: { status: "idle" }, settings: { inputDevice: null, shortcut: "Ctrl+Space", autoPaste: true, retentionDays: 30, launchOnLogin: true, activeMode: "clean" } }));
    const view = overlay({ status: "failed", recovery: { recordingId: "failed-1", audioPath: "f.wav" }, error: "Błąd" });
    Object.assign(view.adapter, { retryTranscription });
    await userEvent.click(await screen.findByRole("button", { name: "Ponów transkrypcję" }));
    expect(retryTranscription).toHaveBeenCalledWith("failed-1");
    expect(screen.getByText(/audio jest bezpiecznie zapisane/i)).toBeVisible();
  });

  test("pokazuje błąd zatrzymania, zachowuje stan i odblokowuje akcje", async () => {
    const stopRecording = vi.fn(async () => { throw new Error("Mikrofon nie odpowiada"); });
    const view = overlay({ status: "recording", recordingId: "a", audioPath: "a.wav" });
    Object.assign(view.adapter, { stopRecording });

    await userEvent.click(await screen.findByRole("button", { name: "Zatrzymaj nagrywanie" }));

    expect(await screen.findByRole("alert")).toHaveTextContent("Mikrofon nie odpowiada");
    expect(screen.getByText("00:00")).toBeVisible();
    expect(screen.getByRole("button", { name: "Zatrzymaj nagrywanie" })).toBeEnabled();
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
    const view = render(<RecorderOverlay adapter={adapter} />);
    view.unmount();
    resolveState(unlistenState);
    resolveLevel(unlistenLevel);
    await waitFor(() => {
      expect(unlistenState).toHaveBeenCalledOnce();
      expect(unlistenLevel).toHaveBeenCalledOnce();
    });
  });
});
