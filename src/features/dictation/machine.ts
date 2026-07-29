import type { DictationEvent, DictationState } from "../../lib/types";

export type { DictationEvent, DictationState } from "../../lib/types";

export const initialDictationState: DictationState = { status: "idle" };

export function transition(
  state: DictationState,
  event: DictationEvent,
): DictationState {
  switch (state.status) {
    case "idle":
      return event.type === "start"
        ? {
            status: "recording",
            recordingId: event.recordingId,
            audioPath: event.audioPath,
          }
        : state;
    case "recording":
      if (event.type === "stop") {
        return {
          status: "processing",
          recordingId: state.recordingId,
          audioPath: state.audioPath,
        };
      }
      return event.type === "cancel" ? initialDictationState : state;
    case "processing":
      if (event.type === "transcription_succeeded") {
        return {
          status: "pasting",
          recordingId: state.recordingId,
          audioPath: state.audioPath,
          transcript: event.transcript,
        };
      }
      if (event.type === "transcription_failed") {
        return {
          status: "failed",
          recovery: {
            recordingId: state.recordingId,
            audioPath: state.audioPath,
          },
          error: event.error,
        };
      }
      return state;
    case "pasting":
      return event.type === "paste_completed" ? initialDictationState : state;
    case "failed":
      if (event.type === "retry") {
        return {
          status: "processing",
          recordingId: state.recovery.recordingId,
          audioPath: state.recovery.audioPath,
        };
      }
      return event.type === "cancel" ? initialDictationState : state;
  }
}
