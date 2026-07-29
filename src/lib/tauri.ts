import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";

import type {
  AppSettings,
  AppSnapshot,
  HistoryQuery,
  InputDeviceInfo,
  Mode,
  ModelStatus,
  PlatformError,
  Recording,
  SettingsUpdateResult,
  VocabularyEntry,
} from "./types";

type Listener<T> = (payload: T) => void;

export interface AppAdapter {
  getAppSnapshot(): Promise<AppSnapshot>;
  listInputDevices(): Promise<InputDeviceInfo[]>;
  startRecording(): Promise<AppSnapshot>;
  stopRecording(): Promise<AppSnapshot>;
  cancelRecording(): Promise<AppSnapshot>;
  retryTranscription(recordingId: string): Promise<AppSnapshot>;
  pasteTranscript(recordingId?: string): Promise<void>;
  listHistory(query: HistoryQuery): Promise<Recording[]>;
  deleteHistory(recordingId: string): Promise<boolean>;
  listVocabulary(): Promise<VocabularyEntry[]>;
  addVocabulary(heard: string, replacement: string): Promise<VocabularyEntry>;
  deleteVocabulary(id: number): Promise<boolean>;
  listModes(): Promise<Mode[]>;
  upsertMode(mode: Mode): Promise<void>;
  deleteMode(id: string): Promise<boolean>;
  getSettings(): Promise<AppSettings>;
  getModelStatus(): Promise<ModelStatus>;
  updateSettings(settings: AppSettings): Promise<SettingsUpdateResult>;
  updateSettingValue(key: string, value: unknown): Promise<void>;
  onState(listener: Listener<AppSnapshot>): Promise<UnlistenFn>;
  onLevel(listener: Listener<number>): Promise<UnlistenFn>;
  onError(listener: Listener<string>): Promise<UnlistenFn>;
}

export function platformErrorMessage(payload: unknown): string {
  if (typeof payload === "string") return payload;
  if (payload && typeof payload === "object") {
    const error = payload as PlatformError;
    if (typeof error.message === "string" && error.message.trim()) return error.message;
  }
  return "Nieznany błąd platformy.";
}

const realAdapter: AppAdapter = {
  getAppSnapshot: () => invoke("get_app_snapshot"),
  listInputDevices: () => invoke("list_input_devices"),
  startRecording: () => invoke("start_recording"),
  stopRecording: () => invoke("stop_recording"),
  cancelRecording: () => invoke("cancel_recording"),
  retryTranscription: (recordingId) =>
    invoke("retry_transcription", { recordingId }),
  pasteTranscript: (recordingId) =>
    invoke("paste_transcript", { recordingId }),
  listHistory: (query) => invoke("list_history", { query }),
  deleteHistory: (recordingId) => invoke("delete_history", { recordingId }),
  listVocabulary: () => invoke("list_vocabulary"),
  addVocabulary: (heard, replacement) =>
    invoke("add_vocabulary", { heard, replacement }),
  deleteVocabulary: (id) => invoke("delete_vocabulary", { id }),
  listModes: () => invoke("list_modes"),
  upsertMode: (mode) => invoke("upsert_mode", { mode }),
  deleteMode: (id) => invoke("delete_mode", { id }),
  getSettings: () => invoke("get_settings"),
  getModelStatus: () => invoke("get_model_status"),
  updateSettings: (settings) => invoke("update_settings", { settings }),
  updateSettingValue: (key, value) =>
    invoke("update_setting_value", { key, value }),
  onState: (listener) =>
    listen<AppSnapshot>("dictation://state", (event) => listener(event.payload)),
  onLevel: (listener) =>
    listen<number>("dictation://level", (event) => listener(event.payload)),
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
  launchOnLogin: true,
  activeMode: "clean",
};

const demoHistory = (): Recording[] => [
  ["r1", "Przygotuj proszę podsumowanie dzisiejszego spotkania i wyślij je do zespołu.", "completed", 24_000, "Word"],
  ["r2", "Omówiliśmy harmonogram wdrożenia oraz kluczowe ryzyka.", "completed", 18_000, "Teams"],
  ["r3", "W załączniku przesyłam uwagi do umowy i propozycję zmian.", "completed", 12_000, "Outlook"],
  ["r4", null, "failed", 9_000, null],
  ["r5", "Sprawdź proszę najnowsze wymagania i daj znać, jeśli coś jest niejasne.", "completed", 17_000, "Edge"],
].map(([id, text, status, durationMs, sourceApp], index) => ({
  id: String(id),
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
    ["clean", "Czysty", "Naturalny tekst bez wypełniaczy.", "Usuń wypełniacze i popraw interpunkcję."],
    ["message", "Wiadomość", "Krótko, przyjaźnie i na temat.", "Zredaguj jako zwięzłą wiadomość."],
    ["code", "Kod", "Nazwy techniczne bez autokorekty.", "Zachowaj składnię i nazwy symboli."],
  ].map(([id, name, description, prompt], index) => ({
    id,
    name,
    description,
    prompt,
    enabled: true,
    isDefault: true,
    createdAt: index,
  }));

export function createBrowserAdapter(): AppAdapter {
  let settings = { ...initialSettings };
  let snapshot: AppSnapshot = { dictation: { status: "idle" }, settings };
  let history = demoHistory();
  let vocabulary: VocabularyEntry[] = [
    { id: 1, heard: "parakit", replacement: "Parakeet" },
  ];
  let modes = builtInModes();
  const stateListeners = new Set<Listener<AppSnapshot>>();
  const levelListeners = new Set<Listener<number>>();
  const emit = () => stateListeners.forEach((listener) => listener(snapshot));
  const setState = (dictation: AppSnapshot["dictation"]) => {
    snapshot = { dictation, settings };
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
      return setState({ status: "recording", recordingId: id, audioPath: `${id}.wav` });
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
        }, ...history];
        setState({ status: "idle" });
      }, 900);
      return snapshot;
    },
    cancelRecording: async () => setState({ status: "idle" }),
    retryTranscription: async (recordingId) => {
      const item = history.find((recording) => recording.id === recordingId);
      if (!item?.audioPath) throw new Error("Brak zachowanego audio.");
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
