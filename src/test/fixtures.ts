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
  },
];

export const settings: AppSettings = {
  inputDevice: null,
  shortcut: "Ctrl+Space",
  autoPaste: true,
  retentionDays: 30,
  launchOnLogin: true,
};

export const snapshot: AppSnapshot = {
  dictation: { status: "idle" },
  settings,
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
  return {
    getAppSnapshot: async () => snapshot,
    listInputDevices: async () => [
      { id: "default", name: "Mikrofon domyślny", isDefault: true },
    ],
    startRecording: async () => snapshot,
    stopRecording: async () => snapshot,
    cancelRecording: async () => snapshot,
    retryTranscription: async () => snapshot,
    pasteTranscript: async () => undefined,
    listHistory: async () => recordings,
    deleteHistory: async () => true,
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
    getSettings: async () => settings,
    updateSettings: async (next) => ({ settings: next, warning: null }),
    updateSettingValue: async () => undefined,
    onState: async () => () => undefined,
    onLevel: async () => () => undefined,
    onError: async () => () => undefined,
    ...overrides,
  };
}
