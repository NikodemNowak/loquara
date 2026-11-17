import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import type {
  AppSettings,
  AppSnapshot,
  HistoryQuery,
  InputDeviceInfo,
  InputDevicesChanged,
  ModelDescriptor,
  ModelDownloadProgress,
  ModelStatus,
  Recording,
  SettingsUpdateResult,
  VocabularyEntry,
  VocabularySuggestion,
} from "./types";
import { realtimeEnabled } from "./types";
import { microphoneChoices, microphonePreferences, resolveMicrophone } from "./microphones";
import { normalizeError } from "./errors";
import { translate } from "./i18n/lang";
type Listener<T> = (payload: T) => void;
export interface AppAdapter {
  getAppSnapshot(): Promise<AppSnapshot>;
  listInputDevices(): Promise<InputDeviceInfo[]>;
  onInputDevicesChanged(listener: Listener<InputDevicesChanged>): Promise<UnlistenFn>;
  startRecording(): Promise<AppSnapshot>;
  stopRecording(): Promise<AppSnapshot>;
  cancelRecording(): Promise<AppSnapshot>;
  requestCancel(): Promise<AppSnapshot>;
  hideOverlay(): Promise<void>;
  dismissOverlayNotice(key: string, generation: number): Promise<AppSnapshot>;
  openOverlayDetails(key: string, generation: number): Promise<void>;
  saveOverlayPosition(x: number, y: number): Promise<void>;
  setShortcutSuspended(suspended: boolean): Promise<void>;
  retryTranscription(recordingId: string): Promise<AppSnapshot>;
  pasteTranscript(recordingId?: string): Promise<void>;
  listHistory(query: HistoryQuery): Promise<Recording[]>;
  deleteHistory(recordingId: string): Promise<boolean>;
  exportTranscript(recordingId: string): Promise<string>;
  clearFailedRecordings(): Promise<number>;
  playRecording(recordingId: string): Promise<void>;
  getRecordingAudio(recordingId: string): Promise<Uint8Array>;
  revealRecording(recordingId: string): Promise<void>;
  openRecordingsFolder(): Promise<void>;
  openModelFolder(modelKey: string): Promise<void>;
  correctTranscript(recordingId: string, text: string): Promise<number>;
  listVocabulary(): Promise<VocabularyEntry[]>;
  /** Adding a term that already exists merges the aliases into it. */
  addVocabulary(term: string, aliases: string[]): Promise<VocabularyEntry>;
  deleteVocabulary(id: number): Promise<boolean>;
  vocabularySuggestions(): Promise<VocabularySuggestion[]>;
  getSettings(): Promise<AppSettings>;
  getModelStatus(): Promise<ModelStatus>;
  listModels(): Promise<ModelDescriptor[]>;
  downloadModel(model: string): Promise<void>;
  cancelDownload(): Promise<void>;
  deleteModel(model: string): Promise<void>;
  updateSettings(settings: AppSettings): Promise<SettingsUpdateResult>;
  updateSettingValue(key: string, value: unknown): Promise<void>;
  onState(listener: Listener<AppSnapshot>): Promise<UnlistenFn>;
  onLevel(listener: Listener<number>): Promise<UnlistenFn>;
  onModelProgress(listener: Listener<ModelDownloadProgress>): Promise<UnlistenFn>;
  onError(listener: Listener<string>): Promise<UnlistenFn>;
}
export function platformErrorMessage(payload: unknown): string {
  return normalizeError(payload, translate("errors.platform"));
}
function isStateNotManaged(error: unknown): boolean {
  const message = normalizeError(error, "").toLowerCase();
  return message.includes("state not managed") || message.includes("not managed for field");
}
async function invokeAfterStateReady<T>(command: string, args?: Record<string, unknown>): Promise<T> {
  let lastError: unknown;
  for (let attempt = 0; attempt < 40; attempt += 1) {
    try {
      return await invoke<T>(command, args);
    } catch (error) {
      lastError = error;
      if (!isStateNotManaged(error)) throw error;
      await new Promise((resolve) => window.setTimeout(resolve, 50));
    }
  }
  throw lastError ?? new Error(translate("errors.stateInit"));
}
const realAdapter: AppAdapter = {
  getAppSnapshot: () => invokeAfterStateReady("get_app_snapshot"),
  listInputDevices: () => invoke("list_input_devices"),
  onInputDevicesChanged: (listener) => listen<InputDevicesChanged>("audio://devices-changed", (event) => listener(event.payload)),
  startRecording: () => invoke("start_recording"),
  stopRecording: () => invoke("stop_recording"),
  cancelRecording: () => invoke("cancel_recording"),
  requestCancel: () => invoke("request_cancel"),
  hideOverlay: () => invoke("hide_overlay"),
  dismissOverlayNotice: (key, generation) => invoke("dismiss_overlay_notice", { key, generation }),
  openOverlayDetails: (key, generation) => invoke("open_overlay_details", { key, generation }),
  saveOverlayPosition: (x, y) => invoke("save_overlay_position", { x, y }),
  setShortcutSuspended: (suspended) => invoke("set_shortcut_suspended", { suspended }),
  retryTranscription: (recordingId) =>
    invoke("retry_transcription", { recordingId }),
  pasteTranscript: (recordingId) =>
    invoke("paste_transcript", { recordingId }),
  listHistory: (query) => invokeAfterStateReady("list_history", { query }),
  deleteHistory: (recordingId) => invoke("delete_history", { recordingId }),
    exportTranscript: (recordingId) => invoke("export_transcript", { recordingId }),
    clearFailedRecordings: () => invoke("clear_failed_recordings"),
    playRecording: (recordingId) => invoke("play_recording", { recordingId }),
    getRecordingAudio: (recordingId) => invoke("read_recording_audio", { recordingId }),
    revealRecording: (recordingId) => invoke("reveal_recording", { recordingId }),
    openRecordingsFolder: () => invoke("reveal_recordings_dir"),
    openModelFolder: (modelKey) => invoke("reveal_model_dir", { model: modelKey }),
    correctTranscript: (recordingId, text) => invoke("correct_transcript", { recordingId, text }),
  listVocabulary: () => invoke("list_vocabulary"),
  addVocabulary: (term, aliases) => invoke("add_vocabulary", { term, aliases }),
  deleteVocabulary: (id) => invoke("delete_vocabulary", { id }),
  vocabularySuggestions: () => invoke("vocabulary_suggestions"),
  getSettings: () => invoke("get_settings"),
  getModelStatus: () => invoke("get_model_status"),
  listModels: () => invoke("list_models"),
  downloadModel: (model) => invoke("download_model", { model }),
  cancelDownload: () => invoke("cancel_download"),
  deleteModel: (model) => invoke("delete_model", { model }),
  updateSettings: (settings) => invoke("update_settings", { settings: { ...settings, overlaySize: "mini" } }),
  updateSettingValue: (key, value) =>
    invoke("update_setting_value", { key, value }),
  onState: (listener) =>
    listen<AppSnapshot>("dictation://state", (event) => listener(event.payload)),
  onLevel: (listener) =>
    listen<number>("dictation://level", (event) => listener(event.payload)),
  onModelProgress: (listener) =>
    listen<ModelDownloadProgress>("models://progress", (event) => listener(event.payload)),
  onError: async (listener) => {
    const unlistenPaste = await listen<unknown>("dictation://paste_error", (event) =>
      listener(platformErrorMessage(event.payload)),
    );
    const unlistenPersistence = await listen<string>(
      "dictation://persistence_error",
      (event) => listener(platformErrorMessage(event.payload)),
    );
    return () => {
      unlistenPaste();
      unlistenPersistence();
    };
  },
};
const initialSettings: AppSettings = {
  inputDevice: null,
  shortcut: "Ctrl+Space",
  activationMode: "toggle",
  autoPaste: true,
  retentionDays: 30,
  launchOnLogin: false,
  startMinimized: false,
  showOverlay: true,
  overlaySize: "mini",
  model: "parakeet",
  streaming: false,
  realtimeByEngine: { "local:parakeet": true },
  language: "system",
  modelKeepAliveSecs: 0,
  pasteMode: "auto",
  mediaDuringDictation: "muffle",
  fillerRemoval: "light",
  voiceCommands: true,
  snippets: [],
  appProfiles: [],
};

