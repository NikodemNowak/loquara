import type { AppAdapter } from "../lib/tauri";
import type {
  AppSettings,
  AppSnapshot,
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
  showOverlay: true,
  overlaySize: "mini",
  model: "parakeet",
  streaming: true,
  realtimeByEngine: { "local:parakeet": true },
  language: "system",
  modelKeepAliveSecs: 0,
  pasteMode: "auto",
  mediaDuringDictation: "muffle",
};
export const snapshot: AppSnapshot = {
  dictation: { status: "idle" },
  settings,
  model: { key: "parakeet", display: "Parakeet TDT 0.6B v3", provider: "NVIDIA", installed: true, totalBytes: 670_478_772 },
  modelLoading: false,
};
export function adapterStub(overrides: Partial<AppAdapter> = {}): AppAdapter {
  const vocabulary: VocabularyEntry[] = [];
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
    addVocabulary: async (term, aliases) => {
      const index = vocabulary.findIndex((item) => item.term.toLowerCase() === term.toLowerCase());
      const item = index >= 0
        ? { ...vocabulary[index], aliases: [...new Set([...vocabulary[index].aliases, ...aliases])] }
        : { id: Math.max(0, ...vocabulary.map(({ id }) => id)) + 1, term, aliases };
      if (index >= 0) vocabulary[index] = item;
      else vocabulary.push(item);
      return item;
    },
    deleteVocabulary: async (id) => {
      const index = vocabulary.findIndex((item) => item.id === id);
      if (index >= 0) vocabulary.splice(index, 1);
      return index >= 0;
    },
    vocabularySuggestions: async () => [],
    getSettings: async () => currentSettings,
    getModelStatus: async () => ({
      state: "ready",
      model: "Parakeet TDT 0.6B v3",
      revision: "",
      device: null,
      message: null,
    }),
    listModels: async () => [
      { key: "parakeet", id: "nvidia/parakeet-tdt-0.6b-v3 (8-bit, block 32)", provider: "NVIDIA", source: "local", revision: "", display: "Parakeet TDT 0.6B v3", minVramGb: 0, minRamGb: 2, languages: "25 języków", estimatedSizeBytes: 670_478_772, status: "ready", installedSizeBytes: 670_478_772, message: null },
    ],
    downloadModel: async () => undefined,
    cancelDownload: async () => undefined,
    deleteModel: async () => undefined,
    updateSettings: async (next) => {
      currentSettings = { ...next };
      return { settings: currentSettings, warning: null };
    },
    updateSettingValue: async () => undefined,
    onState: async () => () => undefined,
    onLevel: async () => () => undefined,
    onModelProgress: async () => () => undefined,
    onInputDevicesChanged: async () => () => undefined,
    onError: async () => () => undefined,
    ...overrides,
  };
}
