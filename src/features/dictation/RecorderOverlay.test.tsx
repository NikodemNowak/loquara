import { act, fireEvent, screen, waitFor } from "@testing-library/react";
import { describe, expect, test, vi } from "vitest";
import { clampOverlayPosition, overlayWindowSize, RecorderOverlay } from "./RecorderOverlay";
import { createBrowserAdapter } from "../../lib/tauri";
import { translate as t } from "../../lib/i18n/lang";
import { adapterStub, settings } from "../../test/fixtures";
import { renderWithI18n } from "../../test/renderWithI18n";
import type { AppSnapshot, DictationState, OverlayNotice, OverlaySize } from "../../lib/types";

function snapshotFor(state: DictationState, overlaySize: OverlaySize = "mini"): AppSnapshot {
  return { dictation: state, settings: { ...settings, overlaySize }, modelLoading: false };
}
function overlay(state: DictationState, overlaySize: OverlaySize = "mini") {
  let stateListener: ((snapshot: AppSnapshot) => void) | undefined;
  let levelListener: ((level: number) => void) | undefined;
  let errorListener: ((message: string) => void) | undefined;
  const adapter = adapterStub({
    getAppSnapshot: async () => snapshotFor(state, overlaySize),
    onState: async (listener) => { stateListener = listener; return () => undefined; },
    onLevel: async (listener) => { levelListener = listener; return () => undefined; },
    onError: async (listener) => { errorListener = listener; return () => undefined; },
  });
  const view = renderWithI18n(<RecorderOverlay adapter={adapter} />);
  return { ...view, adapter,
    emitState: (next: DictationState) => stateListener?.(snapshotFor(next, overlaySize)),
    emitLevel: (level: number) => levelListener?.(level),
    emitError: (message: string) => errorListener?.(message),
  };
}
const take = { recordingId: "take", audioPath: "take.wav" };

