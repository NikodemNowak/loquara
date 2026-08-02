use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RecoveryRecording {
    pub recording_id: String,
    pub audio_path: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum DictationState {
    Idle,
    Recording {
        #[serde(rename = "recordingId")]
        recording_id: String,
        #[serde(rename = "audioPath")]
        audio_path: String,
    },
    Cancelling {
        #[serde(rename = "recordingId")]
        recording_id: String,
        #[serde(rename = "audioPath")]
        audio_path: String,
    },
    Processing {
        #[serde(rename = "recordingId")]
        recording_id: String,
        #[serde(rename = "audioPath")]
        audio_path: String,
    },
    Pasting {
        #[serde(rename = "recordingId")]
        recording_id: String,
        #[serde(rename = "audioPath")]
        audio_path: String,
        transcript: String,
    },
    Failed {
        recovery: RecoveryRecording,
        error: String,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum DictationEvent {
    Start {
        #[serde(rename = "recordingId")]
        recording_id: String,
        #[serde(rename = "audioPath")]
        audio_path: String,
    },
    Stop,
    TranscriptionSucceeded {
        transcript: String,
    },
    TranscriptionFailed {
        error: String,
    },
    PasteCompleted,
    Retry,
    Cancel,
    CancelRequest,
}

pub fn transition(state: DictationState, event: DictationEvent) -> DictationState {
    match (state, event) {
        (
            DictationState::Idle,
            DictationEvent::Start {
                recording_id,
                audio_path,
            },
        ) => DictationState::Recording {
            recording_id,
            audio_path,
        },
        (
            DictationState::Recording {
                recording_id,
                audio_path,
            },
            DictationEvent::Stop,
        ) => DictationState::Processing {
            recording_id,
            audio_path,
        },
        (
            DictationState::Recording {
                recording_id,
                audio_path,
            },
            DictationEvent::CancelRequest,
        ) => DictationState::Cancelling {
            recording_id,
            audio_path,
        },
        (DictationState::Recording { .. }, DictationEvent::Cancel) => DictationState::Idle,
        (
            DictationState::Cancelling {
                recording_id,
                audio_path,
            },
            DictationEvent::CancelRequest,
        ) => DictationState::Recording {
            recording_id,
            audio_path,
        },
        (DictationState::Cancelling { .. }, DictationEvent::Cancel) => DictationState::Idle,
        (
            DictationState::Cancelling {
                recording_id,
                audio_path,
            },
            DictationEvent::Stop,
        ) => DictationState::Processing {
            recording_id,
            audio_path,
        },
        (
            DictationState::Processing {
                recording_id,
                audio_path,
            },
            DictationEvent::TranscriptionSucceeded { transcript },
        ) => DictationState::Pasting {
            recording_id,
            audio_path,
            transcript,
        },
        (
            DictationState::Processing {
                recording_id,
                audio_path,
            },
            DictationEvent::TranscriptionFailed { error },
        ) => DictationState::Failed {
            recovery: RecoveryRecording {
                recording_id,
                audio_path,
            },
            error,
        },
        (DictationState::Pasting { .. }, DictationEvent::PasteCompleted) => DictationState::Idle,
        (DictationState::Failed { recovery, .. }, DictationEvent::Retry) => {
            DictationState::Processing {
                recording_id: recovery.recording_id,
                audio_path: recovery.audio_path,
            }
        }
        (DictationState::Failed { .. }, DictationEvent::Cancel) => DictationState::Idle,
        (state, _) => state,
    }
}

#[cfg(test)]
mod tests {
    use super::{DictationEvent, DictationState, RecoveryRecording, transition};
    use serde_json::{Value, json};

    fn recording() -> RecoveryRecording {
        RecoveryRecording {
            recording_id: "recording-1".into(),
            audio_path: r"C:\recordings\recording-1.wav".into(),
        }
    }

    #[test]
    fn moves_through_a_successful_dictation_cycle() {
        let recording = recording();
        let state = transition(
            DictationState::Idle,
            DictationEvent::Start {
                recording_id: recording.recording_id.clone(),
                audio_path: recording.audio_path.clone(),
            },
        );
        let state = transition(state, DictationEvent::Stop);
        let state = transition(
            state,
            DictationEvent::TranscriptionSucceeded {
                transcript: "Dzień dobry.".into(),
            },
        );

        assert_eq!(
            state,
            DictationState::Pasting {
                recording_id: recording.recording_id,
                audio_path: recording.audio_path,
                transcript: "Dzień dobry.".into(),
            }
        );
        assert_eq!(
            transition(state, DictationEvent::PasteCompleted),
            DictationState::Idle
        );
    }

    #[test]
    fn preserves_the_recording_for_recovery_after_transcription_failure() {
        let recovery = recording();
        let processing = DictationState::Processing {
            recording_id: recovery.recording_id.clone(),
            audio_path: recovery.audio_path.clone(),
        };

        assert_eq!(
            transition(
                processing,
                DictationEvent::TranscriptionFailed {
                    error: "Model jest niedostępny.".into(),
                },
            ),
            DictationState::Failed {
                recovery,
                error: "Model jest niedostępny.".into(),
            }
        );
    }

    #[test]
    fn retries_a_failed_recording_without_replacing_recovery_data() {
        let recovery = recording();
        let failed = DictationState::Failed {
            recovery: recovery.clone(),
            error: "Model jest niedostępny.".into(),
        };

        assert_eq!(
            transition(failed, DictationEvent::Retry),
            DictationState::Processing {
                recording_id: recovery.recording_id,
                audio_path: recovery.audio_path,
            }
        );
    }

    #[test]
    fn cancels_recording_and_dismisses_failure_back_to_idle() {
        let recovery = recording();
        let recording_state = DictationState::Recording {
            recording_id: recovery.recording_id.clone(),
            audio_path: recovery.audio_path.clone(),
        };
        let failed_state = DictationState::Failed {
            recovery,
            error: "Model jest niedostępny.".into(),
        };

        assert_eq!(
            transition(recording_state, DictationEvent::Cancel),
            DictationState::Idle
        );
        assert_eq!(
            transition(failed_state, DictationEvent::Cancel),
            DictationState::Idle
        );
    }

    #[test]
    fn ignores_events_that_are_invalid_for_the_current_state() {
        let recovery = recording();
        let recording_state = DictationState::Recording {
            recording_id: recovery.recording_id,
            audio_path: recovery.audio_path,
        };

        assert_eq!(
            transition(recording_state.clone(), DictationEvent::PasteCompleted),
            recording_state
        );
    }

    #[test]
    fn cancel_request_arms_then_dismisses_the_confirm_prompt() {
        let recovery = recording();
        let recording_state = DictationState::Recording {
            recording_id: recovery.recording_id.clone(),
            audio_path: recovery.audio_path.clone(),
        };
        let cancelling_state = DictationState::Cancelling {
            recording_id: recovery.recording_id.clone(),
            audio_path: recovery.audio_path.clone(),
        };

        assert_eq!(
            transition(recording_state.clone(), DictationEvent::CancelRequest),
            cancelling_state.clone()
        );
        assert_eq!(
            transition(cancelling_state.clone(), DictationEvent::CancelRequest),
            recording_state
        );
    }

    #[test]
    fn cancelling_can_confirm_to_idle_or_finalize_to_processing() {
        let recovery = recording();
        let cancelling_state = DictationState::Cancelling {
            recording_id: recovery.recording_id.clone(),
            audio_path: recovery.audio_path.clone(),
        };

        assert_eq!(
            transition(cancelling_state.clone(), DictationEvent::Cancel),
            DictationState::Idle
        );
        assert_eq!(
            transition(cancelling_state, DictationEvent::Stop),
            DictationState::Processing {
                recording_id: recovery.recording_id,
                audio_path: recovery.audio_path,
            }
        );
    }

    #[test]
    fn state_json_contract_uses_exact_tags_fields_and_round_trips() {
        let cases: Vec<(DictationState, Value)> = vec![
            (DictationState::Idle, json!({ "status": "idle" })),
            (
                DictationState::Recording {
                    recording_id: "recording-1".into(),
                    audio_path: r"C:\recordings\recording-1.wav".into(),
                },
                json!({
                    "status": "recording",
                    "recordingId": "recording-1",
                    "audioPath": r"C:\recordings\recording-1.wav",
                }),
            ),
            (
                DictationState::Cancelling {
                    recording_id: "recording-1".into(),
                    audio_path: r"C:\recordings\recording-1.wav".into(),
                },
                json!({
                    "status": "cancelling",
                    "recordingId": "recording-1",
                    "audioPath": r"C:\recordings\recording-1.wav",
                }),
            ),
            (
                DictationState::Processing {
                    recording_id: "recording-1".into(),
                    audio_path: r"C:\recordings\recording-1.wav".into(),
                },
                json!({
                    "status": "processing",
                    "recordingId": "recording-1",
                    "audioPath": r"C:\recordings\recording-1.wav",
                }),
            ),
            (
                DictationState::Pasting {
                    recording_id: "recording-1".into(),
                    audio_path: r"C:\recordings\recording-1.wav".into(),
                    transcript: "Dzień dobry.".into(),
                },
                json!({
                    "status": "pasting",
                    "recordingId": "recording-1",
                    "audioPath": r"C:\recordings\recording-1.wav",
                    "transcript": "Dzień dobry.",
                }),
            ),
            (
                DictationState::Failed {
                    recovery: recording(),
                    error: "Model jest niedostępny.".into(),
                },
                json!({
                    "status": "failed",
                    "recovery": {
                        "recordingId": "recording-1",
                        "audioPath": r"C:\recordings\recording-1.wav",
                    },
                    "error": "Model jest niedostępny.",
                }),
            ),
        ];

        for (state, expected_json) in cases {
            assert_eq!(serde_json::to_value(&state).unwrap(), expected_json);
            assert_eq!(
                serde_json::from_value::<DictationState>(expected_json).unwrap(),
                state
            );
        }
    }

    #[test]
    fn event_json_contract_uses_exact_tags_fields_and_round_trips() {
        let cases: Vec<(DictationEvent, Value)> = vec![
            (
                DictationEvent::Start {
                    recording_id: "recording-1".into(),
                    audio_path: r"C:\recordings\recording-1.wav".into(),
                },
                json!({
                    "type": "start",
                    "recordingId": "recording-1",
                    "audioPath": r"C:\recordings\recording-1.wav",
                }),
            ),
            (DictationEvent::Stop, json!({ "type": "stop" })),
            (
                DictationEvent::TranscriptionSucceeded {
                    transcript: "Dzień dobry.".into(),
                },
                json!({
                    "type": "transcription_succeeded",
                    "transcript": "Dzień dobry.",
                }),
            ),
            (
                DictationEvent::TranscriptionFailed {
                    error: "Model jest niedostępny.".into(),
                },
                json!({
                    "type": "transcription_failed",
                    "error": "Model jest niedostępny.",
                }),
            ),
            (
                DictationEvent::PasteCompleted,
                json!({ "type": "paste_completed" }),
            ),
            (DictationEvent::Retry, json!({ "type": "retry" })),
            (DictationEvent::Cancel, json!({ "type": "cancel" })),
            (DictationEvent::CancelRequest, json!({ "type": "cancel_request" })),
        ];

        for (event, expected_json) in cases {
            assert_eq!(serde_json::to_value(&event).unwrap(), expected_json);
            assert_eq!(
                serde_json::from_value::<DictationEvent>(expected_json).unwrap(),
                event
            );
        }
    }
}
