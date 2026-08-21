import type { AppAdapter } from "../lib/tauri";
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
  autoPaste: true,
  retentionDays: 30,
  launchOnLogin: true,
  activeMode: "clean",
  showOverlay: true,
  model: "parakeet",
  streaming: true,
  language: "system",
  dictationLanguage: "auto",
  modelKeepAliveSecs: 0,
};
export const snapshot: AppSnapshot = {
  dictation: { status: "idle" },
  settings,
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
      { id: "default", name: "Mikrofon domyślny", isDefault: true },
    ],
    engineStatus: async () => ({
      python: true,
      pythonCommand: "python",
      dependencies: true,
      torch: true,
      requirementsPath: "C:\\Loquara\\engine\\requirements.txt",
    }),
    hfAccount: async () => ({ connected: false, name: null }),
    connectHfAccount: async () => ({ connected: true, name: "tester" }),
    disconnectHfAccount: async () => ({ connected: false, name: null }),
    startRecording: async () => snapshot,
    stopRecording: async () => snapshot,
    cancelRecording: async () => snapshot,
    requestCancel: async () => snapshot,
    hideOverlay: async () => undefined,
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
      model: "nvidia/parakeet-tdt-0.6b-v3",
      revision: "7c35754d166cca382ad1e53e68b01e7c575f3a1d",
      device: null,
      message: null,
    }),
    listModels: async () => [
      { key: "parakeet", id: "nvidia/parakeet-tdt-0.6b-v3", provider: "NVIDIA", source: "local", revision: "7c35754d166cca382ad1e53e68b01e7c575f3a1d", display: "Parakeet TDT 0.6B v3", minVramGb: 3, minRamGb: 8, languages: "auto (PL/EN)", estimatedSizeBytes: 2_500_000_000, status: "ready", installedSizeBytes: 2_500_000_000, message: null },
      { key: "whisper-turbo", id: "openai/whisper-large-v3-turbo", provider: "OpenAI", source: "local", revision: "41f01f3fe87f28c78e2fbf8b568835947dd65ed9", display: "Whisper Large v3 Turbo", minVramGb: 4, minRamGb: 8, languages: "auto (99)", estimatedSizeBytes: 1_600_000_000, status: "not_installed", installedSizeBytes: null, message: null },
      { key: "whisper-small", id: "openai/whisper-small", provider: "OpenAI", source: "local", revision: "973afd24965f72e36ca33b3055d56a652f456b4d", display: "Whisper Small", minVramGb: 1.5, minRamGb: 4, languages: "auto (99)", estimatedSizeBytes: 1_000_000_000, status: "not_installed", installedSizeBytes: null, message: null },
      { key: "cohere", id: "AEmotionStudio/cohere-transcribe-03-2026-models", provider: "Cohere", source: "local", revision: "d114f701a80b2150943f5dbae71458f4d1fcb37b", display: "Cohere Transcribe 2B", minVramGb: 5, minRamGb: 8, languages: "pl/en/fr/de/...", estimatedSizeBytes: 4_132_000_000, status: "not_installed", installedSizeBytes: null, message: null },
    ],
    downloadModel: async () => undefined,
    deleteModel: async () => undefined,
    updateSettings: async (next) => {
      currentSettings = { ...next };
      return { settings: currentSettings, warning: null };
    },
    updateSettingValue: async () => undefined,
    onState: async () => () => undefined,
    onLevel: async () => () => undefined,
    onModelProgress: async () => () => undefined,
    onError: async () => () => undefined,
    ...overrides,
  };
}
