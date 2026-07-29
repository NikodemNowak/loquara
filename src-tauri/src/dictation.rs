use crate::audio::{AudioRecorder, InputDeviceInfo, cleanup_partial};
use crate::domain::{DictationEvent, DictationState, transition};
use crate::platform::{self, SystemWindows, WindowTarget, WindowsApi};
use crate::storage::{
    HistoryQuery, Mode, Recording, RecordingStatus, Retention, Storage, VocabularyEntry,
    postprocess,
};
use crate::transcription::{ClientError, TranscriptionResult, WorkerClient};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tauri::{AppHandle, Emitter, State};
use thiserror::Error;

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum CoordinatorError {
    #[error("the requested operation is invalid for the current state")]
    InvalidState,
}

pub struct CoordinatorMachine {
    state: DictationState,
}

impl Default for CoordinatorMachine {
    fn default() -> Self {
        Self {
            state: DictationState::Idle,
        }
    }
}

impl CoordinatorMachine {
    pub fn snapshot(&self) -> DictationState {
        self.state.clone()
    }

    pub fn started(
        &mut self,
        recording_id: impl Into<String>,
        audio_path: impl Into<String>,
    ) -> Result<(), CoordinatorError> {
        if self.state != DictationState::Idle {
            return Err(CoordinatorError::InvalidState);
        }
        self.state = transition(
            self.state.clone(),
            DictationEvent::Start {
                recording_id: recording_id.into(),
                audio_path: audio_path.into(),
            },
        );
        Ok(())
    }

    pub fn stopped(&mut self) -> Result<(), CoordinatorError> {
        if !matches!(self.state, DictationState::Recording { .. }) {
            return Err(CoordinatorError::InvalidState);
        }
        self.state = transition(self.state.clone(), DictationEvent::Stop);
        Ok(())
    }

    pub fn transcription_failed(
        &mut self,
        error: impl Into<String>,
    ) -> Result<(), CoordinatorError> {
        if !matches!(self.state, DictationState::Processing { .. }) {
            return Err(CoordinatorError::InvalidState);
        }
        self.state = transition(
            self.state.clone(),
            DictationEvent::TranscriptionFailed {
                error: error.into(),
            },
        );
        Ok(())
    }

    pub fn transcription_succeeded(
        &mut self,
        transcript: impl Into<String>,
    ) -> Result<(), CoordinatorError> {
        if !matches!(self.state, DictationState::Processing { .. }) {
            return Err(CoordinatorError::InvalidState);
        }
        self.state = transition(
            self.state.clone(),
            DictationEvent::TranscriptionSucceeded {
                transcript: transcript.into(),
            },
        );
        Ok(())
    }

    pub fn retry(&mut self) -> Result<(), CoordinatorError> {
        if !matches!(self.state, DictationState::Failed { .. }) {
            return Err(CoordinatorError::InvalidState);
        }
        self.state = transition(self.state.clone(), DictationEvent::Retry);
        Ok(())
    }

    pub fn retry_recording(
        &mut self,
        recording_id: impl Into<String>,
        audio_path: impl Into<String>,
    ) -> Result<(), CoordinatorError> {
        if !matches!(
            self.state,
            DictationState::Idle | DictationState::Failed { .. }
        ) {
            return Err(CoordinatorError::InvalidState);
        }
        self.state = DictationState::Processing {
            recording_id: recording_id.into(),
            audio_path: audio_path.into(),
        };
        Ok(())
    }

    pub fn cancel(&mut self) -> Result<(), CoordinatorError> {
        if !matches!(
            self.state,
            DictationState::Recording { .. } | DictationState::Failed { .. }
        ) {
            return Err(CoordinatorError::InvalidState);
        }
        self.state = transition(self.state.clone(), DictationEvent::Cancel);
        Ok(())
    }

