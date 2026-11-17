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

/** Legacy persisted preference. The UI always uses the fixed compact pill. */
export type OverlaySize = "mini" | "large";

export interface AppSettings {
  inputDevice: string | null;
  microphones?: MicrophonePreferences;
  shortcut: string;
  /** Older profiles use toggle when this preference is absent. */
  activationMode?: "toggle" | "push_to_talk";
  autoPaste: boolean;
  retentionDays: 1 | 7 | 30 | null;
  launchOnLogin: boolean;
  startMinimized: boolean;
  showOverlay: boolean;
  overlaySize: OverlaySize;
  model: string;
  streaming: boolean;
  /** Per-engine preview preference. Absent keys retain legacy defaults. */
  realtimeByEngine?: Record<string, boolean>;
  language: LanguageChoice;
  modelKeepAliveSecs: number;
  pasteMode: PasteMode;
  /** Background music/video while recording; calls are never touched. */
  mediaDuringDictation: MediaDuringDictation;
}

export type MediaDuringDictation = "muffle" | "duck" | "pause" | "off";

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
  source: "local";
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

/** Name, maker and readiness of the selected engine, as the backend sees it. */
export interface ModelSummary {
  key: string;
  display: string;
  provider: string;
  /** Ready to run: the model files are here. */
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

export interface LiveTranscript {
  recordingId: string;
  text: string;
  revision: number;
  status: "connecting" | "listening" | "draft" | "finalizing" | "fallback";
}

/** Only the current take may contribute a draft to a visible surface. */
export function matchingLiveTranscript(snapshot: AppSnapshot | undefined): LiveTranscript | null {
  if (!snapshot || (snapshot.dictation.status !== "recording" && snapshot.dictation.status !== "processing")) return null;
  return snapshot.liveTranscript?.recordingId === snapshot.dictation.recordingId ? snapshot.liveTranscript : null;
}

export function realtimeEngineKey(settings: AppSettings): string {
  return `local:${settings.model}`;
}

export function realtimeEnabled(settings: AppSettings, supported: boolean): boolean {
  if (!supported) return false;
  return settings.realtimeByEngine?.[realtimeEngineKey(settings)] ?? settings.model === "parakeet";
}

export interface OverlayNotice {
  key: string;
  generation: number;
  recordingId: string;
  kind: "pasteRequested" | "copied" | "cancelled" | "deliveryFailed" | "startFailed";
  expiresAt?: number | null;
  detail?: string | null;
  /** A delivered take whose microphone input was distorted or near silent. */
  micLevel?: "clipping" | "quiet" | null;
}

export interface AppSnapshot {
  overlayNotice?: OverlayNotice | null;
  dictation: DictationState;
  settings: AppSettings;
  download?: DownloadStatus | null;
  /** Absent only until the first snapshot arrives, which reads as "checking". */
  model?: ModelSummary;
  modelLoading: boolean;
  recordingStartedAt?: number | null;
  liveTranscript?: LiveTranscript | null;
  /** A setup step that failed without stopping the app, e.g. a taken shortcut. */
  startupWarning?: string | null;
}

export interface SettingsUpdateResult {
  settings: AppSettings;
  warning: string | null;
}

export interface InputDeviceInfo {
  id: string;
  name: string;
  isDefault: boolean;
  available: boolean;
}

export interface MicrophonePreferences {
  selection: "manual" | "favorites";
  selectedId: string | null;
  favorites: Array<{ id: string; name: string }>;
  hiddenIds: string[];
  allowSystemDefaultFallback: boolean;
  unresolvedLegacyName: string | null;
}

export interface InputDevicesChanged { generation: number; devices: InputDeviceInfo[] }

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
  term: string;
  /** Other spellings the engine produced for this term. */
  aliases: string[];
}

/** A word from history that sounds like a term but was left uncorrected. */
export interface VocabularySuggestion {
  termId: number;
  term: string;
  heard: string;
  count: number;
  example: string;
}

export type LanguageChoice = "system" | "pl" | "en";
