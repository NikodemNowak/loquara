import { demoCloudProviders, type AppAdapter } from "../lib/tauri";
import type {
  AppSettings,
  AppSnapshot,
  Mode,
  Recording,
  VocabularyEntry,
} from "../lib/types";
export const recordings: Recording[] = [
  {
    id: "failed-1",
    createdAt: 1_786_000_000_000,
    durationMs: 14_000,
    status: "failed",
    text: null,
    model: "NVIDIA Parakeet 0.6B v3",
    audioPath: "C:\\Mow\\failed-1.wav",
    sourceApp: "Word",
    error: "Model chwilowo niedostępny",
    peaks: [40, 180, 90, 220, 60],
  },
  {
    id: "active-1",
    createdAt: 1_786_000_100_000,
    durationMs: 4_000,
    status: "recording",
    text: null,
    model: null,
    audioPath: "C:\\Mow\\active-1.wav",
    sourceApp: null,
    error: null,
    // Still capturing, so the envelope does not exist yet.
    peaks: null,
  },
  {
    id: "complete-1",
    createdAt: 1_786_000_200_000,
    durationMs: 24_000,
    status: "completed",
    text: "Przygotuj proszę podsumowanie spotkania.",
    model: "NVIDIA Parakeet 0.6B v3",
    audioPath: "C:\\Mow\\complete-1.wav",
    sourceApp: "Outlook",
    error: null,
    peaks: [10, 120, 200, 255, 140, 60, 20],
  },
];
export const settings: AppSettings = {
  inputDevice: null,
  shortcut: "Ctrl+Space",
  activationMode: "toggle",
  autoPaste: true,
  retentionDays: 30,
  launchOnLogin: true,
  startMinimized: false,
  activeMode: "clean",
  showOverlay: true,
  overlaySize: "mini",
  model: "parakeet",
  streaming: true,
  realtimeByEngine: { "local:parakeet": true },
  language: "system",
  modelKeepAliveSecs: 0,
  pasteMode: "auto",
  transcriptionSource: "local",
  cloudProvider: "xai",
  cloudModel: "",
  cloudBaseUrl: "",
  cloudLanguage: "",
};
export const snapshot: AppSnapshot = {
  dictation: { status: "idle" },
  settings,
  model: { key: "parakeet", display: "Parakeet TDT 0.6B v3", provider: "NVIDIA", installed: true, totalBytes: 670_478_772, source: "local" },
  modelLoading: false,
};
export function adapterStub(overrides: Partial<AppAdapter> = {}): AppAdapter {
  const vocabulary: VocabularyEntry[] = [];
  const modes: Mode[] = [
    {
      id: "clean",
      name: "Czysty",
      description: "Naturalny tekst bez wypełniaczy.",
      prompt: "Usuń wypełniacze.",
      enabled: true,
      isDefault: true,
      createdAt: 1,
    },
  ];
  let currentSettings = { ...settings };
  return {
    getAppSnapshot: async () => snapshot,
    listInputDevices: async () => [
      { id: "default", name: "Mikrofon domyślny", isDefault: true, available: true },
    ],
    startRecording: async () => snapshot,
    stopRecording: async () => snapshot,
    cancelRecording: async () => snapshot,
    requestCancel: async () => snapshot,
    hideOverlay: async () => undefined,
    dismissOverlayNotice: async () => snapshot,
    openOverlayDetails: async () => undefined,
    saveOverlayPosition: async () => undefined,
    setShortcutSuspended: async () => undefined,
    retryTranscription: async () => snapshot,
    pasteTranscript: async () => undefined,
    listHistory: async () => recordings,
    deleteHistory: async () => true,
    exportTranscript: async () => "exported.txt",
    clearFailedRecordings: async () => 0,
    playRecording: async () => undefined,
    getRecordingAudio: async () => new Uint8Array(0),
    revealRecording: async () => undefined,
    openRecordingsFolder: async () => undefined,
    openModelFolder: async () => undefined,
    correctTranscript: async () => 0,
    listVocabulary: async () => vocabulary,
    addVocabulary: async (heard, replacement) => {
      const item = { id: vocabulary.length + 1, heard, replacement };
      vocabulary.push(item);
      return item;
    },
    deleteVocabulary: async (id) => {
      const index = vocabulary.findIndex((item) => item.id === id);
      if (index >= 0) vocabulary.splice(index, 1);
      return index >= 0;
    },
    listModes: async () => modes,
    upsertMode: async (mode) => {
      const index = modes.findIndex((item) => item.id === mode.id);
      if (index >= 0) modes[index] = mode;
      else modes.push(mode);
    },
    deleteMode: async (id) => {
      const index = modes.findIndex((item) => item.id === id);
      if (index >= 0) modes.splice(index, 1);
      return index >= 0;
    },
    getSettings: async () => currentSettings,
    getModelStatus: async () => ({
      state: "ready",
      model: "Parakeet TDT 0.6B v3",
      revision: "",
      device: null,
      message: null,
    }),
    listModels: async () => [
      { key: "parakeet", id: "csukuangfj/sherpa-onnx-nemo-parakeet-tdt-0.6b-v3-int8", provider: "NVIDIA", source: "local", revision: "", display: "Parakeet TDT 0.6B v3", minVramGb: 0, minRamGb: 2, languages: "25 języków", estimatedSizeBytes: 670_478_772, status: "ready", installedSizeBytes: 670_478_772, message: null },
    ],
    downloadModel: async () => undefined,
    cancelDownload: async () => undefined,
    deleteModel: async () => undefined,
    updateSettings: async (next) => {
      currentSettings = { ...next };
      return { settings: currentSettings, warning: null };
    },
    updateSettingValue: async () => undefined,
    listCloudProviders: async () => demoCloudProviders,
    listCloudKeys: async () => [],
    listCloudModels: async () => [],
    setCloudApiKey: async () => [],
    clearCloudApiKey: async () => [],
    testCloudTranscription: async () => ({ text: "", elapsedMs: 0 }),
    onState: async () => () => undefined,
    onLevel: async () => () => undefined,
    onModelProgress: async () => () => undefined,
    onInputDevicesChanged: async () => () => undefined,
    onError: async () => () => undefined,
    ...overrides,
  };
}