    pub fn paste_completed(&mut self) -> Result<(), CoordinatorError> {
        if !matches!(self.state, DictationState::Pasting { .. }) {
            return Err(CoordinatorError::InvalidState);
        }
        self.state = transition(self.state.clone(), DictationEvent::PasteCompleted);
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AppSettings {
    pub input_device: Option<String>,
    pub shortcut: String,
    pub auto_paste: bool,
    pub retention_days: Option<u32>,
    #[serde(default = "default_launch_on_login")]
    pub launch_on_login: bool,
}

const fn default_launch_on_login() -> bool {
    true
}

impl Default for AppSettings {
    fn default() -> Self {
        Self {
            input_device: None,
            shortcut: "Ctrl+Space".into(),
            auto_paste: true,
            retention_days: Some(30),
            launch_on_login: true,
        }
    }
}

fn retention_policy(days: Option<u32>) -> Result<Retention, String> {
    match days {
        None => Ok(Retention::Forever),
        Some(days @ (1 | 7 | 30)) => Ok(Retention::Days(days)),
        Some(days) => Err(format!("unsupported retention period: {days} days")),
    }
}

pub fn cleanup_retention(state: &AppState) -> Result<usize, String> {
    let retention = retention_policy(
        state
            .settings
            .read()
            .map_err(|_| "settings lock poisoned")?
            .retention_days,
    )?;
    state
        .storage
        .cleanup_retention(retention, epoch_ms())
        .map(|report| report.deleted_audio)
        .map_err(|error| error.to_string())
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AppSnapshot {
    pub dictation: DictationState,
    pub settings: AppSettings,
}

#[derive(Clone)]
pub struct AppState {
    pub machine: Arc<Mutex<CoordinatorMachine>>,
    pub audio: Arc<AudioRecorder>,
    pub storage: Arc<Storage>,
    pub worker: Arc<Mutex<Option<WorkerClient>>>,
    pub target_window: Arc<Mutex<Option<WindowTarget>>>,
    pub settings: Arc<RwLock<AppSettings>>,
    python: String,
    worker_path: PathBuf,
}

pub trait TerminalStatusWriter {
    fn write_terminal_status(
        &self,
        recording_id: &str,
        status: RecordingStatus,
        error: Option<&str>,
    ) -> Result<(), String>;
}

impl TerminalStatusWriter for Storage {
    fn write_terminal_status(
        &self,
        recording_id: &str,
        status: RecordingStatus,
        error: Option<&str>,
    ) -> Result<(), String> {
        self.update_status(recording_id, status, None, None, None, error)
            .map(|_| ())
            .map_err(|error| error.to_string())
    }
}

#[derive(Clone, Debug)]
pub enum TerminalOutcome {
    Failed(String),
    Cancelled,
}

pub fn recover_terminal_state(
    machine: &Mutex<CoordinatorMachine>,
    history: &impl TerminalStatusWriter,
    recording_id: &str,
    audio_path: &str,
    outcome: TerminalOutcome,
) -> Result<(), String> {
    let (status, error) = match &outcome {
        TerminalOutcome::Failed(error) => (RecordingStatus::Failed, Some(error.as_str())),
        TerminalOutcome::Cancelled => (RecordingStatus::Cancelled, None),
    };
    let storage_result = history.write_terminal_status(recording_id, status, error);
    let mut machine = machine
        .lock()
        .map_err(|_| "coordinator lock poisoned".to_owned())?;
    machine.state = match outcome {
        TerminalOutcome::Failed(error) => DictationState::Failed {
            recovery: crate::domain::RecoveryRecording {
                recording_id: recording_id.to_owned(),
                audio_path: audio_path.to_owned(),
            },
            error,
        },
        TerminalOutcome::Cancelled => DictationState::Idle,
    };
    storage_result
}

impl AppState {
    pub fn new(
        audio: AudioRecorder,
        storage: Storage,
        python: impl Into<String>,
        worker_path: impl Into<PathBuf>,
    ) -> Self {
        let settings = storage
            .get_setting::<AppSettings>("app")
            .ok()
            .flatten()
            .unwrap_or_default();
        Self {
            machine: Arc::new(Mutex::new(CoordinatorMachine::default())),
            audio: Arc::new(audio),
            storage: Arc::new(storage),
            worker: Arc::new(Mutex::new(None)),
            target_window: Arc::new(Mutex::new(None)),
            settings: Arc::new(RwLock::new(settings)),
            python: python.into(),
            worker_path: worker_path.into(),
        }
    }

    fn snapshot(&self) -> Result<AppSnapshot, String> {
        Ok(AppSnapshot {
            dictation: self
                .machine
                .lock()
                .map_err(|_| "coordinator lock poisoned")?
                .snapshot(),
            settings: self
                .settings
                .read()
                .map_err(|_| "settings lock poisoned")?
                .clone(),
        })
    }
}

fn epoch_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .try_into()
        .unwrap_or(i64::MAX)
}

fn emit_state(app: &AppHandle, state: &AppState) {
    if let Ok(snapshot) = state.snapshot() {
        let _ = app.emit("dictation://state", snapshot);
    }
}

#[tauri::command]
pub fn get_app_snapshot(state: State<'_, AppState>) -> Result<AppSnapshot, String> {
    state.snapshot()
}

#[tauri::command]
pub fn list_input_devices(state: State<'_, AppState>) -> Result<Vec<InputDeviceInfo>, String> {
    state
        .audio
        .list_devices()
        .map_err(|error| error.to_string())
}

pub fn start_recording_inner(app: &AppHandle, state: &AppState) -> Result<AppSnapshot, String> {
    if !matches!(state.snapshot()?.dictation, DictationState::Idle) {
        return Err(CoordinatorError::InvalidState.to_string());
    }
    let mut windows = SystemWindows;
    let target = windows.foreground_window();
    let device = state
        .settings
        .read()
        .map_err(|_| "settings lock poisoned")?
        .input_device
        .clone();
    let started = state
        .audio
        .start(device.as_deref())
        .map_err(|error| error.to_string())?;
    let audio_path = started.path.to_string_lossy().into_owned();
    let recording = Recording {
        id: started.id.clone(),
        created_at: epoch_ms(),
        duration_ms: 0,
        status: RecordingStatus::Recording,
        text: None,
        model: None,
        audio_path: Some(audio_path.clone()),
        source_app: None,
        error: None,
    };
    if let Err(error) = state.storage.insert_recording(&recording) {
        let _ = state.audio.cancel();
        return Err(error.to_string());
    }
    *state
        .target_window
        .lock()
        .map_err(|_| "target window lock poisoned")? = target;
    state
        .machine
        .lock()
        .map_err(|_| "coordinator lock poisoned")?
        .started(started.id, audio_path)
        .map_err(|error| error.to_string())?;
    platform::show_overlay_without_focus(app).map_err(|error| error.to_string())?;
    emit_state(app, state);
    state.snapshot()
}

#[tauri::command]
pub fn start_recording(app: AppHandle, state: State<'_, AppState>) -> Result<AppSnapshot, String> {
    start_recording_inner(&app, &state)
}

pub async fn stop_recording_inner(app: AppHandle, state: AppState) -> Result<AppSnapshot, String> {
    let (recording_id, audio_path_string) = match state.snapshot()?.dictation {
        DictationState::Recording {
            recording_id,
            audio_path,
        } => (recording_id, audio_path),
        _ => return Err(CoordinatorError::InvalidState.to_string()),
    };
    let audio = state.audio.clone();
    let completed = match tauri::async_runtime::spawn_blocking(move || audio.stop())
        .await
        .map_err(|error| error.to_string())?
    {
        Ok(completed) => completed,
        Err(error) => {
            let message = error.to_string();
            let _ = cleanup_partial(Path::new(&audio_path_string));
            let _ = recover_terminal_state(
                &state.machine,
                state.storage.as_ref(),
                &recording_id,
                &audio_path_string,
                TerminalOutcome::Failed(message.clone()),
            );
            emit_state(&app, &state);
            return Err(message);
        }
    };
    if let Err(error) = state.storage.update_status(
        &completed.id,
        RecordingStatus::Processing,
        None,
        None,
        Some(completed.duration_ms),
        None,
    ) {
        let message = error.to_string();
        let _ = recover_terminal_state(
            &state.machine,
            state.storage.as_ref(),
            &recording_id,
            &audio_path_string,
            TerminalOutcome::Failed(message.clone()),
        );
        emit_state(&app, &state);
        return Err(message);
    }
    let transition_result = {
        state
            .machine
            .lock()
            .map_err(|_| "coordinator lock poisoned")?
            .stopped()
    };
    if let Err(error) = transition_result {
        let message = error.to_string();
        let _ = recover_terminal_state(
            &state.machine,
            state.storage.as_ref(),
            &recording_id,
            &audio_path_string,
            TerminalOutcome::Failed(message.clone()),
        );
        emit_state(&app, &state);
        return Err(message);
    }
    emit_state(&app, &state);
    let snapshot = state.snapshot()?;
    let background_app = app.clone();
    let background_state = state.clone();
    tauri::async_runtime::spawn(async move {
        transcribe_recording(
            background_app,
            background_state,
            completed.id,
            completed.path,
        )
        .await;
    });
    Ok(snapshot)
}

#[tauri::command]
pub async fn stop_recording(
    app: AppHandle,
    state: State<'_, AppState>,
) -> Result<AppSnapshot, String> {
    stop_recording_inner(app, state.inner().clone()).await
}

async fn transcribe_recording(
    app: AppHandle,
    state: AppState,
    recording_id: String,
    audio_path: PathBuf,
) {
    let worker = state.worker.clone();
    let python = state.python.clone();
    let worker_path = state.worker_path.clone();
    let audio_for_worker = audio_path.clone();
    let outcome = tauri::async_runtime::spawn_blocking(move || {
        let existing = worker
            .lock()
            .map_err(|_| ClientError::WorkerUnavailable)?
            .take();
        let mut client = match existing {
            Some(client) => client,
            None => WorkerClient::spawn(python, worker_path, Duration::from_secs(120))?,
        };
        let result = client.transcribe(audio_for_worker, None);
        let must_respawn = matches!(
            result,
            Err(ClientError::Timeout { .. }
                | ClientError::Crashed { .. }
                | ClientError::WorkerUnavailable)
        );
        if !must_respawn {
            *worker.lock().map_err(|_| ClientError::WorkerUnavailable)? = Some(client);
        }
        result
    })
    .await;
    match outcome {
        Ok(Ok(result)) => finish_success(&app, &state, &recording_id, result),
        Ok(Err(error)) => finish_failure(&app, &state, &recording_id, error.to_string()),
        Err(error) => finish_failure(&app, &state, &recording_id, error.to_string()),
    }
}

fn finish_success(
    app: &AppHandle,
    state: &AppState,
    recording_id: &str,
    result: TranscriptionResult,
) {
    let vocabulary = state.storage.list_vocabulary().unwrap_or_default();
    let transcript = postprocess(&result.text, &vocabulary);
    let _ = state.storage.update_status(
        recording_id,
        RecordingStatus::Completed,
        Some(&transcript),
        Some(&result.model),
        Some(result.duration_ms),
        None,
    );
    if let Ok(mut machine) = state.machine.lock() {
        let _ = machine.transcription_succeeded(transcript.clone());
    }
    emit_state(app, state);
    let auto_paste = state
        .settings
        .read()
        .map(|settings| settings.auto_paste)
        .unwrap_or(false);
    if auto_paste {
        let target = state.target_window.lock().ok().and_then(|target| *target);
        let mut windows = SystemWindows;
        if let Err(error) = platform::copy_and_paste(&mut windows, target, &transcript) {
            let _ = app.emit("dictation://paste_error", error);
        }
    } else {
        let mut windows = SystemWindows;
        if let Err(error) = windows.write_clipboard(&transcript) {
            let _ = app.emit("dictation://paste_error", error);
        }
    }
    if let Ok(mut machine) = state.machine.lock() {
        let _ = machine.paste_completed();
    }
    platform::hide_overlay(app);
    emit_state(app, state);
}

fn finish_failure(app: &AppHandle, state: &AppState, recording_id: &str, error: String) {
    let _ = state.storage.update_status(
        recording_id,
        RecordingStatus::Failed,
        None,
        None,
        None,
        Some(&error),
    );
    if let Ok(mut machine) = state.machine.lock() {
        let _ = machine.transcription_failed(error);
    }
    emit_state(app, state);
}

#[tauri::command]
pub fn cancel_recording(app: AppHandle, state: State<'_, AppState>) -> Result<AppSnapshot, String> {
    cancel_recording_inner(&app, &state)
}

pub fn cancel_recording_inner(app: &AppHandle, state: &AppState) -> Result<AppSnapshot, String> {
    let (recording_id, audio_path) = match state.snapshot()?.dictation {
        DictationState::Recording {
            recording_id,
            audio_path,
        } => (recording_id, audio_path),
        _ => return Err(CoordinatorError::InvalidState.to_string()),
    };
    let cancelled = match state.audio.cancel() {
        Ok(cancelled) => cancelled,
        Err(error) => {
            let message = error.to_string();
            let _ = cleanup_partial(Path::new(&audio_path));
            let _ = recover_terminal_state(
                &state.machine,
                state.storage.as_ref(),
                &recording_id,
                &audio_path,
                TerminalOutcome::Cancelled,
            );
            platform::hide_overlay(app);
            emit_state(app, state);
            return Err(message);
        }
    };
    if let Err(error) = state.storage.update_status(
        &cancelled.id,
        RecordingStatus::Cancelled,
        None,
        None,
        Some(cancelled.duration_ms),
        None,
    ) {
        let message = error.to_string();
        let _ = recover_terminal_state(
            &state.machine,
            state.storage.as_ref(),
            &recording_id,
            &audio_path,
            TerminalOutcome::Cancelled,
        );
        platform::hide_overlay(app);
        emit_state(app, state);
        return Err(message);
    }
    state
        .machine
        .lock()
        .map_err(|_| "coordinator lock poisoned")?
        .cancel()
        .map_err(|error| error.to_string())?;
    platform::hide_overlay(app);
    emit_state(app, state);
    state.snapshot()
}

#[tauri::command(rename_all = "camelCase")]
pub fn retry_transcription(
    recording_id: String,
    app: AppHandle,
    state: State<'_, AppState>,
) -> Result<AppSnapshot, String> {
    let recording = state
        .storage
        .get_recording(&recording_id)
        .map_err(|error| error.to_string())?
        .ok_or_else(|| "recording not found".to_owned())?;
    if recording.status != RecordingStatus::Failed {
        return Err("only a failed recording can be retried".into());
    }
    let audio_path = PathBuf::from(
        recording
            .audio_path
            .ok_or_else(|| "recording has no audio".to_owned())?,
    );
    if !audio_path.is_file() {
        return Err("recording audio is missing".into());
    }
    let audio_path_string = audio_path.to_string_lossy().into_owned();
    state
        .machine
        .lock()
        .map_err(|_| "coordinator lock poisoned")?
        .retry_recording(&recording_id, audio_path_string)
        .map_err(|error| error.to_string())?;
    state
        .storage
        .mark_retrying(&recording_id)
        .map_err(|error| error.to_string())?;
    emit_state(&app, &state);
    let background_state = state.inner().clone();
    let background_app = app.clone();
    tauri::async_runtime::spawn(async move {
        transcribe_recording(background_app, background_state, recording_id, audio_path).await;
    });
    state.snapshot()
}

#[tauri::command(rename_all = "camelCase")]
pub fn paste_transcript(
    recording_id: Option<String>,
    state: State<'_, AppState>,
) -> Result<(), String> {
    paste_transcript_inner(recording_id.as_deref(), &state)
}

pub fn paste_transcript_inner(recording_id: Option<&str>, state: &AppState) -> Result<(), String> {
    let recording = if let Some(id) = recording_id {
        state.storage.get_recording(id)
    } else {
        state
            .storage
            .list_history(&HistoryQuery {
                search: None,
                status: Some(RecordingStatus::Completed),
            })
            .map(|recordings| recordings.into_iter().next())
    }
    .map_err(|error| error.to_string())?
    .ok_or_else(|| "completed transcript not found".to_owned())?;
    let text = recording
        .text
        .ok_or_else(|| "recording has no transcript".to_owned())?;
    let target = state.target_window.lock().ok().and_then(|target| *target);
    platform::copy_and_paste(&mut SystemWindows, target, &text).map_err(|error| error.to_string())
}

pub async fn toggle_recording(app: AppHandle, state: AppState) -> Result<(), String> {
    let snapshot = state.snapshot()?;
    match snapshot.dictation {
        DictationState::Idle => {
            start_recording_inner(&app, &state)?;
        }
        DictationState::Recording { .. } => {
            stop_recording_inner(app, state).await?;
        }
        DictationState::Failed { .. } => {
            state
                .machine
                .lock()
                .map_err(|_| "coordinator lock poisoned")?
                .cancel()
                .map_err(|error| error.to_string())?;
            start_recording_inner(&app, &state)?;
        }
        _ => {}
    }
    Ok(())
}

#[tauri::command(rename_all = "camelCase")]
pub fn list_history(
    query: HistoryQuery,
    state: State<'_, AppState>,
) -> Result<Vec<Recording>, String> {
    state
        .storage
        .list_history(&query)
        .map_err(|error| error.to_string())
}

#[tauri::command(rename_all = "camelCase")]
pub fn delete_history(recording_id: String, state: State<'_, AppState>) -> Result<bool, String> {
    let recording = state
        .storage
        .get_recording(&recording_id)
        .map_err(|error| error.to_string())?;
    if let Some(recording) = recording
        && let Some(audio_path) = recording.audio_path
    {
        remove_managed_audio(Path::new(&audio_path), state.storage.recordings_dir())?;
    }
    state
        .storage
        .delete_recording(&recording_id)
        .map_err(|error| error.to_string())
}

fn remove_managed_audio(path: &Path, managed_dir: &Path) -> Result<(), String> {
    if !path.exists() {
        return Ok(());
    }
    let managed = managed_dir
        .canonicalize()
        .map_err(|error| error.to_string())?;
    let canonical = path.canonicalize().map_err(|error| error.to_string())?;
    if !canonical.starts_with(&managed)
        || canonical
            .extension()
            .is_none_or(|extension| extension != "wav")
    {
        return Err("refusing to delete audio outside the managed recordings directory".into());
    }
    std::fs::remove_file(canonical).map_err(|error| error.to_string())
}

#[tauri::command]
pub fn list_vocabulary(state: State<'_, AppState>) -> Result<Vec<VocabularyEntry>, String> {
    state
        .storage
        .list_vocabulary()
        .map_err(|error| error.to_string())
}

#[tauri::command(rename_all = "camelCase")]
pub fn add_vocabulary(
    heard: String,
    replacement: String,
    state: State<'_, AppState>,
) -> Result<VocabularyEntry, String> {
    state
        .storage
        .add_vocabulary(&heard, &replacement)
        .map_err(|error| error.to_string())
}

#[tauri::command(rename_all = "camelCase")]
pub fn delete_vocabulary(id: i64, state: State<'_, AppState>) -> Result<bool, String> {
    state
        .storage
        .delete_vocabulary(id)
        .map_err(|error| error.to_string())
}

#[tauri::command]
pub fn list_modes(state: State<'_, AppState>) -> Result<Vec<Mode>, String> {
    state
        .storage
        .list_modes()
        .map_err(|error| error.to_string())
}

#[tauri::command(rename_all = "camelCase")]
pub fn upsert_mode(mode: Mode, state: State<'_, AppState>) -> Result<(), String> {
    state
        .storage
        .upsert_mode(&mode)
        .map_err(|error| error.to_string())
}

#[tauri::command(rename_all = "camelCase")]
pub fn delete_mode(id: String, state: State<'_, AppState>) -> Result<bool, String> {
    state
        .storage
        .delete_mode(&id)
        .map_err(|error| error.to_string())
}

#[tauri::command]
pub fn get_settings(state: State<'_, AppState>) -> Result<AppSettings, String> {
    state
        .settings
        .read()
        .map(|settings| settings.clone())
        .map_err(|_| "settings lock poisoned".into())
}

#[tauri::command(rename_all = "camelCase")]
pub fn update_settings(
    settings: AppSettings,
    app: AppHandle,
    state: State<'_, AppState>,
) -> Result<AppSettings, String> {
    retention_policy(settings.retention_days)?;
    platform::register_shortcuts(&app, &settings.shortcut).map_err(|error| error.to_string())?;
    platform::sync_autostart(&app, settings.launch_on_login).map_err(|error| error.to_string())?;
    state
        .storage
        .set_setting("app", &settings)
        .map_err(|error| error.to_string())?;
    *state
        .settings
        .write()
        .map_err(|_| "settings lock poisoned")? = settings.clone();
    cleanup_retention(&state)?;
    Ok(settings)
}

#[tauri::command(rename_all = "camelCase")]
pub fn update_setting_value(
    key: String,
    value: Value,
    state: State<'_, AppState>,
) -> Result<(), String> {
    state
        .storage
        .set_setting(&key, &value)
        .map_err(|error| error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{DictationState, RecoveryRecording};

    #[test]
    fn coordinator_rejects_a_second_start_without_losing_active_state() {
        let mut machine = CoordinatorMachine::default();
        machine.started("one", "one.wav").unwrap();

        assert_eq!(
            machine.started("two", "two.wav").unwrap_err(),
            CoordinatorError::InvalidState
        );
        assert!(matches!(
            machine.snapshot(),
            DictationState::Recording { recording_id, .. } if recording_id == "one"
        ));
    }

    #[test]
    fn failed_transcription_is_retryable_with_the_same_audio() {
        let recovery = RecoveryRecording {
            recording_id: "one".into(),
            audio_path: "one.wav".into(),
        };
        let mut machine = CoordinatorMachine::default();
        machine
            .started(&recovery.recording_id, &recovery.audio_path)
            .unwrap();
        machine.stopped().unwrap();
        machine.transcription_failed("worker crashed").unwrap();
        machine.retry().unwrap();

        assert_eq!(
            machine.snapshot(),
            DictationState::Processing {
                recording_id: recovery.recording_id,
                audio_path: recovery.audio_path,
            }
        );
    }

    #[test]
    fn historical_failure_can_be_retried_after_restart_from_idle() {
        let mut machine = CoordinatorMachine::default();

        machine
            .retry_recording("history-one", "history-one.wav")
            .unwrap();

        assert_eq!(
            machine.snapshot(),
            DictationState::Processing {
                recording_id: "history-one".into(),
                audio_path: "history-one.wav".into(),
            }
        );
    }

    #[test]
    fn settings_default_to_launching_on_login() {
        assert!(AppSettings::default().launch_on_login);
    }

    struct FailingHistory;

    impl TerminalStatusWriter for FailingHistory {
        fn write_terminal_status(
            &self,
            _recording_id: &str,
            _status: RecordingStatus,
            _error: Option<&str>,
        ) -> Result<(), String> {
            Err("database unavailable".into())
        }
    }

    #[test]
    fn storage_failure_still_releases_recording_state_for_next_start() {
        let machine = Mutex::new(CoordinatorMachine::default());
        machine.lock().unwrap().started("one", "one.wav").unwrap();

        let result = recover_terminal_state(
            &machine,
            &FailingHistory,
            "one",
            "one.wav",
            TerminalOutcome::Failed("disk full".into()),
        );

        assert_eq!(result.unwrap_err(), "database unavailable");
        {
            let mut machine = machine.lock().unwrap();
            assert!(matches!(machine.snapshot(), DictationState::Failed { .. }));
            machine.cancel().unwrap();
            assert!(machine.started("two", "two.wav").is_ok());
        }
    }

    #[test]
    fn cancellation_recovery_goes_idle_even_when_storage_fails() {
        let machine = Mutex::new(CoordinatorMachine::default());
        machine.lock().unwrap().started("one", "one.wav").unwrap();

        let result = recover_terminal_state(
            &machine,
            &FailingHistory,
            "one",
            "one.wav",
            TerminalOutcome::Cancelled,
        );

        assert_eq!(result.unwrap_err(), "database unavailable");
        assert_eq!(machine.lock().unwrap().snapshot(), DictationState::Idle);
    }
}
