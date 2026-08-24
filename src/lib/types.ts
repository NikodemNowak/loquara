export interface RecoveryRecording {
  recordingId: string;
  audioPath: string;
}

export type DictationState =
  | { status: "idle" }
  | ({ status: "recording" } & RecoveryRecording)
  | ({ status: "cancelling" } & RecoveryRecording)
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
  | { type: "cancel" }
  | { type: "cancel_request" };

export type PasteMode = "auto" | "ctrl_v" | "ctrl_shift_v" | "shift_insert";

export interface AppSettings {
  inputDevice: string | null;
  shortcut: string;
  autoPaste: boolean;
  retentionDays: 1 | 7 | 30 | null;
  launchOnLogin: boolean;
  startMinimized: boolean;
  activeMode: string;
  showOverlay: boolean;
  model: string;
  streaming: boolean;
  language: LanguageChoice;
  modelKeepAliveSecs: number;
  pasteMode: PasteMode;
}

export interface ModelStatus {
  state: "ready" | "not_installed" | "error";
  model: string;
  revision: string;
  device: string | null;
  message: string | null;
}

export interface ModelDownloadProgress {
  model: string;
  phase: "preparing" | "downloading" | "validating";
  downloadedBytes: number;
  totalBytes: number | null;
}

export interface ModelDescriptor {
  key: string;
  id: string;
  provider: string;
  source: "local" | "cloud";
  revision: string;
  display: string;
  minVramGb: number;
  minRamGb: number;
  languages: string;
  estimatedSizeBytes: number;
  status: ModelStatus["state"];
  installedSizeBytes: number | null;
  message: string | null;
}

export interface PlatformError {
  code: string;
  message?: string;
}

/** Name, maker and readiness of the selected model, as the backend sees it. */
export interface ModelSummary {
  key: string;
  display: string;
  provider: string;
  installed: boolean;
  /** What fetching it will cost, so the size can be shown before asking. */
  totalBytes: number;
}

/** A model transfer in flight, owned by the backend so any screen can show it. */
export interface DownloadStatus {
  model: string;
  downloadedBytes: number;
  totalBytes: number;
}

export interface AppSnapshot {
  dictation: DictationState;
  settings: AppSettings;
  download?: DownloadStatus | null;
  /** Absent only until the first snapshot arrives, which reads as "checking". */
  model?: ModelSummary;
  modelLoading: boolean;
  recordingStartedAt?: number | null;
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
  /** Amplitude envelope, one byte per bucket (0-255). Null for recordings
   *  captured before envelopes were stored, and while one is still running. */
  peaks: number[] | null;
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


export type LanguageChoice = "system" | "pl" | "en";
