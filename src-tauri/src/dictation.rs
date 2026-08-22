use crate::engine::{Engine, EngineError};
use crate::audio::{
    AudioRecorder, CompletedRecording, InputDeviceInfo, cleanup_partial, quantize_peaks,
};
use crate::domain::{DictationEvent, DictationState, transition};
use crate::platform::{self, SystemWindows, WindowTarget, WindowsApi};
use crate::storage::{
    HistoryQuery, Mode, Recording, RecordingStatus, Retention, Storage, StorageError,
    VocabularyEntry, postprocess_for_mode,
};
use crate::transcription::TranscriptionResult;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeSet;
use std::io::{BufRead, Read};
use std::path::{Component, Path, PathBuf};
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tauri::{AppHandle, Emitter, Manager, State};
use thiserror::Error;

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum CoordinatorError {
    #[error("the requested operation is invalid for the current state")]
    InvalidState,
    #[error("another lifecycle operation is in progress")]
    Busy,
}

fn lifecycle_guard(gate: &Mutex<()>) -> Result<std::sync::MutexGuard<'_, ()>, CoordinatorError> {
    gate.try_lock().map_err(|_| CoordinatorError::Busy)
}

#[derive(Debug, Error, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "code", rename_all = "snake_case")]
pub enum AppError {
    #[error("recording is still active: {recording_id}")]
    ActiveRecording { recording_id: String },
    #[error("recording is busy: {recording_id}")]
    Busy { recording_id: String },
    #[error("recording cannot be retried: {message}")]
    NotRetryable { message: String },
    #[error("storage failed: {message}")]
    Storage { message: String },
    #[error("filesystem failed: {message}")]
    Io { message: String },
}

