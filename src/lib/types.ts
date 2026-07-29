export interface RecoveryRecording {
  recordingId: string;
  audioPath: string;
}

export type DictationState =
  | { status: "idle" }
  | ({ status: "recording" } & RecoveryRecording)
  | ({ status: "processing" } & RecoveryRecording)
  | ({ status: "pasting"; transcript: string } & RecoveryRecording)
  | { status: "failed"; recovery: RecoveryRecording; error: string };

export type DictationEvent =
  | ({ type: "start" } & RecoveryRecording)
  | { type: "stop" }
  | { type: "transcription_succeeded"; transcript: string }
  | { type: "transcription_failed"; error: string }
  | { type: "paste_completed" }
  | { type: "retry" }
  | { type: "cancel" };

export interface AppSettings {
  inputDevice: string | null;
  shortcut: string;
  autoPaste: boolean;
  retentionDays: 1 | 7 | 30 | null;
  launchOnLogin: boolean;
}

export interface AppSnapshot {
  dictation: DictationState;
  settings: AppSettings;
}

export interface SettingsUpdateResult {
  settings: AppSettings;
  warning: string | null;
}

export interface InputDeviceInfo {
  id: string;
  name: string;
  isDefault: boolean;
}

export type RecordingStatus =
  | "recording"
  | "processing"
  | "completed"
  | "failed"
  | "cancelled";

export interface Recording {
  id: string;
  createdAt: number;
  durationMs: number;
  status: RecordingStatus;
  text: string | null;
  model: string | null;
  audioPath: string | null;
  sourceApp: string | null;
  error: string | null;
}

export interface HistoryQuery {
  search?: string | null;
  status?: RecordingStatus | null;
}

export interface VocabularyEntry {
  id: number;
  heard: string;
  replacement: string;
}

export interface Mode {
  id: string;
  name: string;
  description: string;
  prompt: string;
  enabled: boolean;
  isDefault: boolean;
  createdAt: number;
}

export type ThemeChoice = "system" | "light" | "dark";
