//! Types shared between the engine and the rest of the app.

use serde::{Deserialize, Serialize};

/// What one finished transcription produced.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct TranscriptionResult {
    pub text: String,
    pub model: String,
    #[serde(default)]
    pub language: Option<String>,
    pub duration_ms: u64,
}

/// Keeps a spawned process from flashing a console window on Windows.
///
/// Still needed for the handful of things Loquara shells out for, such as
/// revealing a recording in the file manager.
pub fn no_console(command: &mut std::process::Command) -> &mut std::process::Command {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        command.creation_flags(CREATE_NO_WINDOW);
    }
    command
}