impl From<StorageError> for AppError {
    fn from(error: StorageError) -> Self {
        Self::Storage {
            message: error.to_string(),
        }
    }
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
        if !matches!(
            self.state,
            DictationState::Recording { .. } | DictationState::Cancelling { .. }
        ) {
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
            DictationState::Recording { .. }
                | DictationState::Failed { .. }
                | DictationState::Cancelling { .. }
        ) {
            return Err(CoordinatorError::InvalidState);
        }
        self.state = transition(self.state.clone(), DictationEvent::Cancel);
        Ok(())
    }

    pub fn request_cancel(&mut self) -> Result<(), CoordinatorError> {
        if !matches!(
            self.state,
            DictationState::Recording { .. } | DictationState::Cancelling { .. }
        ) {
            return Err(CoordinatorError::InvalidState);
        }
        self.state = transition(self.state.clone(), DictationEvent::CancelRequest);
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
#[serde(rename_all = "snake_case")]
pub enum LanguageChoice {
    System,
    Pl,
    En,
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
    #[serde(default = "default_active_mode")]
    pub active_mode: String,
    #[serde(default = "default_show_overlay")]
    pub show_overlay: bool,
    #[serde(default = "default_model")]
    pub model: String,
    #[serde(default = "default_streaming")]
    pub streaming: bool,
    #[serde(default = "default_language")]
    pub language: LanguageChoice,
    #[serde(default = "default_dictation_language")]
    pub dictation_language: String,
    /// How many seconds the loaded model stays in memory after the last use.
    /// 0 means "always" (never unload).
    #[serde(default)]
    pub model_keep_alive_secs: u64,
}

const fn default_launch_on_login() -> bool {
    false
}

const fn default_show_overlay() -> bool {
    true
}

const fn default_streaming() -> bool {
    false
}

fn default_model() -> String {
    crate::models::default_model().to_owned()
}

fn default_active_mode() -> String {
    "clean".into()
}

const fn default_language() -> LanguageChoice {
    LanguageChoice::System
}

fn default_dictation_language() -> String {
    "auto".into()
}

impl Default for AppSettings {
    fn default() -> Self {
        Self {
            input_device: None,
            shortcut: "Ctrl+Space".into(),
            auto_paste: true,
            retention_days: Some(30),
            launch_on_login: false,
            active_mode: default_active_mode(),
            show_overlay: default_show_overlay(),
            model: default_model(),
            streaming: default_streaming(),
            language: default_language(),
            dictation_language: default_dictation_language(),
            model_keep_alive_secs: 0,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelDescriptor {
    pub key: String,
    pub id: String,
    pub provider: String,
    pub source: String,
    pub revision: String,
    pub display: String,
    pub min_vram_gb: f32,
    pub min_ram_gb: f32,
    pub languages: String,
    pub estimated_size_bytes: u64,
    pub status: ModelStatusState,
    pub installed_size_bytes: Option<u64>,
    pub message: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ModelStatusState {
    Ready,
    NotInstalled,
    Error,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelStatus {
    pub state: ModelStatusState,
    pub model: String,
    pub revision: String,
    pub device: Option<String>,
    pub message: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelDownloadProgress {
    pub model: String,
    pub phase: String,
    pub downloaded_bytes: u64,
    pub total_bytes: Option<u64>,
}

#[derive(Debug, Deserialize)]
struct WorkerDownloadProgress {
    event: String,
    downloaded_bytes: u64,
    total_bytes: Option<u64>,
}


fn emit_model_progress(app: &AppHandle, model: &str, phase: &str, downloaded_bytes: u64, total_bytes: Option<u64>) {
    let _ = app.emit(
        "models://progress",
        ModelDownloadProgress {
            model: model.to_owned(),
            phase: phase.to_owned(),
            downloaded_bytes,
            total_bytes,
        },
    );
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
    pub model_loading: bool,
    /// Epoch ms when the active recording started, so the UI can keep an
    /// accurate elapsed-time counter even across page remounts.
    pub recording_started_at: Option<i64>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SettingsUpdateResult {
    pub settings: AppSettings,
    pub warning: Option<String>,
}

#[derive(Clone)]
pub struct AppState {
    pub machine: Arc<Mutex<CoordinatorMachine>>,
    pub audio: Arc<AudioRecorder>,
    pub storage: Arc<Storage>,
    pub engine: Arc<Engine>,
    /// When the engine last finished work, so a keep-alive watchdog can unload
    /// the model after a configurable idle timeout.
    engine_last_used: Arc<Mutex<Instant>>,
    pub target_window: Arc<Mutex<Option<WindowTarget>>>,
    pub settings: Arc<RwLock<AppSettings>>,
    pub model_warm: Arc<ModelWarmup>,
    lifecycle: Arc<Mutex<()>>,
    settings_update: Arc<Mutex<()>>,
    model_home: PathBuf,
}

/// Tracks model warm-up so transcription never loads the model twice and waits
/// for the background warm-up to finish before transcribing. Re-warming when
/// the model changes is supported.
#[derive(Default)]
pub struct ModelWarmup {
    state: Mutex<ModelWarmupState>,
    changed: std::sync::Condvar,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
enum ModelWarmupState {
    #[default]
    Idle,
    Warming(String),
    Ready(String),
    Failed(String),
}

impl ModelWarmup {
    pub fn begin_for(&self, model: String) {
        let mut state = self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let needs_start = match &*state {
            ModelWarmupState::Idle => true,
            ModelWarmupState::Warming(current) | ModelWarmupState::Ready(current) | ModelWarmupState::Failed(current) => {
                current != &model
            }
        };
        if needs_start {
            *state = ModelWarmupState::Warming(model);
            self.changed.notify_all();
        }
    }

    pub fn finish_for(&self, model: String, ready: bool) {
        let mut state = self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        if matches!(&*state, ModelWarmupState::Warming(current) if *current == model) {
            *state = if ready {
                ModelWarmupState::Ready(model)
            } else {
                ModelWarmupState::Failed(model)
            };
            self.changed.notify_all();
        }
    }

    /// Forgets that any model is loaded.
    ///
    /// Called when the engine releases its model on idle: leaving the state at
    /// `Ready` would tell the next warm-up there was nothing to do, and the
    /// first dictation afterwards would pay the load cost while claiming to be
    /// ready.
    pub fn forget(&self) {
        let mut state = self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        *state = ModelWarmupState::Idle;
        self.changed.notify_all();
    }

    pub fn is_ready_for(&self, model: &str) -> bool {
        let state = self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        matches!(&*state, ModelWarmupState::Ready(current) if current == model)
    }

    pub fn is_loading(&self) -> bool {
        let state = self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        matches!(&*state, ModelWarmupState::Warming(_))
    }

    /// Blocks until the named model has finished warming, or the wait times
    /// out. A timeout is reported as an error so the caller can decide; a
    /// failed warm-up is not, because the load will simply be retried.
    fn wait_until_ready_for(&self, model: &str, timeout: Duration) -> Result<(), EngineError> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| EngineError::Load("warm-up lock poisoned".into()))?;
        if matches!(&*state, ModelWarmupState::Ready(current) if current == model) {
            return Ok(());
        }
        if matches!(&*state, ModelWarmupState::Failed(current) if current == model) {
            return Ok(());
        }
        let (guard, wait_result) = self
            .changed
            .wait_timeout_while(state, timeout, |state| {
                matches!(state, ModelWarmupState::Warming(current) if current == model)
            })
            .map_err(|_| EngineError::Load("warm-up lock poisoned".into()))?;
        state = guard;
        if wait_result.timed_out()
            && matches!(&*state, ModelWarmupState::Warming(current) if current == model)
        {
            return Err(EngineError::Load(format!(
                "the model did not finish loading within {}s",
                timeout.as_secs()
            )));
        }
        Ok(())
    }
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

pub trait CompletionWriter {
    fn write_completed(
        &self,
        recording_id: &str,
        text: &str,
        model: &str,
        duration_ms: u64,
    ) -> Result<(), String>;
    fn write_failed(&self, recording_id: &str, error: &str) -> Result<(), String>;
}

impl CompletionWriter for Storage {
    fn write_completed(
        &self,
        recording_id: &str,
        text: &str,
        model: &str,
        duration_ms: u64,
    ) -> Result<(), String> {
        match self.update_status(
            recording_id,
            RecordingStatus::Completed,
            Some(text),
            Some(model),
            Some(duration_ms),
            None,
        ) {
            Ok(true) => Ok(()),
            Ok(false) => Err("recording disappeared before completion".into()),
            Err(error) => Err(error.to_string()),
        }
    }

    fn write_failed(&self, recording_id: &str, error: &str) -> Result<(), String> {
        self.update_status(
            recording_id,
            RecordingStatus::Failed,
            None,
            None,
            None,
            Some(error),
        )
        .map(|_| ())
        .map_err(|error| error.to_string())
    }
}

pub struct DurableTranscript<'a> {
    pub recording_id: &'a str,
    pub audio_path: &'a str,
    pub transcript: &'a str,
    pub model: &'a str,
    pub duration_ms: u64,
}

pub fn complete_transcription_durably(
    machine: &Mutex<CoordinatorMachine>,
    history: &impl CompletionWriter,
    completed: DurableTranscript<'_>,
    after_commit: impl FnOnce(),
) -> Result<(), String> {
    if let Err(error) = history.write_completed(
        completed.recording_id,
        completed.transcript,
        completed.model,
        completed.duration_ms,
    ) {
        let _ = history.write_failed(completed.recording_id, &error);
        let mut machine = machine
            .lock()
            .map_err(|_| "coordinator lock poisoned".to_owned())?;
        machine.state = DictationState::Failed {
            recovery: crate::domain::RecoveryRecording {
                recording_id: completed.recording_id.to_owned(),
                audio_path: completed.audio_path.to_owned(),
            },
            error: error.clone(),
        };
        return Err(error);
    }
    {
        let mut machine = machine
            .lock()
            .map_err(|_| "coordinator lock poisoned".to_owned())?;
        machine
            .transcription_succeeded(completed.transcript)
            .map_err(|error| error.to_string())?;
    }
    after_commit();
    machine
        .lock()
        .map_err(|_| "coordinator lock poisoned".to_owned())?
        .paste_completed()
        .map_err(|error| error.to_string())
}

pub trait ShortcutSettingsEffect {
    fn validate(&self, shortcut: &str) -> Result<(), String>;
    fn replace(&mut self, old: &str, new: &str) -> Result<(), String>;
}

pub trait AutostartSettingsEffect {
    fn apply(&mut self, enabled: bool) -> Result<(), String>;
}

pub trait SettingsStoreEffect {
    fn save(&mut self, settings: &AppSettings) -> Result<(), String>;
    fn cleanup_retention(&mut self) -> Result<(), String>;
}

pub fn apply_settings_change(
    shortcuts: &mut impl ShortcutSettingsEffect,
    autostart: &mut impl AutostartSettingsEffect,
    storage: &mut impl SettingsStoreEffect,
    live: &mut AppSettings,
    new: AppSettings,
) -> Result<SettingsUpdateResult, String> {
    shortcuts.validate(&new.shortcut)?;
    let old = live.clone();
    shortcuts.replace(&old.shortcut, &new.shortcut)?;
    if let Err(error) = autostart.apply(new.launch_on_login) {
        let autostart_rollback = autostart.apply(old.launch_on_login);
        let shortcut_rollback = shortcuts.replace(&new.shortcut, &old.shortcut);
        return Err(with_rollback_error(
            with_rollback_error(error, autostart_rollback),
            shortcut_rollback,
        ));
    }
    if let Err(error) = storage.save(&new) {
        let autostart_rollback = autostart.apply(old.launch_on_login);
        let shortcut_rollback = shortcuts.replace(&new.shortcut, &old.shortcut);
        return Err(with_rollback_error(
            with_rollback_error(error, autostart_rollback),
            shortcut_rollback,
        ));
    }
    *live = new.clone();
    let warning = storage.cleanup_retention().err();
    Ok(SettingsUpdateResult {
        settings: new,
        warning,
    })
}

fn with_rollback_error(error: String, rollback: Result<(), String>) -> String {
    match rollback {
        Ok(()) => error,
        Err(rollback) => format!("{error}; rollback failed: {rollback}"),
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
        model_home: impl Into<PathBuf>,
    ) -> Self {
        let model_home = model_home.into();
        let mut settings = storage
            .get_setting::<AppSettings>("app")
            .ok()
            .flatten()
            .unwrap_or_default();
        let resolved = crate::models::resolve(&settings.model);
        if resolved != settings.model {
            settings.model = resolved.to_owned();
            let _ = storage.set_setting("app", &settings);
        }
        Self {
            machine: Arc::new(Mutex::new(CoordinatorMachine::default())),
            audio: Arc::new(audio),
            storage: Arc::new(storage),
            engine: Arc::new(Engine::new(model_home.clone())),
            engine_last_used: Arc::new(Mutex::new(Instant::now())),
            target_window: Arc::new(Mutex::new(None)),
            settings: Arc::new(RwLock::new(settings)),
            model_warm: Arc::new(ModelWarmup::default()),
            lifecycle: Arc::new(Mutex::new(())),
            settings_update: Arc::new(Mutex::new(())),
            model_home,
        }
    }


    fn snapshot(&self) -> Result<AppSnapshot, String> {
        let dictation = self
            .machine
            .lock()
            .map_err(|_| "coordinator lock poisoned")?
            .snapshot();
        let recording_started_at = match &dictation {
            DictationState::Recording { recording_id, .. }
            | DictationState::Cancelling { recording_id, .. } => self
                .storage
                .get_recording(recording_id)
                .ok()
                .flatten()
                .map(|recording| recording.created_at),
            _ => None,
        };
        Ok(AppSnapshot {
            dictation,
            settings: self
                .settings
                .read()
                .map_err(|_| "settings lock poisoned")?
                .clone(),
            model_loading: self.model_warm.is_loading(),
            recording_started_at,
        })
    }

    fn show_overlay(&self) -> bool {
        self.settings
            .read()
            .map(|settings| settings.show_overlay)
            .unwrap_or(true)
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct OverlayPosition {
    pub x: i32,
    pub y: i32,
}

/// Remembered overlay position, so the user can move the pill like in
/// Superwhisper and it stays where they left it.
fn overlay_position(state: &AppState) -> Option<(i32, i32)> {
    state
        .storage
        .get_setting::<OverlayPosition>("overlay_position")
        .ok()
        .flatten()
        .map(|position| (position.x, position.y))
}

struct TauriShortcutSettings<'a> {
    app: &'a AppHandle,
}

impl ShortcutSettingsEffect for TauriShortcutSettings<'_> {
    fn validate(&self, shortcut: &str) -> Result<(), String> {
        crate::platform::Shortcut::parse(shortcut)
            .map(|_| ())
            .map_err(|error| error.to_string())
    }

    fn replace(&mut self, old: &str, new: &str) -> Result<(), String> {
        platform::replace_toggle_shortcut(self.app, old, new).map_err(|error| error.to_string())
    }
}

struct TauriAutostartSettings<'a> {
    app: &'a AppHandle,
}

impl AutostartSettingsEffect for TauriAutostartSettings<'_> {
    fn apply(&mut self, enabled: bool) -> Result<(), String> {
        platform::sync_autostart(self.app, enabled).map_err(|error| error.to_string())
    }
}

struct PersistentSettings<'a> {
    storage: &'a Storage,
    retention: Retention,
}

impl SettingsStoreEffect for PersistentSettings<'_> {
    fn save(&mut self, settings: &AppSettings) -> Result<(), String> {
        self.storage
            .set_setting("app", settings)
            .map_err(|error| error.to_string())
    }

    fn cleanup_retention(&mut self) -> Result<(), String> {
        self.storage
            .cleanup_retention(self.retention, epoch_ms())
            .map(|_| ())
            .map_err(|error| error.to_string())
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

pub fn warm_up_model(app: &AppHandle, state: &AppState) {
    let warm = state.model_warm.clone();
    let engine = state.engine.clone();
    let engine_last_used = state.engine_last_used.clone();
    let model = state
        .settings
        .read()
        .map(|settings| settings.model.clone())
        .unwrap_or_else(|_| default_model());
    // Loading must never turn into an implicit download: the explicit model
    // action owns installation, and warm-up only uses what is already here.
    if !engine.is_installed(&model) {
        return;
    }
    if warm.is_ready_for(&model) {
        return;
    }
    warm.begin_for(model.clone());
    emit_state(app, state);

    let warm_model = model.clone();
    let emit_app = app.clone();
    let emit_state_after = state.clone();
    std::thread::spawn(move || {
        let ready = engine.ensure_loaded(&warm_model).is_ok();
        if ready {
            if let Ok(mut last_used) = engine_last_used.lock() {
                *last_used = Instant::now();
            }
        }
        warm.finish_for(warm_model, ready);
        emit_state(&emit_app, &emit_state_after);
    });
}

fn emit_state(app: &AppHandle, state: &AppState) {
    if let Ok(snapshot) = state.snapshot() {
        // Escape belongs to whatever the user is doing, except while there is
        // a recording it could discard. Every state change passes through
        // here, so this is where the key is claimed and given back.
        let _ = platform::set_cancel_shortcut(app, holds_discardable_audio(&snapshot.dictation));
        let _ = app.emit("dictation://state", snapshot);
    }
}

/// Whether there is a recording in progress that Escape could discard.
pub fn holds_discardable_audio(state: &DictationState) -> bool {
    matches!(
        state,
        DictationState::Recording { .. } | DictationState::Cancelling { .. }
    )
}

/// Watches the idle worker and unloads the model from GPU/RAM after the
/// configured keep-alive timeout, freeing memory for other apps. With the
/// default setting (`0` = always) it never unloads.
pub fn spawn_model_keep_alive_watchdog(state: &AppState) {
    let state = state.clone();
    std::thread::spawn(move || loop {
        let keep = state
            .settings
            .read()
            .map(|settings| settings.model_keep_alive_secs)
            .unwrap_or(0);
        if keep > 0 {
            let idle_elapsed = state
                .engine_last_used
                .lock()
                .map(|last| last.elapsed())
                .unwrap_or_default();
            if idle_elapsed >= Duration::from_secs(keep) && state.engine.loaded_model().is_some() {
                state.engine.unload();
                state.model_warm.forget();
                if let Ok(mut last) = state.engine_last_used.lock() {
                    *last = Instant::now();
                }
            }
        }
        std::thread::sleep(Duration::from_secs(10));
    });
}

#[tauri::command]
pub fn get_app_snapshot(state: State<'_, AppState>) -> Result<AppSnapshot, String> {
    state.snapshot()
}

pub fn set_input_device(state: &AppState, device: String) {
    let mut settings = state
        .settings
        .write()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    settings.input_device = (!device.is_empty()).then_some(device);
    let _ = state.storage.set_setting("app", &*settings);
}

#[tauri::command]
pub fn list_input_devices(state: State<'_, AppState>) -> Result<Vec<InputDeviceInfo>, String> {
    state
        .audio
        .list_devices()
        .map_err(|error| error.to_string())
}

pub fn start_recording_inner(app: &AppHandle, state: &AppState) -> Result<AppSnapshot, String> {
    let _lifecycle = lifecycle_guard(&state.lifecycle).map_err(|error| error.to_string())?;
    if !matches!(state.snapshot()?.dictation, DictationState::Idle) {
        return Err(CoordinatorError::InvalidState.to_string());
    }
    let selected_model = state
        .settings
        .read()
        .map_err(|_| "settings lock poisoned")?
        .model
        .clone();
    if !state.engine.is_installed(&selected_model) {
        return Err("Najpierw pobierz wybrany model.".into());
    }
    // Warm the model while the user talks, so transcription does not pay the
    // load cost the moment they stop.
    warm_up_model(app, state);
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
        // Filled in when the capture stops and the envelope is known.
        peaks: None,
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
    if state.show_overlay() {
        // Give the overlay focus so its Esc/Enter controls for cancelling
        // actually reach the DOM (the pill is where the user interacts while
        // dictating; focus is restored to the target window afterwards).
        platform::show_overlay_with_focus(app, overlay_position(&state))
            .map_err(|error| error.to_string())?;
    }
    crate::sound::play_recording_started();
    emit_state(app, state);
    state.snapshot()
}

#[tauri::command]
pub fn start_recording(app: AppHandle, state: State<'_, AppState>) -> Result<AppSnapshot, String> {
    start_recording_inner(&app, &state)
}

#[tauri::command]
pub fn hide_overlay(app: AppHandle) {
    platform::hide_overlay(&app);
}

#[tauri::command]
pub fn set_shortcut_suspended(
    app: AppHandle,
    state: State<'_, AppState>,
    suspended: bool,
) -> Result<(), String> {
    if suspended {
        platform::suspend_shortcuts(&app).map_err(|error| error.to_string())
    } else {
        let shortcut = state
            .settings
            .read()
            .map(|settings| settings.shortcut.clone())
            .unwrap_or_else(|_| "Ctrl+Space".to_string());
        platform::register_shortcuts(&app, &shortcut).map_err(|error| error.to_string())
    }
}

#[tauri::command]
pub fn save_overlay_position(
    x: i32,
    y: i32,
    state: State<'_, AppState>,
) -> Result<(), String> {
    state
        .storage
        .set_setting("overlay_position", &OverlayPosition { x, y })
        .map_err(|error| error.to_string())
}

pub async fn stop_recording_inner(app: AppHandle, state: AppState) -> Result<AppSnapshot, String> {
    let blocking_state = state.clone();
    let completed_result = tauri::async_runtime::spawn_blocking(move || {
        let _lifecycle =
            lifecycle_guard(&blocking_state.lifecycle).map_err(|error| error.to_string())?;
        stop_recording_committed(&blocking_state)
    })
    .await
    .map_err(|error| error.to_string())?;
    let completed = match completed_result {
        Ok(completed) => completed,
        Err(error) => {
            emit_state(&app, &state);
            return Err(error);
        }
    };
    crate::sound::play_recording_stopped();
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

fn stop_recording_committed(state: &AppState) -> Result<CompletedRecording, String> {
    let (recording_id, audio_path_string) = match state.snapshot()?.dictation {
        DictationState::Recording {
            recording_id,
            audio_path,
        }
        | DictationState::Cancelling {
            recording_id,
            audio_path,
        } => (recording_id, audio_path),
        _ => return Err(CoordinatorError::InvalidState.to_string()),
    };
    let completed = match state.audio.stop() {
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
            return Err(message);
        }
    };
    // Best effort: a missing envelope costs a waveform in the history list,
    // which is not worth failing a stop that already has usable audio.
    if !completed.peaks.is_empty() {
        let _ = state
            .storage
            .store_peaks(&completed.id, &quantize_peaks(&completed.peaks));
    }
    if let Err(error) =
        mark_processing(state.storage.as_ref(), &completed.id, completed.duration_ms)
    {
        let message = error.to_string();
        let _ = recover_terminal_state(
            &state.machine,
            state.storage.as_ref(),
            &recording_id,
            &audio_path_string,
            TerminalOutcome::Failed(message.clone()),
        );
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
        return Err(message);
    }
    Ok(completed)
}

fn mark_processing(
    storage: &Storage,
    recording_id: &str,
    duration_ms: u64,
) -> Result<(), AppError> {
    match storage.update_status(
        recording_id,
        RecordingStatus::Processing,
        None,
        None,
        Some(duration_ms),
        None,
    ) {
        Ok(true) => Ok(()),
        Ok(false) => Err(AppError::Storage {
            message: format!("recording disappeared before processing: {recording_id}"),
        }),
        Err(error) => Err(error.into()),
    }
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
    let engine = state.engine.clone();
    let engine_last_used = state.engine_last_used.clone();
    let audio_for_engine = audio_path.clone();
    let warm = state.model_warm.clone();
    let model = state
        .settings
        .read()
        .map(|settings| settings.model.clone())
        .unwrap_or_else(|_| default_model());
    let started = Instant::now();
    let outcome = tauri::async_runtime::spawn_blocking(move || {
        // A warm-up started when recording began may still be finishing; the
        // engine would serialise on its own lock anyway, but waiting here
        // keeps the reported state honest.
        let _ = warm.wait_until_ready_for(&model, Duration::from_secs(240));
        let text = engine.transcribe(&model, &audio_for_engine)?;
        if let Ok(mut last_used) = engine_last_used.lock() {
            *last_used = Instant::now();
        }
        Ok::<_, EngineError>(TranscriptionResult {
            text,
            model,
            language: None,
            duration_ms: started.elapsed().as_millis().try_into().unwrap_or(u64::MAX),
        })
    })
    .await;
    match outcome {
        Ok(Ok(result)) => finish_success(&app, &state, &recording_id, &audio_path, result),
        Ok(Err(error)) => finish_failure(&app, &state, &recording_id, error.to_string()),
        Err(error) => finish_failure(&app, &state, &recording_id, error.to_string()),
    }
}

fn finish_success(
    app: &AppHandle,
    state: &AppState,
    recording_id: &str,
    audio_path: &Path,
    result: TranscriptionResult,
) {
    let vocabulary = state.storage.list_vocabulary().unwrap_or_default();
    let settings = state
        .settings
        .read()
        .map(|settings| settings.clone())
        .unwrap_or_default();
    let mode = state
        .storage
        .get_mode(&settings.active_mode)
        .ok()
        .flatten()
        .filter(|mode| mode.enabled);
    let transcript = postprocess_with_mode(&result.text, &vocabulary, mode.as_ref());
    let audio_path = audio_path.to_string_lossy();
    let durable = complete_transcription_durably(
        &state.machine,
        state.storage.as_ref(),
        DurableTranscript {
            recording_id,
            audio_path: &audio_path,
            transcript: &transcript,
            model: &result.model,
            duration_ms: result.duration_ms,
        },
        || {
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
        },
    );
    if durable.is_ok() {
        crate::sound::play_transcription_ready();
        platform::hide_overlay(app);
    } else {
        if let Err(error) = durable {
            let _ = app.emit("dictation://persistence_error", error);
        }
    }
    emit_state(app, state);
}

fn postprocess_with_mode(
    text: &str,
    vocabulary: &[VocabularyEntry],
    mode: Option<&Mode>,
) -> String {
    mode.map_or_else(
        || {
            let fallback = Mode {
                id: "clean".into(),
                name: "Czysty".into(),
                description: String::new(),
                prompt: String::new(),
                enabled: true,
                is_default: true,
                created_at: 0,
            };
            postprocess_for_mode(text, vocabulary, &fallback)
        },
        |selected| postprocess_for_mode(text, vocabulary, selected),
    )
}

fn finish_failure(app: &AppHandle, state: &AppState, recording_id: &str, error: String) {
    let _ = state.storage.write_failed(recording_id, &error);
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
    let _lifecycle = lifecycle_guard(&state.lifecycle).map_err(|error| error.to_string())?;
    let (recording_id, audio_path) = match state.snapshot()?.dictation {
        DictationState::Recording {
            recording_id,
            audio_path,
        }
        | DictationState::Cancelling {
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
    platform::restore_foreground_to(
        app,
        state.target_window.lock().ok().and_then(|target| *target),
    );
    emit_state(app, state);
    state.snapshot()
}

#[tauri::command]
pub fn request_cancel(app: AppHandle, state: State<'_, AppState>) -> Result<AppSnapshot, String> {
    request_cancel_inner(&app, &state)
}

/// What a press of the cancel key means, given what dictation is doing.
///
/// The recorder pill is shown without focus and Windows refuses to hand it
/// focus while another window of ours is active, so keyboard handling in the
/// pill's document cannot be relied on. The global shortcut is the only path
/// that fires whatever has focus, which makes it the only place this decision
/// can live.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CancelPress {
    /// Nothing has been asked yet: put the question on screen.
    Ask,
    /// The question is already on screen; this press answers it.
    Confirm,
    /// There is nothing to cancel.
    Ignore,
}

pub fn cancel_press_for(state: &DictationState) -> CancelPress {
    match state {
        DictationState::Recording { .. } => CancelPress::Ask,
        DictationState::Cancelling { .. } => CancelPress::Confirm,
        _ => CancelPress::Ignore,
    }
}

/// Applies a press of the cancel key to whatever dictation is currently doing.
pub fn handle_cancel_press(app: &AppHandle, state: &AppState) {
    let Ok(snapshot) = state.snapshot() else { return };
    match cancel_press_for(&snapshot.dictation) {
        CancelPress::Ask => {
            let _ = request_cancel_inner(app, state);
        }
        CancelPress::Confirm => {
            let _ = cancel_recording_inner(app, state);
        }
        CancelPress::Ignore => {}
    }
}

pub fn request_cancel_inner(app: &AppHandle, state: &AppState) -> Result<AppSnapshot, String> {
    let _lifecycle = lifecycle_guard(&state.lifecycle).map_err(|error| error.to_string())?;
    match state.snapshot()?.dictation {
        DictationState::Recording { .. } => {
            state
                .machine
                .lock()
                .map_err(|_| "coordinator lock poisoned")?
                .request_cancel()
                .map_err(|error| error.to_string())?;
            platform::show_overlay_with_focus(app, overlay_position(state))
                .map_err(|error| error.to_string())?;
            emit_state(app, state);
            state.snapshot()
        }
        DictationState::Cancelling { .. } => {
            state
                .machine
                .lock()
                .map_err(|_| "coordinator lock poisoned")?
                .request_cancel()
                .map_err(|error| error.to_string())?;
            platform::restore_foreground_to(
                app,
                state.target_window.lock().ok().and_then(|target| *target),
            );
            emit_state(app, state);
            state.snapshot()
        }
        _ => state.snapshot(),
    }
}

#[tauri::command(rename_all = "camelCase")]
pub fn retry_transcription(
    recording_id: String,
    app: AppHandle,
    state: State<'_, AppState>,
) -> Result<AppSnapshot, String> {
    let audio_path = {
        let _lifecycle = lifecycle_guard(&state.lifecycle).map_err(|error| error.to_string())?;
        let mut machine = state
            .machine
            .lock()
            .map_err(|_| "coordinator lock poisoned")?;
        if !matches!(
            machine.snapshot(),
            DictationState::Idle | DictationState::Failed { .. }
        ) {
            return Err(CoordinatorError::InvalidState.to_string());
        }
        let recording = state
            .storage
            .get_recording(&recording_id)
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "recording not found".to_owned())?;
        let audio_path = retry_audio_path(&recording).map_err(|error| error.to_string())?;
        let selected_model = state
            .settings
            .read()
            .map_err(|_| "settings lock poisoned")?
            .model
            .clone();
        if !state.engine.is_installed(&selected_model) {
            return Err(format!(
                "The selected model is not ready. Download it before retrying: {selected_model}."
            ));
        }
        match state.storage.mark_retrying(&recording_id) {
            Ok(true) => {}
            Ok(false) => return Err("recording disappeared before retry".into()),
            Err(error) => return Err(error.to_string()),
        }
        machine
            .retry_recording(&recording_id, audio_path.to_string_lossy().into_owned())
            .map_err(|error| error.to_string())?;
        audio_path
    };
    emit_state(&app, &state);
    let background_state = state.inner().clone();
    let background_app = app.clone();
    tauri::async_runtime::spawn(async move {
        transcribe_recording(background_app, background_state, recording_id, audio_path).await;
    });
    state.snapshot()
}

fn retry_audio_path(recording: &Recording) -> Result<PathBuf, AppError> {
    if recording.status != RecordingStatus::Failed {
        return Err(AppError::NotRetryable {
            message: "only a failed recording can be retried".into(),
        });
    }
    let audio_path =
        PathBuf::from(
            recording
                .audio_path
                .as_deref()
                .ok_or_else(|| AppError::NotRetryable {
                    message: "recording has no finalized audio".into(),
                })?,
        );
    if !audio_path.is_file() {
        return Err(AppError::NotRetryable {
            message: "recording audio is missing".into(),
        });
    }
    Ok(audio_path)
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
        DictationState::Recording { .. } | DictationState::Cancelling { .. } => {
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
pub fn delete_history(recording_id: String, state: State<'_, AppState>) -> Result<bool, AppError> {
    delete_history_safely(&state.machine, state.storage.as_ref(), &recording_id)
}

/// Writes a completed transcript to a `.txt` file in the user's Downloads
/// folder and returns the saved path so the UI can reveal or copy it.
#[tauri::command(rename_all = "camelCase")]
pub fn export_transcript(
    recording_id: String,
    app: AppHandle,
    state: State<'_, AppState>,
) -> Result<String, String> {
    let recording = state
        .storage
        .get_recording(&recording_id)
        .map_err(|error| error.to_string())?
        .ok_or_else(|| "recording not found".to_owned())?;
    let text = recording
        .text
        .filter(|text| !text.trim().is_empty())
        .ok_or_else(|| "This recording has no transcript to export.".to_owned())?;
    let dir = app
        .path()
        .download_dir()
        .map_err(|error| format!("Could not locate the Downloads folder: {error}"))?;
    std::fs::create_dir_all(&dir).map_err(|error| error.to_string())?;
    let stem = recording.id.trim_start_matches("recording-");
    let path = dir.join(format!("{stem}.txt"));
    std::fs::write(&path, text).map_err(|error| error.to_string())?;
    Ok(path.to_string_lossy().into_owned())
}

/// Deletes every failed recording (and its audio) so history stays clean.
/// Runs one at a time through the same safe deletion path as the UI.
#[tauri::command(rename_all = "camelCase")]
pub fn clear_failed_recordings(state: State<'_, AppState>) -> Result<usize, String> {
    let recordings = state
        .storage
        .list_history(&HistoryQuery {
            search: None,
            status: Some(RecordingStatus::Failed),
        })
        .map_err(|error| error.to_string())?;
    let mut deleted = 0usize;
    for recording in recordings {
        if delete_history_safely(&state.machine, state.storage.as_ref(), &recording.id).map_err(|error| error.to_string())? {
            deleted += 1;
        }
    }
    Ok(deleted)
}

#[tauri::command(rename_all = "camelCase")]
pub async fn play_recording(
    recording_id: String,
    state: State<'_, AppState>,
) -> Result<(), String> {
    let recording = state
        .storage
        .get_recording(&recording_id)
        .map_err(|error| error.to_string())?
        .ok_or_else(|| "recording not found".to_owned())?;
    let Some(audio_path) = recording.audio_path else {
        return Err("nagranie nie ma zapisanego audio".into());
    };
    let path = PathBuf::from(&audio_path);
    if !path.is_file() {
        return Err(format!("brak pliku audio: {}", path.display()));
    }
    if path.extension().and_then(|extension| extension.to_str()) != Some("wav") {
        return Err("Only local WAV files are supported".into());
    }
    tauri::async_runtime::spawn_blocking(move || crate::sound::play_file(&path))
        .await
        .map_err(|error| error.to_string())?
}

/// Returns the raw WAV bytes for a recording so the UI can play it back with a
/// seekable, in-app `<audio>` element instead of blocking on the OS player.
#[tauri::command(rename_all = "camelCase")]
pub fn read_recording_audio(
    recording_id: String,
    state: State<'_, AppState>,
) -> Result<Vec<u8>, String> {
    let recording = state
        .storage
        .get_recording(&recording_id)
        .map_err(|error| error.to_string())?
        .ok_or_else(|| "recording not found".to_owned())?;
    let Some(audio_path) = recording.audio_path else {
        return Err("nagranie nie ma zapisanego audio".into());
    };
    let path = PathBuf::from(&audio_path);
    if !path.is_file() {
        return Err(format!("brak pliku audio: {}", path.display()));
    }
    std::fs::read(&path).map_err(|error| format!("nie udało się odczytać audio: {error}"))
}

#[tauri::command(rename_all = "camelCase")]
pub fn reveal_recording(
    recording_id: String,
    state: State<'_, AppState>,
) -> Result<(), String> {
    let recording = state
        .storage
        .get_recording(&recording_id)
        .map_err(|error| error.to_string())?
        .ok_or_else(|| "recording not found".to_owned())?;
    let Some(audio_path) = recording.audio_path else {
        return Err("nagranie nie ma zapisanego audio".into());
    };
    let path = PathBuf::from(&audio_path);
    if !path.is_file() {
        return Err(format!("brak pliku audio: {}", path.display()));
    }
    platform::reveal_path(&path).map_err(|error| error.to_string())
}

#[tauri::command(rename_all = "camelCase")]
pub fn reveal_recordings_dir(state: State<'_, AppState>) -> Result<(), String> {
    platform::reveal_path(state.storage.recordings_dir()).map_err(|error| error.to_string())
}

#[tauri::command(rename_all = "camelCase")]
pub fn reveal_model_dir(model: String, state: State<'_, AppState>) -> Result<(), String> {
    if crate::models::spec(&model).is_none() {
        return Err(format!("Nieznany model: {model}."));
    }
    let path = state.engine.model_dir(&model);
    platform::reveal_path(&path).map_err(|error| error.to_string())
}

#[tauri::command(rename_all = "camelCase")]
pub fn correct_transcript(
    recording_id: String,
    text: String,
    state: State<'_, AppState>,
) -> Result<usize, String> {
    let recording = state
        .storage
        .get_recording(&recording_id)
        .map_err(|error| error.to_string())?
        .ok_or_else(|| "recording not found".to_owned())?;
    let previous = recording.text.unwrap_or_default();
    let learned = learn_vocabulary(&previous, &text, |heard, replacement| {
        state
            .storage
            .add_vocabulary(heard, replacement)
            .map(|_| ())
            .map_err(|error| error.to_string())
    })?;
    state
        .storage
        .update_status(&recording_id, RecordingStatus::Completed, Some(&text), None, None, None)
        .map_err(|error| error.to_string())?;
    Ok(learned)
}

fn learn_vocabulary(
    previous: &str,
    corrected: &str,
    mut add: impl FnMut(&str, &str) -> Result<(), String>,
) -> Result<usize, String> {
    let previous_words = tokenize_words(previous);
    let corrected_words = tokenize_words(corrected);
    let mut learned = 0;
    for corrected_word in &corrected_words {
        let lower = corrected_word.to_lowercase();
        if lower.chars().all(|c| c.is_ascii_alphanumeric()) && lower.len() > 2 {
            let matches_previous = previous_words.iter().any(|previous_word| {
                previous_word.to_lowercase() == lower
                    || levenshtein_words(previous_word, corrected_word) <= 1
            });
            if !matches_previous {
                add(&lower, corrected_word)?;
                learned += 1;
            }
        }
    }
    Ok(learned)
}

fn tokenize_words(text: &str) -> Vec<String> {
    text.split(|c: char| !c.is_alphanumeric())
        .filter(|word| !word.is_empty())
        .map(str::to_owned)
        .collect()
}

fn levenshtein_words(left: &str, right: &str) -> usize {
    let left = left.chars().collect::<Vec<_>>();
    let right = right.chars().collect::<Vec<_>>();
    let mut previous = (0..=right.len()).collect::<Vec<_>>();
    let mut current = vec![0; right.len() + 1];
    for (i, lc) in left.iter().enumerate() {
        current[0] = i + 1;
        for (j, rc) in right.iter().enumerate() {
            let cost = if lc == rc { 0 } else { 1 };
            current[j + 1] = (previous[j + 1] + 1)
                .min(current[j] + 1)
                .min(previous[j] + cost);
        }
        std::mem::swap(&mut previous, &mut current);
    }
    previous[right.len()]
}

fn delete_history_safely(
    machine: &Mutex<CoordinatorMachine>,
    storage: &Storage,
    recording_id: &str,
) -> Result<bool, AppError> {
    let machine = machine.lock().map_err(|_| AppError::Busy {
        recording_id: recording_id.to_owned(),
    })?;
    match machine.snapshot() {
        DictationState::Recording {
            recording_id: active,
            ..
        } if active == recording_id => {
            return Err(AppError::ActiveRecording {
                recording_id: recording_id.to_owned(),
            });
        }
        DictationState::Processing {
            recording_id: active,
            ..
        }
        | DictationState::Pasting {
            recording_id: active,
            ..
        } if active == recording_id => {
            return Err(AppError::Busy {
                recording_id: recording_id.to_owned(),
            });
        }
        _ => {}
    }

    let Some(recording) = storage.get_recording(recording_id)? else {
        return Ok(false);
    };
    match recording.status {
        RecordingStatus::Recording => {
            return Err(AppError::ActiveRecording {
                recording_id: recording_id.to_owned(),
            });
        }
        RecordingStatus::Processing => {
            return Err(AppError::Busy {
                recording_id: recording_id.to_owned(),
            });
        }
        RecordingStatus::Completed | RecordingStatus::Failed | RecordingStatus::Cancelled => {}
    }
    if let Some(audio_path) = recording.audio_path {
        remove_managed_audio(Path::new(&audio_path), storage.recordings_dir())?;
    }
    let deleted = storage.delete_recording(recording_id)?;
    drop(machine);
    Ok(deleted)
}

fn remove_managed_audio(path: &Path, managed_dir: &Path) -> Result<(), AppError> {
    if !path.exists() {
        return Ok(());
    }
    let managed = managed_dir.canonicalize().map_err(|error| AppError::Io {
        message: error.to_string(),
    })?;
    let canonical = path.canonicalize().map_err(|error| AppError::Io {
        message: error.to_string(),
    })?;
    if !canonical.starts_with(&managed)
        || canonical
            .extension()
            .is_none_or(|extension| extension != "wav")
    {
        return Err(AppError::Io {
            message: "refusing to delete audio outside the managed recordings directory".into(),
        });
    }
    std::fs::remove_file(canonical).map_err(|error| AppError::Io {
        message: error.to_string(),
    })
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

fn validate_active_mode(storage: &Storage, id: &str) -> Result<Mode, String> {
    match storage.get_mode(id).map_err(|error| error.to_string())? {
        None => Err(format!("Tryb „{id}” nie istnieje.")),
        Some(mode) if !mode.enabled => Err(format!("Mode \"{}\" is disabled.", mode.name)),
        Some(mode) => Ok(mode),
    }
}

fn validate_mode_upsert(active_mode: &str, mode: &Mode) -> Result<(), String> {
    if mode.id == active_mode && !mode.enabled {
        Err("Cannot disable the mode that is currently in use.".into())
    } else {
        Ok(())
    }
}

fn validate_mode_delete(active_mode: &str, id: &str) -> Result<(), String> {
    if id == active_mode {
        Err("Cannot delete the mode that is currently in use.".into())
    } else {
        Ok(())
    }
}

#[tauri::command(rename_all = "camelCase")]
pub fn upsert_mode(mode: Mode, state: State<'_, AppState>) -> Result<(), String> {
    let _update_guard = state
        .settings_update
        .lock()
        .map_err(|_| "settings update lock poisoned")?;
    let active_mode = state
        .settings
        .read()
        .map_err(|_| "settings lock poisoned")?
        .active_mode
        .clone();
    validate_mode_upsert(&active_mode, &mode)?;
    state
        .storage
        .upsert_mode(&mode)
        .map_err(|error| error.to_string())
}

#[tauri::command(rename_all = "camelCase")]
pub fn delete_mode(id: String, state: State<'_, AppState>) -> Result<bool, String> {
    let _update_guard = state
        .settings_update
        .lock()
        .map_err(|_| "settings update lock poisoned")?;
    let active_mode = state
        .settings
        .read()
        .map_err(|_| "settings lock poisoned")?
        .active_mode
        .clone();
    validate_mode_delete(&active_mode, &id)?;
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

/// Returns the primary OS UI language subtag, lowercase ("pl" or "en"),
/// falling back to "en" when the OS language is unknown or unsupported.
fn platform_ui_language() -> String {
    #[cfg(windows)]
    {
        use windows_sys::Win32::Globalization::GetUserDefaultUILanguage;
        let langid = unsafe { GetUserDefaultUILanguage() };
        let primary = langid & 0x3FF;
        return match primary {
            0x15 => "pl".to_string(), // LANG_POLISH
            0x09 => "en".to_string(), // LANG_ENGLISH
            _ => "en".to_string(),
        };
    }
    #[cfg(not(windows))]
    {
        let locale = std::env::var("LC_ALL")
            .or_else(|_| std::env::var("LANG"))
            .unwrap_or_default();
        let language = locale
            .split('.')
            .next()
            .unwrap_or("")
            .split('_')
            .next()
            .unwrap_or("")
            .to_ascii_lowercase();
        return match language.as_str() {
            "pl" => "pl".to_string(),
            "en" => "en".to_string(),
            _ => "en".to_string(),
        };
    }
}

#[tauri::command]
pub fn system_locale() -> Result<String, String> {
    Ok(platform_ui_language())
}

/// Resolves the OS UI language to a static subtag ("pl" or "en"), sharing the
/// exact same detection logic as the `system_locale` command.
pub fn system_lang() -> &'static str {
    match platform_ui_language().as_str() {
        "pl" => "pl",
        _ => "en",
    }
}

#[tauri::command]
pub fn list_models(state: State<'_, AppState>) -> Vec<ModelDescriptor> {
    crate::models::CATALOGUE
        .iter()
        .map(|spec| {
            let installed = state.engine.is_installed(spec.key);
            ModelDescriptor {
                key: spec.key.to_owned(),
                id: spec.repo.to_owned(),
                provider: spec.provider.to_owned(),
                source: "local".to_owned(),
                revision: String::new(),
                display: spec.display.to_owned(),
                min_vram_gb: 0.0,
                min_ram_gb: 2.0,
                languages: spec.languages.to_owned(),
                estimated_size_bytes: spec.total_bytes(),
                status: if installed {
                    ModelStatusState::Ready
                } else {
                    ModelStatusState::NotInstalled
                },
                installed_size_bytes: Some(crate::models::installed_bytes(
                    spec.key,
                    &state.engine.model_dir(spec.key),
                ))
                .filter(|size| *size > 0),
                message: None,
            }
        })
        .collect()
}



#[tauri::command]
pub async fn download_model(
    model: String,
    app: AppHandle,
    state: State<'_, AppState>,
) -> Result<(), String> {
    let spec =
        crate::models::spec(&model).ok_or_else(|| format!("Nieznany model: {model}."))?;
    let directory = state.engine.model_dir(&model);
    let total = spec.total_bytes();
    let progress_app = app.clone();
    let progress_model = model.clone();
    emit_model_progress(&app, &model, "downloading", 0, Some(total));

    let fetch_model = model.clone();
    tauri::async_runtime::spawn_blocking(move || {
        crate::models::download(
            &fetch_model,
            &directory,
            |done, total| {
                emit_model_progress(&progress_app, &progress_model, "downloading", done, Some(total));
            },
            || true,
        )
    })
    .await
    .map_err(|error| error.to_string())?
    .map_err(|error| error.to_string())?;

    // A model that has just arrived is the one the user means to dictate with,
    // so it is worth loading before they press the shortcut.
    warm_up_model(&app, &state);
    emit_state(&app, &state);
    Ok(())
}

#[tauri::command]
pub fn delete_model(model: String, state: State<'_, AppState>) -> Result<(), String> {
    if crate::models::spec(&model).is_none() {
        return Err(format!("Nieznany model: {model}."));
    }
    match state.snapshot()?.dictation {
        DictationState::Recording { .. }
        | DictationState::Processing { .. }
        | DictationState::Pasting { .. }
        | DictationState::Cancelling { .. } => {
            return Err("Stop dictating before deleting a model.".into());
        }
        DictationState::Idle | DictationState::Failed { .. } => {}
    }
    let active_model = state
        .settings
        .read()
        .map_err(|_| "settings lock poisoned")?
        .model
        .clone();
    if active_model == model {
        return Err("Select another model before deleting the active one.".into());
    }
    let cache = state.model_home.join(&model);
    match std::fs::remove_dir_all(&cache) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(format!("Could not delete the model: {error}")),
    }
}

#[tauri::command]
pub fn get_model_status(state: State<'_, AppState>) -> ModelStatus {
    let model = state
        .settings
        .read()
        .map(|settings| settings.model.clone())
        .unwrap_or_else(|_| default_model());
    let Some(spec) = crate::models::spec(&model) else {
        return ModelStatus {
            state: ModelStatusState::Error,
            model: model.clone(),
            revision: String::new(),
            device: None,
            message: Some(format!("Nieznany model: {model}.")),
        };
    };
    let installed = state.engine.is_installed(&model);
    ModelStatus {
        state: if installed {
            ModelStatusState::Ready
        } else {
            ModelStatusState::NotInstalled
        },
        model: spec.display.to_owned(),
        revision: String::new(),
        // Reported so the interface can say whether the graphics card is in
        // use without asking the user what hardware they have.
        device: state
            .engine
            .accelerator()
            .map(|kind| format!("{kind:?}").to_lowercase()),
        message: None,
    }
}

#[tauri::command(rename_all = "camelCase")]
pub fn update_settings(
    settings: AppSettings,
    app: AppHandle,
    state: State<'_, AppState>,
) -> Result<SettingsUpdateResult, String> {
    let retention = retention_policy(settings.retention_days)?;
    if crate::models::spec(&settings.model).is_none() {
        return Err(format!("Nieznany model: {}.", settings.model));
    }
    let _update_guard = state
        .settings_update
        .lock()
        .map_err(|_| "settings update lock poisoned")?;
    validate_active_mode(&state.storage, &settings.active_mode)?;
    let mut live = state
        .settings
        .read()
        .map_err(|_| "settings lock poisoned")?
        .clone();
    let mut shortcut_effect = TauriShortcutSettings { app: &app };
    let mut autostart_effect = TauriAutostartSettings { app: &app };
    let mut storage_effect = PersistentSettings {
        storage: state.storage.as_ref(),
        retention,
    };
    let old_language = live.language.clone();
    let result = apply_settings_change(
        &mut shortcut_effect,
        &mut autostart_effect,
        &mut storage_effect,
        &mut live,
        settings,
    )?;
    let language_changed = old_language != live.language;
    let model_changed = {
        let current = state
            .settings
            .read()
            .map_err(|_| "settings lock poisoned")?
            .model
            .clone();
        current != live.model
    };
    *state
        .settings
        .write()
        .unwrap_or_else(std::sync::PoisonError::into_inner) = live;
    if model_changed {
        // The old model is of no further use, and holding two at once would
        // double the memory for as long as the new one takes to load.
        state.engine.unload();
        state.model_warm.forget();
        warm_up_model(&app, &state);
    }
    if language_changed {
        let settings_guard = state
            .settings
            .read()
            .map_err(|_| "settings lock poisoned")?;
        let locale = crate::resolved_language(&settings_guard).to_owned();
        crate::rebuild_tray_menu(&app, &state, &locale);
    }
    emit_state(&app, &state);
    Ok(result)
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
    use std::fs;

    fn history_recording(id: &str, status: RecordingStatus, audio_path: &Path) -> Recording {
        Recording {
            id: id.to_owned(),
            created_at: 1,
            duration_ms: 10,
            status,
            text: None,
            model: None,
            audio_path: Some(audio_path.to_string_lossy().into_owned()),
            source_app: None,
            error: None,
            peaks: None,
        }
    }

    #[test]
    fn escape_is_held_only_while_a_recording_could_be_discarded() {
        // A global hotkey is exclusive: holding Escape permanently would stop
        // every other application from ever seeing the key.
        let recording = DictationState::Recording {
            recording_id: "r".into(),
            audio_path: "r.wav".into(),
        };
        let cancelling = DictationState::Cancelling {
            recording_id: "r".into(),
            audio_path: "r.wav".into(),
        };

        assert!(holds_discardable_audio(&recording));
        assert!(holds_discardable_audio(&cancelling));
    }

    #[test]
    fn escape_is_released_once_capture_is_over() {
        for state in [
            DictationState::Idle,
            DictationState::Processing {
                recording_id: "r".into(),
                audio_path: "r.wav".into(),
            },
            DictationState::Pasting {
                recording_id: "r".into(),
                audio_path: "r.wav".into(),
                transcript: "hello".into(),
            },
            DictationState::Failed {
                recovery: RecoveryRecording {
                    recording_id: "r".into(),
                    audio_path: "r.wav".into(),
                },
                error: "boom".into(),
            },
        ] {
            assert!(!holds_discardable_audio(&state), "{state:?}");
        }
    }

    #[test]
    fn a_cancel_press_asks_before_it_discards_audio() {
        let recording = DictationState::Recording {
            recording_id: "r".into(),
            audio_path: "r.wav".into(),
        };

        assert_eq!(cancel_press_for(&recording), CancelPress::Ask);
    }

    #[test]
    fn pressing_cancel_again_answers_the_question() {
        // The pill has no keyboard focus, so the same global key has to both
        // raise the question and answer it. Without this the prompt could be
        // opened but never confirmed.
        let cancelling = DictationState::Cancelling {
            recording_id: "r".into(),
            audio_path: "r.wav".into(),
        };

        assert_eq!(cancel_press_for(&cancelling), CancelPress::Confirm);
    }

    #[test]
    fn cancel_does_nothing_when_there_is_nothing_to_discard() {
        for state in [
            DictationState::Idle,
            DictationState::Processing {
                recording_id: "r".into(),
                audio_path: "r.wav".into(),
            },
            DictationState::Pasting {
                recording_id: "r".into(),
                audio_path: "r.wav".into(),
                transcript: "hello".into(),
            },
        ] {
            assert_eq!(cancel_press_for(&state), CancelPress::Ignore, "{state:?}");
        }
    }

    #[test]
    fn deleting_a_recording_in_progress_is_rejected_without_touching_history_or_audio() {
        let temp = tempfile::tempdir().unwrap();
        let recordings_dir = temp.path().join("recordings");
        fs::create_dir_all(&recordings_dir).unwrap();
        let storage = Storage::open_in_memory(&recordings_dir).unwrap();
        let audio_path = recordings_dir.join("active.wav");
        fs::write(&audio_path, b"wav").unwrap();
        storage
            .insert_recording(&history_recording(
                "active",
                RecordingStatus::Recording,
                &audio_path,
            ))
            .unwrap();
        let machine = Mutex::new(CoordinatorMachine::default());
        machine
            .lock()
            .unwrap()
            .started("active", audio_path.to_string_lossy())
            .unwrap();

        let error = delete_history_safely(&machine, &storage, "active").unwrap_err();

        assert!(matches!(error, AppError::ActiveRecording { .. }));
        assert!(storage.get_recording("active").unwrap().is_some());
        assert!(audio_path.is_file());
    }

    #[test]
    fn deleting_processing_history_is_rejected_without_touching_history_or_audio() {
        let temp = tempfile::tempdir().unwrap();
        let recordings_dir = temp.path().join("recordings");
        fs::create_dir_all(&recordings_dir).unwrap();
        let storage = Storage::open_in_memory(&recordings_dir).unwrap();
        let audio_path = recordings_dir.join("processing.wav");
        fs::write(&audio_path, b"wav").unwrap();
        storage
            .insert_recording(&history_recording(
                "processing",
                RecordingStatus::Processing,
                &audio_path,
            ))
            .unwrap();
        let machine = Mutex::new(CoordinatorMachine {
            state: DictationState::Processing {
                recording_id: "processing".into(),
                audio_path: audio_path.to_string_lossy().into_owned(),
            },
        });

        let error = delete_history_safely(&machine, &storage, "processing").unwrap_err();

        assert!(matches!(error, AppError::Busy { .. }));
        assert!(storage.get_recording("processing").unwrap().is_some());
        assert!(audio_path.is_file());
    }

    #[test]
    fn deleting_completed_and_failed_history_removes_rows_and_managed_audio() {
        let temp = tempfile::tempdir().unwrap();
        let recordings_dir = temp.path().join("recordings");
        fs::create_dir_all(&recordings_dir).unwrap();
        let storage = Storage::open_in_memory(&recordings_dir).unwrap();
        let machine = Mutex::new(CoordinatorMachine::default());

        for (id, status) in [
            ("completed", RecordingStatus::Completed),
            ("failed", RecordingStatus::Failed),
        ] {
            let audio_path = recordings_dir.join(format!("{id}.wav"));
            fs::write(&audio_path, b"wav").unwrap();
            storage
                .insert_recording(&history_recording(id, status, &audio_path))
                .unwrap();

            assert!(delete_history_safely(&machine, &storage, id).unwrap());
            assert!(storage.get_recording(id).unwrap().is_none());
            assert!(!audio_path.exists());
        }
    }

    #[test]
    fn missing_history_row_cannot_be_marked_as_processing() {
        let temp = tempfile::tempdir().unwrap();
        let storage = Storage::open_in_memory(temp.path().join("recordings")).unwrap();

        let error = mark_processing(&storage, "missing", 10).unwrap_err();

        assert!(matches!(error, AppError::Storage { .. }));
    }

    #[test]
    fn retry_requires_a_failed_row_with_existing_finalized_audio() {
        let temp = tempfile::tempdir().unwrap();
        let final_audio = temp.path().join("final.wav");
        fs::write(&final_audio, b"wav").unwrap();
        let retryable =
            history_recording("retryable", RecordingStatus::Failed, final_audio.as_path());
        let mut interrupted = history_recording(
            "interrupted",
            RecordingStatus::Failed,
            final_audio.as_path(),
        );
        interrupted.audio_path = None;

        assert_eq!(retry_audio_path(&retryable).unwrap(), final_audio);
        assert!(matches!(
            retry_audio_path(&interrupted),
            Err(AppError::NotRetryable { .. })
        ));
    }

    #[test]
    fn simultaneous_lifecycle_operations_have_one_winner_and_one_busy_loser() {
        let gate = Arc::new(Mutex::new(()));
        let entered = Arc::new(std::sync::Barrier::new(2));
        let release = Arc::new(std::sync::Barrier::new(2));
        let worker_gate = gate.clone();
        let worker_entered = entered.clone();
        let worker_release = release.clone();
        let winner = std::thread::spawn(move || {
            let _guard = lifecycle_guard(&worker_gate)?;
            worker_entered.wait();
            worker_release.wait();
            Ok::<_, CoordinatorError>("stop")
        });
        entered.wait();

        let loser = lifecycle_guard(&gate);
        release.wait();

        assert_eq!(winner.join().unwrap().unwrap(), "stop");
        assert_eq!(loser.unwrap_err(), CoordinatorError::Busy);
    }

    #[test]
    fn simultaneous_start_and_retry_cannot_overwrite_the_reserved_operation() {
        let gate = Arc::new(Mutex::new(()));
        let owner = Arc::new(Mutex::new(None));
        let entered = Arc::new(std::sync::Barrier::new(2));
        let release = Arc::new(std::sync::Barrier::new(2));
        let worker_gate = gate.clone();
        let worker_owner = owner.clone();
        let worker_entered = entered.clone();
        let worker_release = release.clone();
        let start = std::thread::spawn(move || {
            let _guard = lifecycle_guard(&worker_gate)?;
            *worker_owner.lock().unwrap() = Some("start");
            worker_entered.wait();
            worker_release.wait();
            Ok::<_, CoordinatorError>(())
        });
        entered.wait();

        let retry = lifecycle_guard(&gate);
        release.wait();

        start.join().unwrap().unwrap();
        assert_eq!(retry.unwrap_err(), CoordinatorError::Busy);
        assert_eq!(*owner.lock().unwrap(), Some("start"));
    }

    #[test]
    fn simultaneous_double_stop_has_one_commit_and_one_busy_result() {
        let gate = Arc::new(Mutex::new(()));
        let commits = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let entered = Arc::new(std::sync::Barrier::new(2));
        let release = Arc::new(std::sync::Barrier::new(2));
        let worker_gate = gate.clone();
        let worker_commits = commits.clone();
        let worker_entered = entered.clone();
        let worker_release = release.clone();
        let first = std::thread::spawn(move || {
            let _guard = lifecycle_guard(&worker_gate)?;
            worker_entered.wait();
            worker_commits.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            worker_release.wait();
            Ok::<_, CoordinatorError>(())
        });
        entered.wait();

        let second = lifecycle_guard(&gate);
        release.wait();

        first.join().unwrap().unwrap();
        assert_eq!(second.unwrap_err(), CoordinatorError::Busy);
        assert_eq!(commits.load(std::sync::atomic::Ordering::SeqCst), 1);
    }

    #[test]
    fn retention_reservation_wins_before_retry_and_makes_audio_non_retryable() {
        let temp = tempfile::tempdir().unwrap();
        let recordings = temp.path().join("recordings");
        fs::create_dir_all(&recordings).unwrap();
        let audio_path = recordings.join("failed.wav");
        fs::write(&audio_path, b"wav").unwrap();
        let storage = Storage::open_in_memory(&recordings).unwrap();
        let mut row = history_recording("failed", RecordingStatus::Failed, &audio_path);
        row.created_at = 0;
        storage.insert_recording(&row).unwrap();

        let reserved = storage
            .reserve_retention_audio(Retention::Days(1), 172_800_000)
            .unwrap();
        let reserved_row = storage.get_recording("failed").unwrap().unwrap();

        assert_eq!(reserved, vec![audio_path.canonicalize().unwrap()]);
        assert!(reserved_row.audio_path.is_none());
        assert!(matches!(
            retry_audio_path(&reserved_row),
            Err(AppError::NotRetryable { .. })
        ));
    }

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
    fn request_cancel_arms_and_dismisses_the_confirm_prompt() {
        let mut machine = CoordinatorMachine::default();
        machine.started("one", "one.wav").unwrap();

        machine.request_cancel().unwrap();
        assert_eq!(
            machine.snapshot(),
            DictationState::Cancelling {
                recording_id: "one".into(),
                audio_path: "one.wav".into(),
            }
        );

        machine.request_cancel().unwrap();
        assert_eq!(
            machine.snapshot(),
            DictationState::Recording {
                recording_id: "one".into(),
                audio_path: "one.wav".into(),
            }
        );
    }

    #[test]
    fn cancelling_prompt_can_confirm_cancel_or_finalize() {
        let mut machine = CoordinatorMachine::default();
        machine.started("one", "one.wav").unwrap();
        machine.request_cancel().unwrap();

        machine.cancel().unwrap();
        assert_eq!(machine.snapshot(), DictationState::Idle);
    }

    #[test]
    fn cancelling_prompt_can_finalize_instead_of_cancelling() {
        let mut machine = CoordinatorMachine::default();
        machine.started("one", "one.wav").unwrap();
        machine.request_cancel().unwrap();

        machine.stopped().unwrap();
        assert_eq!(
            machine.snapshot(),
            DictationState::Processing {
                recording_id: "one".into(),
                audio_path: "one.wav".into(),
            }
        );
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
    fn settings_default_to_not_launching_on_login() {
        assert!(!AppSettings::default().launch_on_login);
    }

    #[test]
    fn settings_from_older_versions_default_to_clean_mode_and_opt_out_of_login() {
        let settings: AppSettings = serde_json::from_value(serde_json::json!({
            "inputDevice": null,
            "shortcut": "Ctrl+Space",
            "autoPaste": true,
            "retentionDays": 30
        }))
        .unwrap();

        assert_eq!(settings.active_mode, "clean");
        assert!(!settings.launch_on_login);
        assert_eq!(
            serde_json::to_value(settings).unwrap()["activeMode"],
            serde_json::json!("clean")
        );
    }

    #[test]
    fn settings_from_older_versions_default_to_system_language() {
        let settings: AppSettings = serde_json::from_value(serde_json::json!({
            "inputDevice": null,
            "shortcut": "Ctrl+Space",
            "autoPaste": true,
            "retentionDays": 30
        }))
        .unwrap();

        assert_eq!(settings.language, LanguageChoice::System);
        assert_eq!(
            serde_json::to_value(&settings).unwrap()["language"],
            serde_json::json!("system")
        );
    }

    #[test]
    fn selected_mode_setting_controls_transcript_pipeline() {
        let vocabulary = vec![VocabularyEntry {
            id: 1,
            heard: "parakit".into(),
            replacement: "Parakeet".into(),
        }];
        let input = "pierwsza\n    parakit ;";
        let mut settings = AppSettings::default();

        let clean = Mode {
            id: "clean".into(),
            name: "Czysty".into(),
            description: String::new(),
            prompt: String::new(),
            enabled: true,
            is_default: true,
            created_at: 0,
        };
        assert_eq!(
            postprocess_with_mode(input, &vocabulary, Some(&clean)),
            "pierwsza Parakeet;"
        );
        settings.active_mode = "code".into();
        let code = Mode {
            id: "code".into(),
            name: "Kod".into(),
            ..clean
        };
        assert_eq!(
            postprocess_with_mode(input, &vocabulary, Some(&code)),
            "pierwsza\n    Parakeet;"
        );
    }

    #[test]
    fn active_mode_must_exist_and_be_enabled() {
        let temp = tempfile::tempdir().unwrap();
        let storage = Storage::open_in_memory(temp.path().join("recordings")).unwrap();
        let mut disabled = storage.get_mode("message").unwrap().unwrap();
        disabled.enabled = false;
        storage.upsert_mode(&disabled).unwrap();

        assert!(
            validate_active_mode(&storage, "missing")
                .unwrap_err()
                .contains("nie istnieje")
        );
        assert!(
            validate_active_mode(&storage, "message")
                .unwrap_err()
                .contains("disabled")
        );
        assert_eq!(validate_active_mode(&storage, "clean").unwrap().id, "clean");
    }

    #[test]
    fn active_custom_mode_cannot_be_disabled_or_deleted() {
        let temp = tempfile::tempdir().unwrap();
        let storage = Storage::open_in_memory(temp.path().join("recordings")).unwrap();
        let custom = Mode {
            id: "custom".into(),
            name: "Własny".into(),
            description: String::new(),
            prompt: "prefix: TEST".into(),
            enabled: true,
            is_default: false,
            created_at: 10,
        };
        storage.upsert_mode(&custom).unwrap();

        let mut disabled = custom.clone();
        disabled.enabled = false;
        assert!(validate_mode_upsert("custom", &disabled).is_err());
        assert!(validate_mode_delete("custom", "custom").is_err());
        assert!(validate_mode_delete("clean", "custom").is_ok());
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

    #[derive(Default)]
    struct OneShotCompletion {
        completed_attempts: std::cell::Cell<usize>,
        failed_attempts: std::cell::Cell<usize>,
    }

    impl CompletionWriter for OneShotCompletion {
        fn write_completed(
            &self,
            _recording_id: &str,
            _text: &str,
            _model: &str,
            _duration_ms: u64,
        ) -> Result<(), String> {
            let attempts = self.completed_attempts.get();
            self.completed_attempts.set(attempts + 1);
            if attempts == 0 {
                Err("completed write failed".into())
            } else {
                Ok(())
            }
        }

        fn write_failed(&self, _recording_id: &str, _error: &str) -> Result<(), String> {
            self.failed_attempts.set(self.failed_attempts.get() + 1);
            Ok(())
        }
    }

    #[test]
    fn terminal_write_failure_never_pastes_and_moves_to_failed() {
        let machine = Mutex::new(CoordinatorMachine {
            state: DictationState::Processing {
                recording_id: "one".into(),
                audio_path: "one.wav".into(),
            },
        });
        let storage = OneShotCompletion::default();
        let paste_calls = std::cell::Cell::new(0);

        let first = complete_transcription_durably(
            &machine,
            &storage,
            DurableTranscript {
                recording_id: "one",
                audio_path: "one.wav",
                transcript: "tekst",
                model: "model",
                duration_ms: 10,
            },
            || paste_calls.set(paste_calls.get() + 1),
        );

        assert_eq!(first.unwrap_err(), "completed write failed");
        assert_eq!(paste_calls.get(), 0);
        assert_eq!(storage.failed_attempts.get(), 1);
        assert!(matches!(
            machine.lock().unwrap().snapshot(),
            DictationState::Failed { .. }
        ));
    }

    #[test]
    fn paste_runs_only_after_completed_is_durable() {
        let machine = Mutex::new(CoordinatorMachine {
            state: DictationState::Processing {
                recording_id: "one".into(),
                audio_path: "one.wav".into(),
            },
        });
        let storage = OneShotCompletion::default();
        storage.completed_attempts.set(1);
        let paste_calls = std::cell::Cell::new(0);

        complete_transcription_durably(
            &machine,
            &storage,
            DurableTranscript {
                recording_id: "one",
                audio_path: "one.wav",
                transcript: "tekst",
                model: "model",
                duration_ms: 10,
            },
            || paste_calls.set(paste_calls.get() + 1),
        )
        .unwrap();

        assert_eq!(paste_calls.get(), 1);
        assert_eq!(machine.lock().unwrap().snapshot(), DictationState::Idle);
    }

    struct FakeRegistrar {
        active: String,
        fail_target: Option<String>,
    }

    impl ShortcutSettingsEffect for FakeRegistrar {
        fn validate(&self, shortcut: &str) -> Result<(), String> {
            crate::platform::Shortcut::parse(shortcut)
                .map(|_| ())
                .map_err(|error| error.to_string())
        }

        fn replace(&mut self, _old: &str, new: &str) -> Result<(), String> {
            if self.fail_target.as_deref() == Some(new) {
                return Err("shortcut conflict".into());
            }
            self.active = new.into();
            Ok(())
        }
    }

    struct FakeAutostart {
        enabled: bool,
        fail_value: Option<bool>,
    }

    impl AutostartSettingsEffect for FakeAutostart {
        fn apply(&mut self, enabled: bool) -> Result<(), String> {
            self.enabled = enabled;
            if self.fail_value == Some(enabled) {
                return Err("autostart failed".into());
            }
            Ok(())
        }
    }

    struct FakeSettingsStore {
        saved: AppSettings,
        fail_save: bool,
        cleanup_error: Option<String>,
    }

    impl SettingsStoreEffect for FakeSettingsStore {
        fn save(&mut self, settings: &AppSettings) -> Result<(), String> {
            if self.fail_save {
                return Err("database failed".into());
            }
            self.saved = settings.clone();
            Ok(())
        }

        fn cleanup_retention(&mut self) -> Result<(), String> {
            self.cleanup_error.clone().map_or(Ok(()), Err)
        }
    }

    fn changed_settings() -> (AppSettings, AppSettings) {
        let old = AppSettings::default();
        let new = AppSettings {
            shortcut: "Alt+Space".into(),
            launch_on_login: false,
            retention_days: Some(7),
            ..old.clone()
        };
        (old, new)
    }

    #[test]
    fn shortcut_conflict_keeps_all_old_settings_active() {
        let (mut live, new) = changed_settings();
        let old = live.clone();
        let mut registrar = FakeRegistrar {
            active: old.shortcut.clone(),
            fail_target: Some(new.shortcut.clone()),
        };
        let mut autostart = FakeAutostart {
            enabled: old.launch_on_login,
            fail_value: None,
        };
        let mut store = FakeSettingsStore {
            saved: old.clone(),
            fail_save: false,
            cleanup_error: None,
        };

        assert!(
            apply_settings_change(&mut registrar, &mut autostart, &mut store, &mut live, new)
                .is_err()
        );
        assert_eq!(registrar.active, old.shortcut);
        assert_eq!(autostart.enabled, old.launch_on_login);
        assert_eq!(store.saved, old);
        assert_eq!(live, store.saved);
    }

    #[test]
    fn autostart_failure_rolls_back_shortcut_and_live_settings() {
        let (mut live, new) = changed_settings();
        let old = live.clone();
        let mut registrar = FakeRegistrar {
            active: old.shortcut.clone(),
            fail_target: None,
        };
        let mut autostart = FakeAutostart {
            enabled: old.launch_on_login,
            fail_value: Some(new.launch_on_login),
        };
        let mut store = FakeSettingsStore {
            saved: old.clone(),
            fail_save: false,
            cleanup_error: None,
        };

        assert!(
            apply_settings_change(&mut registrar, &mut autostart, &mut store, &mut live, new)
                .is_err()
        );
        assert_eq!(registrar.active, old.shortcut);
        assert_eq!(autostart.enabled, old.launch_on_login);
        assert_eq!(live, old);
    }

    #[test]
    fn database_failure_rolls_back_shortcut_autostart_and_live_settings() {
        let (mut live, new) = changed_settings();
        let old = live.clone();
        let mut registrar = FakeRegistrar {
            active: old.shortcut.clone(),
            fail_target: None,
        };
        let mut autostart = FakeAutostart {
            enabled: old.launch_on_login,
            fail_value: None,
        };
        let mut store = FakeSettingsStore {
            saved: old.clone(),
            fail_save: true,
            cleanup_error: None,
        };

        assert!(
            apply_settings_change(&mut registrar, &mut autostart, &mut store, &mut live, new)
                .is_err()
        );
        assert_eq!(registrar.active, old.shortcut);
        assert_eq!(autostart.enabled, old.launch_on_login);
        assert_eq!(live, old);
    }

    #[test]
    fn successful_settings_commit_returns_cleanup_warning_without_rollback() {
        let (mut live, new) = changed_settings();
        let old = live.clone();
        let mut registrar = FakeRegistrar {
            active: old.shortcut.clone(),
            fail_target: None,
        };
        let mut autostart = FakeAutostart {
            enabled: old.launch_on_login,
            fail_value: None,
        };
        let mut store = FakeSettingsStore {
            saved: old,
            fail_save: false,
            cleanup_error: Some("cleanup locked".into()),
        };

        let result = apply_settings_change(
            &mut registrar,
            &mut autostart,
            &mut store,
            &mut live,
            new.clone(),
        )
        .unwrap();

        assert_eq!(result.settings, new);
        assert_eq!(result.warning.as_deref(), Some("cleanup locked"));
        assert_eq!(registrar.active, new.shortcut);
        assert_eq!(autostart.enabled, new.launch_on_login);
        assert_eq!(store.saved, new);
        assert_eq!(live, new);
    }

    #[test]
    fn model_warmup_waits_for_background_ready() {
        let warm = std::sync::Arc::new(ModelWarmup::default());
        warm.begin_for("parakeet".into());
        let guard = warm.clone();
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(30));
            guard.finish_for("parakeet".into(), true);
        });

        warm.wait_until_ready_for("parakeet", Duration::from_secs(2))
            .unwrap();
        assert!(warm.is_ready_for("parakeet"));
    }

    #[test]
    fn model_warmup_marks_failure_and_does_not_block_transcription() {
        let warm = ModelWarmup::default();
        warm.begin_for("parakeet".into());
        warm.finish_for("parakeet".into(), false);

        warm.wait_until_ready_for("parakeet", Duration::from_secs(2))
            .unwrap();
        assert!(!warm.is_ready_for("parakeet"));
    }

    #[test]
    fn model_warmup_already_ready_returns_immediately() {
        let warm = ModelWarmup::default();
        warm.begin_for("parakeet".into());
        warm.finish_for("parakeet".into(), true);

        warm.wait_until_ready_for("parakeet", Duration::from_millis(1))
            .unwrap();
        assert!(warm.is_ready_for("parakeet"));
    }

    #[test]
    fn model_warmup_times_out_while_still_warming() {
        let warm = ModelWarmup::default();
        warm.begin_for("parakeet".into());

        let error = warm
            .wait_until_ready_for("parakeet", Duration::from_millis(20))
            .unwrap_err();

        // The wait reports a timeout so the caller knows the model was still
        // loading, rather than silently proceeding without it.
        assert!(matches!(error, EngineError::Load(_)), "{error:?}");
        assert!(error.to_string().contains("did not finish loading"), "{error}");
    }

    #[test]
    fn model_warmup_rewarms_when_model_changes() {
        let warm = std::sync::Arc::new(ModelWarmup::default());
        warm.begin_for("parakeet".into());
        warm.finish_for("parakeet".into(), true);
        assert!(warm.is_ready_for("parakeet"));

        warm.begin_for("whisper-turbo".into());
        let guard = warm.clone();
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(20));
            guard.finish_for("whisper-turbo".into(), true);
        });

        assert!(!warm.is_ready_for("whisper-turbo"));
        warm.wait_until_ready_for("whisper-turbo", Duration::from_secs(2))
            .unwrap();
        assert!(warm.is_ready_for("whisper-turbo"));
    }

    #[test]
    fn learn_vocabulary_adds_only_new_words_as_replacements() {
        let mut learned: Vec<(String, String)> = Vec::new();
        let count = learn_vocabulary(
            "uruchom parakeet i zobacz wynik",
            "uruchom Parakeet i zobacz rezultat",
            |heard, replacement| {
                learned.push((heard.to_owned(), replacement.to_owned()));
                Ok(())
            },
        )
        .unwrap();

        assert_eq!(count, 1);
        assert_eq!(learned, vec![("rezultat".to_owned(), "rezultat".to_owned())]);
    }

    #[test]
    fn learn_vocabulary_ignores_identical_and_short_words() {
        let mut learned: Vec<(String, String)> = Vec::new();
        let count = learn_vocabulary(
            "idzmy do domu",
            "idzmy do domu teraz",
            |heard, replacement| {
                learned.push((heard.to_owned(), replacement.to_owned()));
                Ok(())
            },
        )
        .unwrap();

        assert_eq!(count, 1);
        assert_eq!(learned, vec![("teraz".to_owned(), "teraz".to_owned())]);
    }
}
