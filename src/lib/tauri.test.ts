import { describe, expect, test, vi } from "vitest";

import { createBrowserAdapter, platformErrorMessage } from "./tauri";

test("missing manual and favorite microphones block synthetic capture without default fallback", async () => {
  const adapter = createBrowserAdapter();
  const settings = await adapter.getSettings();
  await adapter.updateSettings({ ...settings, microphones: {
    selection: "manual", selectedId: "missing-endpoint", favorites: [], hiddenIds: [],
    allowSystemDefaultFallback: false, unresolvedLegacyName: null,
  } } as typeof settings);
  await expect(adapter.startRecording()).rejects.toThrow(/microphone/i);
  expect((await adapter.getAppSnapshot()).dictation.status).toBe("idle");
  await adapter.updateSettings({ ...settings, microphones: {
    selection: "favorites", selectedId: null, favorites: [{ id: "missing", name: "Lost USB" }], hiddenIds: [],
    allowSystemDefaultFallback: false, unresolvedLegacyName: null,
  } });
  expect(await adapter.listInputDevices()).toContainEqual(expect.objectContaining({ id: "missing", name: "Lost USB", available: false }));
  await expect(adapter.startRecording()).rejects.toThrow(/microphone/i);
  const saved = await adapter.getSettings();
  await adapter.updateSettings({ ...saved, microphones: { ...saved.microphones!, allowSystemDefaultFallback: true } });
  expect((await adapter.startRecording()).dictation.status).toBe("recording");
  await adapter.cancelRecording();
});

describe("adapter demonstracyjny", () => {
  test("izoluje dane każdej instancji i nie zwraca danych po angielsku", async () => {
    const first = createBrowserAdapter();
    const second = createBrowserAdapter();
    const before = await second.listVocabulary();

    await first.addVocabulary("parakit", "Parakeet");

    expect(await second.listVocabulary()).toEqual(before);
    const history = await first.listHistory({});
    expect(history.length).toBeGreaterThan(2);
    expect(history.some((item) => item.text?.includes("spotkania"))).toBe(true);
  });

  test("normalizuje obiektowy błąd platformy i zachowuje zgodność ze stringiem", () => {
    expect(platformErrorMessage({ code: "focus_failed", message: "Nie udało się przywrócić okna." }))
      .toBe("Nie udało się przywrócić okna.");
    expect(platformErrorMessage("Starszy błąd")).toBe("Starszy błąd");
    expect(platformErrorMessage({ error: { message: "Błąd schowka" } })).toBe("Błąd schowka");
    expect(platformErrorMessage({ code: "unknown" })).toBe("{\"code\":\"unknown\"}");
  });

  test("demo raportuje prawdziwy kontrakt gotowego modelu", async () => {
    expect(await createBrowserAdapter().getModelStatus()).toMatchObject({
      state: "ready",
      model: "Parakeet TDT 0.6B v3",
    });
  });
});


test("browser snapshot includes engine metadata and publishes bounded scoped synthetic drafts", async () => {
  vi.useFakeTimers();
  try {
    const adapter = createBrowserAdapter();
    expect(await adapter.getAppSnapshot()).toMatchObject({ model: { installed: true, source: "local", key: "parakeet" } });
    const events: unknown[] = [];
    const dispose = await adapter.onState((state) => events.push(state));
    const started = await adapter.startRecording();
    const id = started.dictation.status === "recording" ? started.dictation.recordingId : "";
    vi.advanceTimersByTime(2500);
    expect(await adapter.getAppSnapshot()).toMatchObject({ liveTranscript: { recordingId: id, revision: 3, status: "draft", clipboardReady: false } });
    expect(events).toHaveLength(4);
    await adapter.cancelRecording();
    vi.advanceTimersByTime(10000);
    expect(await adapter.getAppSnapshot()).toMatchObject({ dictation: { status: "idle" }, liveTranscript: null });
    dispose();
  } finally { vi.useRealTimers(); }
});

test("cancelling during synthetic finalization prevents late history publication", async () => {
  vi.useFakeTimers();
  try {
    const adapter = createBrowserAdapter();
    const before = await adapter.listHistory({});
    await adapter.startRecording();
    await adapter.stopRecording();
    await adapter.cancelRecording();
    vi.advanceTimersByTime(5000);
    expect(await adapter.listHistory({})).toEqual(before);
    expect(await adapter.getAppSnapshot()).toMatchObject({ dictation: { status: "idle" }, liveTranscript: null });
  } finally { vi.useRealTimers(); }
});


test("browser model catalogue matches the shipped int8 Parakeet engine", async () => {
  const models = await createBrowserAdapter().listModels();
  expect(models).toHaveLength(1);
  expect(models[0]).toMatchObject({ key: "parakeet", estimatedSizeBytes: 670_478_772, installedSizeBytes: 670_478_772, minVramGb: 0, minRamGb: 2 });
});


test("browser settings save coerces legacy expanded overlay to compact", async () => {
  const adapter = createBrowserAdapter();
  const initial = await adapter.getSettings();
  const persisted = await adapter.updateSettings({ ...initial, overlaySize: "large", autoPaste: false });
  expect(persisted.settings).toMatchObject({ overlaySize: "mini", autoPaste: false });
  expect(await adapter.getAppSnapshot()).toMatchObject({ settings: { overlaySize: "mini", autoPaste: false } });
});


test.each(["pasting", "initerror", "deliveryerror"])("DEV fixture %s follows the real adapter contract", async (state) => {
  const previousUrl = window.location.href;
  vi.useFakeTimers();
  try {
    window.history.replaceState({}, "", `/?demoState=${state}&demoSize=large`);
    const adapter = createBrowserAdapter();
    if (state === "initerror") {
      await expect(adapter.getAppSnapshot()).rejects.toThrow();
    } else if (state === "pasting") {
      expect(await adapter.getAppSnapshot()).toMatchObject({ dictation: { status: "pasting", recordingId: "synthetic-preview" }, settings: { overlaySize: "large" } });
    } else {
      expect(await adapter.getAppSnapshot()).toMatchObject({ dictation: { status: "idle" }, overlayNotice: { kind: "deliveryFailed" } });
      const listener = vi.fn();
      const dispose = await adapter.onError(listener);
      vi.advanceTimersByTime(50);
      expect(listener).not.toHaveBeenCalled();
      dispose();
    }
  } finally {
    window.history.replaceState({}, "", previousUrl);
    vi.useRealTimers();
  }
});
