import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import type {
  AppSettings,
  AppSnapshot,
  HfAccount,
  HistoryQuery,
  InputDeviceInfo,
  Mode,
  ModelDescriptor,
  ModelDownloadProgress,
  ModelStatus,
  Recording,
  SettingsUpdateResult,
  VocabularyEntry,
} from "./types";
import { normalizeError } from "./errors";
import { translate } from "./i18n/lang";
type Listener<T> = (payload: T) => void;
export interface AppAdapter {
  getAppSnapshot(): Promise<AppSnapshot>;
  listInputDevices(): Promise<InputDeviceInfo[]>;
  startRecording(): Promise<AppSnapshot>;
  stopRecording(): Promise<AppSnapshot>;
  cancelRecording(): Promise<AppSnapshot>;
  requestCancel(): Promise<AppSnapshot>;
  hideOverlay(): Promise<void>;
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
  addVocabulary(heard: string, replacement: string): Promise<VocabularyEntry>;
  deleteVocabulary(id: number): Promise<boolean>;
  listModes(): Promise<Mode[]>;
  upsertMode(mode: Mode): Promise<void>;
  deleteMode(id: string): Promise<boolean>;
  getSettings(): Promise<AppSettings>;
  getModelStatus(): Promise<ModelStatus>;
  listModels(): Promise<ModelDescriptor[]>;
  downloadModel(model: string): Promise<void>;
  hfAccount(): Promise<HfAccount>;
  connectHfAccount(token: string): Promise<HfAccount>;
  disconnectHfAccount(): Promise<HfAccount>;
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
  startRecording: () => invoke("start_recording"),
  stopRecording: () => invoke("stop_recording"),
  cancelRecording: () => invoke("cancel_recording"),
  requestCancel: () => invoke("request_cancel"),
  hideOverlay: () => invoke("hide_overlay"),
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
  addVocabulary: (heard, replacement) =>
    invoke("add_vocabulary", { heard, replacement }),
  deleteVocabulary: (id) => invoke("delete_vocabulary", { id }),
  listModes: () => invoke("list_modes"),
  upsertMode: (mode) => invoke("upsert_mode", { mode }),
  deleteMode: (id) => invoke("delete_mode", { id }),
  getSettings: () => invoke("get_settings"),
  getModelStatus: () => invoke("get_model_status"),
  listModels: () => invoke("list_models"),
  downloadModel: (model) => invoke("download_model", { model }),
  hfAccount: () => invoke("hf_account"),
  connectHfAccount: (token) => invoke("connect_hf_account", { token }),
  disconnectHfAccount: () => invoke("disconnect_hf_account"),
  deleteModel: (model) => invoke("delete_model", { model }),
  updateSettings: (settings) => invoke("update_settings", { settings }),
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
  autoPaste: true,
  retentionDays: 30,
  launchOnLogin: false,
  activeMode: "clean",
  showOverlay: true,
  model: "parakeet",
  streaming: false,
  language: "system",
  dictationLanguage: "auto",
  modelKeepAliveSecs: 0,
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
const builtInModes = (): Mode[] =>
  [
    ["clean", "demo.mode.clean.name", "demo.mode.clean.description", "demo.mode.clean.prompt"],
    ["message", "demo.mode.message.name", "demo.mode.message.description", "demo.mode.message.prompt"],
    ["code", "demo.mode.code.name", "demo.mode.code.description", "demo.mode.code.prompt"],
  ].map(([id, name, description, prompt], index) => ({
    id,
    name: translate(name as "demo.mode.clean.name"),
    description: translate(description as "demo.mode.clean.description"),
    prompt: translate(prompt as "demo.mode.clean.prompt"),
    enabled: true,
    isDefault: true,
    createdAt: index,
  }));
export function createBrowserAdapter(): AppAdapter {
  let settings = { ...initialSettings };
  let snapshot: AppSnapshot = { dictation: { status: "idle" }, settings, modelLoading: false };
  let history = demoHistory();
  let vocabulary: VocabularyEntry[] = [
    { id: 1, heard: "parakit", replacement: "Parakeet" },
  ];
  let modes = builtInModes();
  const stateListeners = new Set<Listener<AppSnapshot>>();
  const levelListeners = new Set<Listener<number>>();
  const emit = () => stateListeners.forEach((listener) => listener(snapshot));
  const setState = (dictation: AppSnapshot["dictation"]) => {
    snapshot = { dictation, settings, modelLoading: false };
    emit();
    return snapshot;
  };
  return {
    getAppSnapshot: async () => snapshot,
    listInputDevices: async () => [
      { id: "default", name: "Mikrofon (domyślny)", isDefault: true },
      { id: "studio", name: "Mikrofon USB", isDefault: false },
    ],
    startRecording: async () => {
      const id = `demo-${Date.now()}`;
      snapshot = { dictation: { status: "recording", recordingId: id, audioPath: `${id}.wav` }, settings, modelLoading: false, recordingStartedAt: Date.now() };
      emit();
      return snapshot;
    },
    stopRecording: async () => {
      if (snapshot.dictation.status !== "recording") return snapshot;
      const { recordingId, audioPath } = snapshot.dictation;
      setState({ status: "processing", recordingId, audioPath });
      window.setTimeout(() => {
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
    cancelRecording: async () => setState({ status: "idle" }),
    requestCancel: async () => snapshot,
    hideOverlay: async () => undefined,
    saveOverlayPosition: async () => undefined,
    setShortcutSuspended: async () => undefined,
    retryTranscription: async (recordingId) => {
      const item = history.find((recording) => recording.id === recordingId);
      if (!item?.audioPath) throw new Error(translate("errors.noAudio"));
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
    addVocabulary: async (heard, replacement) => {
      const item = { id: Math.max(0, ...vocabulary.map(({ id }) => id)) + 1, heard, replacement };
      vocabulary = [...vocabulary, item];
      return item;
    },
    deleteVocabulary: async (id) => {
      const before = vocabulary.length;
      vocabulary = vocabulary.filter((item) => item.id !== id);
      return before !== vocabulary.length;
    },
    listModes: async () => [...modes],
    upsertMode: async (mode) => {
      modes = modes.some(({ id }) => id === mode.id)
        ? modes.map((item) => item.id === mode.id ? mode : item)
        : [...modes, mode];
    },
    deleteMode: async (id) => {
      const before = modes.length;
      modes = modes.filter((item) => item.id !== id || item.isDefault);
      return before !== modes.length;
    },
    getSettings: async () => settings,
    getModelStatus: async () => ({
      state: "ready",
      model: "nvidia/parakeet-tdt-0.6b-v3",
      revision: "7c35754d166cca382ad1e53e68b01e7c575f3a1d",
      device: null,
      message: null,
    }),
    listModels: async () => ([
      { key: "parakeet", id: "nvidia/parakeet-tdt-0.6b-v3", provider: "NVIDIA", source: "local", revision: "7c35754d166cca382ad1e53e68b01e7c575f3a1d", display: "Parakeet TDT 0.6B v3", minVramGb: 3, minRamGb: 8, languages: "auto (PL/EN)", estimatedSizeBytes: 2_500_000_000, status: "ready", installedSizeBytes: 2_500_000_000, message: null },
      { key: "whisper-turbo", id: "openai/whisper-large-v3-turbo", provider: "OpenAI", source: "local", revision: "41f01f3fe87f28c78e2fbf8b568835947dd65ed9", display: "Whisper Large v3 Turbo", minVramGb: 4, minRamGb: 8, languages: "auto (99)", estimatedSizeBytes: 1_600_000_000, status: "not_installed", installedSizeBytes: null, message: null },
      { key: "whisper-small", id: "openai/whisper-small", provider: "OpenAI", source: "local", revision: "973afd24965f72e36ca33b3055d56a652f456b4d", display: "Whisper Small", minVramGb: 1.5, minRamGb: 4, languages: "auto (99)", estimatedSizeBytes: 1_000_000_000, status: "not_installed", installedSizeBytes: null, message: null },
      { key: "cohere", id: "AEmotionStudio/cohere-transcribe-03-2026-models", provider: "Cohere", source: "local", revision: "d114f701a80b2150943f5dbae71458f4d1fcb37b", display: "Cohere Transcribe 2B", minVramGb: 5, minRamGb: 8, languages: "pl/en/fr/de/...", estimatedSizeBytes: 4_132_000_000, status: "not_installed", installedSizeBytes: null, message: null },
    ]),
    downloadModel: async () => undefined,
    hfAccount: async () => ({ connected: false, name: null }),
    connectHfAccount: async () => ({ connected: true, name: "demo" }),
    disconnectHfAccount: async () => ({ connected: false, name: null }),
    deleteModel: async () => undefined,
    updateSettings: async (next) => {
      settings = { ...next };
      snapshot = { ...snapshot, settings };
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
    onError: async () => () => undefined,
  };
}
let browserAdapter: AppAdapter | undefined;
export function getAdapter(): AppAdapter {
  const isTauri = "__TAURI_INTERNALS__" in window;
  if (isTauri) return realAdapter;
  browserAdapter ??= createBrowserAdapter();
  return browserAdapter;
}
