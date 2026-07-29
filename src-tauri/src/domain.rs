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
        (DictationState::Recording { .. }, DictationEvent::Cancel) => DictationState::Idle,
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
}