describe("fixed compact pill", () => {
  const states: DictationState[] = [
    { status: "idle" }, { status: "recording", ...take }, { status: "processing", ...take },
    { status: "pasting", ...take, transcript: "Private final words" },
    { status: "cancelling", ...take }, { status: "failed", recovery: take, error: "Failure details" },
  ];
  test.each(states.flatMap((state) => (["mini", "large"] as const).map((size) => [state, size] as const)))(
    "state %s and legacy %s keep 136x36", async (state, size) => {
      overlay(state, size);
      await act(async () => { await Promise.resolve(); });
      expect(overlayWindowSize(state.status, size, false)).toEqual({ width: 136, height: 36 });
      expect(overlayWindowSize(state.status, size, true)).toEqual({ width: 136, height: 36 });
      expect(document.querySelector("main")).toHaveStyle({ width: "136px", height: "36px" });
      expect(screen.queryByText("Private final words")).toBeNull();
      expect(screen.queryByLabelText(t("overlay.elapsed"))).toBeNull();
      expect(screen.queryByRole("alertdialog")).toBeNull();
    },
  );
  test("recording has Fold, real level, and named focusable contextual commands", async () => {
    const view = overlay({ status: "recording", ...take });
    const stopRecording = vi.fn(async () => snapshotFor({ status: "processing", ...take }));
    Object.assign(view.adapter, { stopRecording });
    expect(await screen.findByLabelText(t("overlay.micLevel"))).toBeVisible();
    expect(document.querySelector(".overlay-reading .loquara-mark")).not.toBeNull();
    act(() => view.emitLevel(.8));
    expect(screen.getByLabelText(t("overlay.micLevel"))).toHaveAttribute("data-level", "0.80");
    const stop = screen.getByRole("button", { name: t("overlay.stop") });
    stop.focus(); expect(stop).toHaveFocus();
    fireEvent.click(stop);
    expect(stopRecording).toHaveBeenCalledOnce();
    expect(await screen.findByText(t("overlay.transcribing"))).toBeVisible();
  });
  test("new take resets the previous meter and processing rejects microphone levels", async () => {
    const view = overlay({ status: "recording", ...take });
    await screen.findByLabelText(t("overlay.micLevel"));
    act(() => view.emitLevel(.9));
    act(() => view.emitState({ status: "processing", ...take }));
    act(() => view.emitLevel(.9));
    expect(screen.queryByLabelText(t("overlay.micLevel"))).toBeNull();
    act(() => view.emitState({ status: "recording", recordingId: "next", audioPath: "next.wav" }));
    expect(screen.getByLabelText(t("overlay.micLevel"))).toHaveAttribute("data-level", "0.00");
  });
  test.each(["recording", "processing"] as const)("%s cancels once without confirmation", async (status) => {
    const view = overlay({ status, ...take });
    const cancelRecording = vi.fn(async () => snapshotFor({ status: "idle" }));
    Object.assign(view.adapter, { cancelRecording });
    fireEvent.click(await screen.findByRole("button", { name: t("overlay.cancel") }));
    expect(cancelRecording).toHaveBeenCalledOnce();
    expect(screen.queryByRole("alertdialog")).toBeNull();
  });
  test("long cancellation exposes retranscription and leaves expiry to native lifecycle", async () => {
    vi.useFakeTimers();
    try {
      const view = overlay({ status: "cancelling", ...take });
      const requestCancel = vi.fn();
      const retryTranscription = vi.fn(async () => snapshotFor({ status: "processing", ...take }));
      Object.assign(view.adapter, { requestCancel, retryTranscription });
      await act(async () => { await Promise.resolve(); });
      act(() => vi.advanceTimersByTime(5100));
      expect(requestCancel).not.toHaveBeenCalled();
      fireEvent.click(screen.getByRole("button", { name: t("overlay.undoAria") }));
      expect(retryTranscription).toHaveBeenCalledWith("take");
    } finally { vi.useRealTimers(); }
  });
  test("failure stays actionable and retries retained audio", async () => {
    const view = overlay({ status: "failed", recovery: take, error: "Full failure details" });
    const retryTranscription = vi.fn(async () => snapshotFor({ status: "processing", ...take }));
    Object.assign(view.adapter, { retryTranscription });
    const retry = await screen.findByRole("button", { name: t("overlay.failed.retryAria") });
    expect(retry).toHaveAttribute("title", "Full failure details");
    fireEvent.click(retry);
    expect(retryTranscription).toHaveBeenCalledWith("take");
  });
  test("draft clipboard error preserves meter and transport", async () => {
    const view = overlay({ status: "recording", ...take });
    await screen.findByLabelText(t("overlay.micLevel"));
    act(() => { view.emitLevel(.7); view.emitError("Clipboard temporarily busy"); });
    expect(screen.getByRole("alert")).toHaveTextContent("Clipboard temporarily busy");
    expect(screen.getByLabelText(t("overlay.micLevel"))).toHaveAttribute("data-level", "0.70");
    expect(screen.getByRole("button", { name: t("overlay.stop") })).toBeEnabled();
  });
  test("pasting is not a premature delivery acknowledgement", async () => {
    overlay({ status: "pasting", ...take, transcript: "hello" });
    expect(await screen.findByText(t("overlay.transcribing"))).toBeVisible();
    expect(screen.queryByText(t("overlay.ready"))).toBeNull();
    expect(screen.queryByText(t("overlay.copied"))).toBeNull();
  });
  test.each(["copied", "pasteRequested", "cancelled"] as const)("idle %s notice is visible without frontend hide", async (kind) => {
    const hideOverlay = vi.fn();
    const current = snapshotFor({ status: "idle" });
    current.overlayNotice = { key: "notice", generation: 2, recordingId: "take", kind };
    renderWithI18n(<RecorderOverlay adapter={adapterStub({ getAppSnapshot: async () => current, hideOverlay })} />);
    expect(await screen.findByText(t(kind === "copied" ? "overlay.copied" : kind === "cancelled" ? "overlay.cancelled" : "overlay.ready"))).toBeVisible();
    expect(hideOverlay).not.toHaveBeenCalled();
    if (kind === "pasteRequested") expect(screen.getByRole("status")).toHaveAttribute("aria-label", t("overlay.pasteRequestedDetail"));
  });
  test("delivery failure opens details or dismisses only on explicit owner-aware action", async () => {
    const current = snapshotFor({ status: "idle" });
    current.overlayNotice = { key: "failure", generation: 7, recordingId: "take", kind: "deliveryFailed", detail: "Native target refused input" };
    const dismissOverlayNotice = vi.fn(async () => snapshotFor({ status: "idle" }));
    const openOverlayDetails = vi.fn(async () => undefined);
    renderWithI18n(<RecorderOverlay adapter={adapterStub({ getAppSnapshot: async () => current, dismissOverlayNotice, openOverlayDetails })} />);
    const details = await screen.findByRole("button", { name: t("overlay.openDetails") });
    expect(openOverlayDetails).not.toHaveBeenCalled();
    fireEvent.click(details);
    expect(openOverlayDetails).toHaveBeenCalledWith("failure", 7);
    fireEvent.click(screen.getByRole("button", { name: t("overlay.dismiss") }));
    expect(dismissOverlayNotice).toHaveBeenCalledWith("failure", 7);
  });
  test("legacy context menu cannot change geometry", async () => {
    overlay({ status: "recording", ...take }, "large");
    fireEvent.contextMenu(await screen.findByLabelText(t("overlay.micLevel")));
    expect(screen.queryByRole("menu")).toBeNull();
  });
});

