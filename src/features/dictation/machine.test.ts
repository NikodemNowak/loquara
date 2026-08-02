import { describe, expect, test } from "vitest";

import {
  initialDictationState,
  transition,
  type DictationState,
} from "./machine";

const recording = {
  recordingId: "recording-1",
  audioPath: "C:\\recordings\\recording-1.wav",
};

describe("dictation state machine", () => {
  test("moves through a successful dictation cycle", () => {
    const recordingState = transition(initialDictationState, {
      type: "start",
      ...recording,
    });
    const processingState = transition(recordingState, { type: "stop" });
    const pastingState = transition(processingState, {
      type: "transcription_succeeded",
      transcript: "Dzień dobry.",
    });
    const idleState = transition(pastingState, { type: "paste_completed" });

    expect(recordingState).toEqual({ status: "recording", ...recording });
    expect(processingState).toEqual({ status: "processing", ...recording });
    expect(pastingState).toEqual({
      status: "pasting",
      ...recording,
      transcript: "Dzień dobry.",
    });
    expect(idleState).toBe(initialDictationState);
  });

  test("preserves the recording for recovery after transcription failure", () => {
    const processingState = transition(
      transition(initialDictationState, { type: "start", ...recording }),
      { type: "stop" },
    );

    expect(
      transition(processingState, {
        type: "transcription_failed",
        error: "Model jest niedostępny.",
      }),
    ).toEqual({
      status: "failed",
      recovery: recording,
      error: "Model jest niedostępny.",
    });
  });

  test("retries a failed recording without replacing its recovery data", () => {
    const failedState: DictationState = {
      status: "failed",
      recovery: recording,
      error: "Model jest niedostępny.",
    };

    expect(transition(failedState, { type: "retry" })).toEqual({
      status: "processing",
      ...recording,
    });
  });

  test("cancels recording and dismisses failure back to idle", () => {
    const recordingState: DictationState = {
      status: "recording",
      ...recording,
    };
    const failedState: DictationState = {
      status: "failed",
      recovery: recording,
      error: "Model jest niedostępny.",
    };

    expect(transition(recordingState, { type: "cancel" })).toBe(
      initialDictationState,
    );
    expect(transition(failedState, { type: "cancel" })).toBe(
      initialDictationState,
    );
  });

  test("arms and dismisses the cancel confirmation prompt", () => {
    const recordingState: DictationState = {
      status: "recording",
      ...recording,
    };

    expect(transition(recordingState, { type: "cancel_request" })).toEqual({
      status: "cancelling",
      ...recording,
    });

    const cancellingState: DictationState = {
      status: "cancelling",
      ...recording,
    };
    expect(transition(cancellingState, { type: "cancel_request" })).toEqual({
      status: "recording",
      ...recording,
    });
  });

  test("confirms cancel or finalizes from the cancelling prompt", () => {
    const cancellingState: DictationState = {
      status: "cancelling",
      ...recording,
    };

    expect(transition(cancellingState, { type: "cancel" })).toBe(
      initialDictationState,
    );
    expect(transition(cancellingState, { type: "stop" })).toEqual({
      status: "processing",
      ...recording,
    });
  });

  test("ignores events that are invalid for the current state", () => {
    const recordingState: DictationState = {
      status: "recording",
      ...recording,
    };

    expect(transition(recordingState, { type: "paste_completed" })).toBe(
      recordingState,
    );
  });
});