/** A speech-shaped envelope, so the demo mode looks like real dictation. */
const demoPeaks = (seed: number): number[] => {
  let value = seed * 2_654_435_761;
  const next = () => {
    value = (value * 1_103_515_245 + 12_345) & 0x7fffffff;
    return value / 0x7fffffff;
  };
  return Array.from({ length: 200 }, (_, index) => {
    const syllable = 0.5 + 0.5 * Math.sin(index / 3.1);
    const phrase = index % 47 < 6 ? 0.12 : 1;
    return Math.round(Math.min(1, syllable * phrase * (0.55 + 0.45 * next())) * 255);
  });
};

const demoHistory = (): Recording[] => [
  ["r1", "Przygotuj proszę podsumowanie dzisiejszego spotkania i wyślij je do zespołu.", "completed", 24_000, "Word"],
  ["r2", "Omówiliśmy harmonogram wdrożenia oraz kluczowe ryzyka.", "completed", 18_000, "Teams"],
  ["r3", "W załączniku przesyłam uwagi do umowy i propozycję zmian.", "completed", 12_000, "Outlook"],
  ["r4", null, "failed", 9_000, null],
  ["r5", "Sprawdź proszę najnowsze wymagania i daj znać, jeśli coś jest niejasne.", "completed", 17_000, "Edge"],
].map(([id, text, status, durationMs, sourceApp], index) => ({
  id: String(id),
  peaks: demoPeaks(index + 1),
  createdAt: Date.now() - index * 3_600_000,
  durationMs: Number(durationMs),
  status: status as Recording["status"],
  text: text as string | null,
  model: "NVIDIA Parakeet 0.6B v3",
  audioPath: `C:\\Mów\\nagrania\\${id}.wav`,
  sourceApp: sourceApp as string | null,
  error: status === "failed" ? "Nie udało się uruchomić modelu." : null,
}));
export function createBrowserAdapter(): AppAdapter {
  let settings = { ...initialSettings };
  const devices: InputDeviceInfo[] = [
    { id: "default", name: "Mikrofon (domyślny)", isDefault: true, available: true },
    { id: "studio", name: "Mikrofon USB", isDefault: false, available: true },
  ];
  settings.microphones = microphonePreferences(settings, devices);
  const deviceListeners = new Set<Listener<InputDevicesChanged>>();
  let deviceGeneration = 0;
  const modelSummary = () => {
    const names: Record<string, string> = { parakeet: "Parakeet TDT 0.6B v3" };
    return { key: settings.model, display: names[settings.model] ?? settings.model, provider: "NVIDIA",
      installed: settings.model === "parakeet", totalBytes: 1_020_355_800 };
  };
  let snapshot: AppSnapshot = { dictation: { status: "idle" }, settings, modelLoading: false, model: modelSummary(), liveTranscript: null };
  let history = demoHistory();
  let vocabulary: VocabularyEntry[] = [
    { id: 1, term: "Parakeet", aliases: ["parakit"] },
    { id: 2, term: "Loquara", aliases: [] },
    { id: 3, term: "Claude Code", aliases: [] },
  ];
  const stateListeners = new Set<Listener<AppSnapshot>>();
  const levelListeners = new Set<Listener<number>>();
  const emit = () => stateListeners.forEach((listener) => listener(snapshot));
  let generation = 0;
  const timers = new Set<number>();
  const schedule = (work: () => void, ms: number) => {
    const token = generation;
    const timer = window.setTimeout(() => { timers.delete(timer); if (token === generation) work(); }, ms);
    timers.add(timer);
  };
  const invalidate = () => { generation += 1; timers.forEach((timer) => window.clearTimeout(timer)); timers.clear(); };
  const setState = (dictation: AppSnapshot["dictation"]) => {
    snapshot = { ...snapshot, dictation, settings, modelLoading: false, model: modelSummary(), liveTranscript: dictation.status === "idle" ? null : snapshot.liveTranscript };
    emit();
    return snapshot;
  };
  // Bounded, explicitly synthetic layout fixtures exist only in Vite development.
  const params = new URLSearchParams(window.location.search);
  const demoState = import.meta.env.DEV ? params.get("demoState") : null;
  if (demoState && ["recording", "draft", "processing", "fallback", "failed", "cancelling", "pasting", "deliveryerror"].includes(demoState)) {
    settings = { ...settings, overlaySize: params.get("demoSize") === "large" ? "large" : "mini" };
    const take = { recordingId: "synthetic-preview", audioPath: "synthetic-preview.wav" };
    snapshot = { ...snapshot, settings, recordingStartedAt: Date.now() - 12_000,
      dictation: demoState === "failed" ? { status: "failed", recovery: take, error: translate("demo.previewError") }
        : demoState === "pasting" ? { status: "pasting", transcript: translate("demo.previewText"), ...take }
        : { status: demoState === "processing" ? "processing" : demoState === "cancelling" ? "cancelling" : "recording", ...take },
      liveTranscript: demoState === "draft" || demoState === "processing" || demoState === "fallback" ? { recordingId: take.recordingId,
        text: demoState === "fallback" ? "" : translate("demo.previewText"), revision: 2,
        status: demoState === "processing" ? "finalizing" : demoState === "fallback" ? "fallback" : "draft" } : null };
  }
  if (demoState && ["copied", "pasteRequested", "cancelled", "deliveryerror"].includes(demoState)) {
    snapshot = { ...snapshot, dictation: { status: "idle" }, liveTranscript: null,
      overlayNotice: { key: "synthetic-notice", generation, recordingId: "synthetic-preview",
        kind: demoState === "deliveryerror" ? "deliveryFailed" : demoState as "copied" | "pasteRequested" | "cancelled",
        detail: demoState === "deliveryerror" ? translate("demo.previewError") : null } };
  }
  return {
    getAppSnapshot: async () => {
      if (demoState === "initerror") throw new Error(translate("demo.previewError"));
      return snapshot;
    },
    listInputDevices: async () => microphoneChoices(microphonePreferences(settings, devices), devices),
    onInputDevicesChanged: async (listener) => { deviceListeners.add(listener); return () => deviceListeners.delete(listener); },
    startRecording: async () => {
      resolveMicrophone(microphonePreferences(settings, devices), devices);
      invalidate();
      const id = `demo-${Date.now()}-${generation}`;
      const preview = realtimeEnabled(settings, settings.model === "parakeet");
      snapshot = { dictation: { status: "recording", recordingId: id, audioPath: `${id}.wav` }, settings, modelLoading: false, model: modelSummary(), recordingStartedAt: Date.now(),
        liveTranscript: preview ? { recordingId: id, text: "", revision: 0, status: "listening" } : null };
      emit();
      if (preview) [800, 1600, 2400].forEach((delay, index) => schedule(() => {
        if (snapshot.dictation.status !== "recording" || snapshot.dictation.recordingId !== id) return;
        const words = translate("demo.previewText").split(" ");
        snapshot = { ...snapshot, liveTranscript: { recordingId: id, text: words.slice(0, Math.ceil(words.length * (index + 1) / 3)).join(" "), revision: index + 1, status: "draft" } };
        // Synthetic browser drafts never claim to have written the OS clipboard.
        levelListeners.forEach((listener) => listener(.34 + index * .12)); emit();
      }, delay));
      return snapshot;
    },
    stopRecording: async () => {
      if (snapshot.dictation.status !== "recording") return snapshot;
      const { recordingId, audioPath } = snapshot.dictation;
      invalidate();
      if (snapshot.liveTranscript) snapshot = { ...snapshot, liveTranscript: { ...snapshot.liveTranscript, status: "finalizing" } };
      setState({ status: "processing", recordingId, audioPath });
      schedule(() => {
        history = [{
          id: recordingId, createdAt: Date.now(), durationMs: 8_000,
          status: "completed", text: "To jest przykładowa transkrypcja z trybu demonstracyjnego.",
          model: "NVIDIA Parakeet 0.6B v3", audioPath, sourceApp: null, error: null,
          peaks: demoPeaks(history.length + 1),
        }, ...history];
        setState({ status: "idle" });
      }, 900);
      return snapshot;
    },
    cancelRecording: async () => { invalidate(); return setState({ status: "idle" }); },
    requestCancel: async () => { invalidate(); return setState({ status: "idle" }); },
    hideOverlay: async () => undefined,
    dismissOverlayNotice: async (key, owner) => {
      if (snapshot.overlayNotice?.key === key && snapshot.overlayNotice.generation === owner && owner === generation) {
        snapshot = { ...snapshot, overlayNotice: null }; emit();
      }
      return snapshot;
    },
    openOverlayDetails: async () => undefined,
    saveOverlayPosition: async () => undefined,
    setShortcutSuspended: async () => undefined,
    retryTranscription: async (recordingId) => {
      const item = history.find((recording) => recording.id === recordingId);
      if (!item?.audioPath) throw new Error(translate("errors.noAudio"));
      invalidate();
      snapshot = { ...snapshot, overlayNotice: null };
      return setState({ status: "processing", recordingId, audioPath: item.audioPath });
    },
    pasteTranscript: async () => undefined,
    listHistory: async (query) => {
      const term = query.search?.toLocaleLowerCase("pl") ?? "";
      return history.filter((item) =>
        (!query.status || item.status === query.status) &&
        (!term || `${item.text ?? ""} ${item.error ?? ""}`.toLocaleLowerCase("pl").includes(term)),
      );
    },
    deleteHistory: async (recordingId) => {
      const before = history.length;
      history = history.filter((item) => item.id !== recordingId);
      return before !== history.length;
    },
    exportTranscript: async (recordingId) => {
      const item = history.find((recording) => recording.id === recordingId);
      if (!item?.text) throw new Error(translate("errors.noTranscript"));
      return `${item.text}`;
    },
    clearFailedRecordings: async () => {
      const before = history.length;
      history = history.filter((item) => item.status !== "failed");
      return before - history.length;
    },
    playRecording: async () => undefined,
    getRecordingAudio: async () => new Uint8Array(0),
    revealRecording: async () => undefined,
    openRecordingsFolder: async () => undefined,
    openModelFolder: async () => undefined,
    correctTranscript: async () => 0,
    listVocabulary: async () => [...vocabulary],
    addVocabulary: async (term, aliases) => {
      const existing = vocabulary.find((item) => item.term.toLocaleLowerCase() === term.toLocaleLowerCase());
      const item = existing
        ? { ...existing, aliases: [...existing.aliases, ...aliases.filter((alias) => !existing.aliases.some((known) => known.toLocaleLowerCase() === alias.toLocaleLowerCase()))] }
        : { id: Math.max(0, ...vocabulary.map(({ id }) => id)) + 1, term, aliases };
      vocabulary = existing ? vocabulary.map((entry) => entry.id === item.id ? item : entry) : [...vocabulary, item];
      return item;
    },
    deleteVocabulary: async (id) => {
      const before = vocabulary.length;
      vocabulary = vocabulary.filter((item) => item.id !== id);
      return before !== vocabulary.length;
    },
    vocabularySuggestions: async () => {
      const term = vocabulary.find((item) => item.term === "Loquara" && !item.aliases.includes("lokwara"));
      return term ? [{ termId: term.id, term: term.term, heard: "lokwara", count: 3, example: "…wklej to do lokwara i sprawdź…" }] : [];
    },
    getSettings: async () => settings,
    getModelStatus: async () => ({
      state: "ready",
      model: "Parakeet TDT 0.6B v3",
      revision: "",
      device: null,
      message: null,
    }),
    listModels: async () => ([
      { key: "parakeet", id: "nvidia/parakeet-tdt-0.6b-v3 (8-bit, block 32)", provider: "NVIDIA", source: "local", revision: "", display: "Parakeet TDT 0.6B v3", minVramGb: 0, minRamGb: 2, languages: "25 języków", estimatedSizeBytes: 1_020_355_800, status: "ready", installedSizeBytes: 1_020_355_800, message: null },
    ]),
    downloadModel: async () => undefined,
    cancelDownload: async () => undefined,
    deleteModel: async () => undefined,
    updateSettings: async (next) => {
      const microphones = microphonePreferences(next, devices);
      if (microphones.selection === "manual" && microphones.selectedId !== null) microphones.hiddenIds = microphones.hiddenIds.filter((id) => id !== microphones.selectedId);
      settings = { ...next, microphones, inputDevice: microphones.selectedId, overlaySize: "mini" };
      const change = { generation: ++deviceGeneration, devices: microphoneChoices(microphones, devices) };
      deviceListeners.forEach((listener) => listener(change));
      snapshot = { ...snapshot, settings, model: modelSummary() };
      emit();
      return { settings, warning: null };
    },
    updateSettingValue: async () => undefined,
    onState: async (listener) => {
      stateListeners.add(listener);
      return () => stateListeners.delete(listener);
    },
    onLevel: async (listener) => {
      levelListeners.add(listener);
      return () => levelListeners.delete(listener);
    },
    onModelProgress: async () => () => undefined,
    onError: async (listener) => {
      if (demoState !== "drafterror") return () => undefined;
      const timer = window.setTimeout(() => listener(translate("demo.previewError")), 50);
      return () => window.clearTimeout(timer);
    },
  };
}
let browserAdapter: AppAdapter | undefined;
export function getAdapter(): AppAdapter {
  const isTauri = "__TAURI_INTERNALS__" in window;
  if (isTauri) return realAdapter;
  browserAdapter ??= createBrowserAdapter();
  return browserAdapter;
}