describe("initialization and event ownership", () => {
  test("initial wait shows actual loading and fixed geometry", () => {
    renderWithI18n(<RecorderOverlay adapter={adapterStub({ getAppSnapshot: () => new Promise(() => undefined) })} />);
    expect(screen.getByRole("status", { name: t("overlay.boot") })).toBeVisible();
    expect(document.querySelector(".loquara-mark--working")).not.toBeNull();
  });
  test.each(["snapshot", "state", "level", "error"] as const)("initialization %s failure remains retryable", async (failure) => {
    const fail = async () => { throw new Error("Initialization unavailable"); };
    const adapter = adapterStub({
      ...(failure === "snapshot" ? { getAppSnapshot: fail } : {}),
      ...(failure === "state" ? { onState: fail } : {}),
      ...(failure === "level" ? { onLevel: fail } : {}),
      ...(failure === "error" ? { onError: fail } : {}),
    });
    renderWithI18n(<RecorderOverlay adapter={adapter} />);
    expect(await screen.findByRole("alert")).toHaveAttribute("title", "Initialization unavailable");
    expect(screen.getByRole("button", { name: t("overlay.initError.retryAria") })).toBeEnabled();
  });
  test("retry remounts failed initialization", async () => {
    const getAppSnapshot = vi.fn().mockRejectedValueOnce(new Error("Temporary")).mockResolvedValueOnce(snapshotFor({ status: "recording", ...take }));
    renderWithI18n(<RecorderOverlay adapter={adapterStub({ getAppSnapshot })} />);
    fireEvent.click(await screen.findByRole("button", { name: t("overlay.initError.retryAria") }));
    expect(await screen.findByLabelText(t("overlay.micLevel"))).toBeVisible();
    expect(getAppSnapshot).toHaveBeenCalledTimes(2);
  });
  test("late initial snapshot cannot overwrite a newer event", async () => {
    let resolveSnapshot!: (value: AppSnapshot) => void;
    let emitState!: (value: AppSnapshot) => void;
    renderWithI18n(<RecorderOverlay adapter={adapterStub({
      getAppSnapshot: () => new Promise((resolve) => { resolveSnapshot = resolve; }),
      onState: async (listener) => { emitState = listener; return () => undefined; },
    })} />);
    await waitFor(() => expect(emitState).toBeTypeOf("function"));
    act(() => emitState(snapshotFor({ status: "recording", ...take })));
    resolveSnapshot(snapshotFor({ status: "idle" }));
    expect(await screen.findByLabelText(t("overlay.micLevel"))).toBeVisible();
  });
  test("late command response cannot overwrite a newer native notice", async () => {
    let reply!: (value: AppSnapshot) => void;
    let emit!: (value: AppSnapshot) => void;
    const notice: OverlayNotice = { key: "n", generation: 1, recordingId: "take", kind: "copied" };
    renderWithI18n(<RecorderOverlay adapter={adapterStub({
      getAppSnapshot: async () => snapshotFor({ status: "recording", ...take }),
      stopRecording: () => new Promise((resolve) => { reply = resolve; }),
      onState: async (listener) => { emit = listener; return () => undefined; },
    })} />);
    fireEvent.click(await screen.findByRole("button", { name: t("overlay.stop") }));
    act(() => emit({ ...snapshotFor({ status: "idle" }), overlayNotice: notice }));
    await act(async () => reply(snapshotFor({ status: "processing", ...take })));
    expect(screen.getByText(t("overlay.copied"))).toBeVisible();
  });
  test("late listeners are disposed after unmount", async () => {
    const dispose = vi.fn(); let resolve!: (value: () => void) => void;
    const view = renderWithI18n(<RecorderOverlay adapter={adapterStub({ onState: () => new Promise((done) => { resolve = done; }) })} />);
    view.unmount(); resolve(dispose);
    await waitFor(() => expect(dispose).toHaveBeenCalledOnce());
  });
});

test.each(["copied", "pasteRequested", "cancelled", "deliveryerror"])("DEV %s fixture exercises idle notice consumer", async (kind) => {
  const previous = window.location.href;
  try {
    window.history.replaceState({}, "", `/?demoState=${kind}`);
    const adapter = createBrowserAdapter();
    renderWithI18n(<RecorderOverlay adapter={adapter} />);
    const current = await adapter.getAppSnapshot();
    expect(current.dictation.status).toBe("idle");
    expect(current.overlayNotice).toBeTruthy();
    await act(async () => { await Promise.resolve(); });
    expect(document.querySelector("main")).toHaveStyle({ width: "136px", height: "36px" });
  } finally { window.history.replaceState({}, "", previous); }
});

test("physical geometry clamps negative-coordinate monitor", () => {
  const area = { position: { x: -1920, y: -120 }, size: { width: 1920, height: 1040 } };
  expect(clampOverlayPosition({ x: -40, y: 900 }, { width: 204, height: 54 }, area)).toEqual({ x: -204, y: 866 });
  expect(clampOverlayPosition({ x: -2400, y: -400 }, { width: 204, height: 54 }, area)).toEqual({ x: -1920, y: -120 });
});
