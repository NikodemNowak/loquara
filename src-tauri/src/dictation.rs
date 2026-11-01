use crate::cloud;
use crate::hold::{HoldController, HoldGesture};
use crate::engine::{Engine, EngineError};
use crate::audio::{
    AudioRecorder, CompletedRecording, InputDeviceInfo, cleanup_partial, quantize_peaks,
};
use crate::secret;
use crate::streaming;
use crate::domain::{DictationEvent, DictationState, transition};
use crate::platform::{self, PasteMode, SystemWindows, WindowTarget, WindowsApi};
use crate::storage::{
    HistoryQuery, Mode, Recording, RecordingStatus, Retention, Storage, StorageError,
    VocabularyEntry, postprocess_for_mode,
};
use crate::transcription::TranscriptionResult;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
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
            DictationState::Idle
                | DictationState::Failed { .. }
                | DictationState::Cancelling { .. }
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
                | DictationState::Processing { .. }
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
pub enum OverlaySize {
    Mini,
    Large,
}

/// Where the words come from: the model on this machine, or a provider's API.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TranscriptionSource {
    #[default]
    Local,
    Cloud,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AppSettings {
    pub input_device: Option<String>,
    #[serde(default)]
    pub microphones: Option<crate::microphones::MicrophonePreferences>,
    pub shortcut: String,
    #[serde(default)]
    pub activation_mode: ActivationMode,
    pub auto_paste: bool,
    pub retention_days: Option<u32>,
    #[serde(default = "default_launch_on_login")]
    pub launch_on_login: bool,
    /// Start with the window in the tray rather than on screen.
    ///
    /// Pairs with launching on login: an app that opens itself every time the
    /// computer starts is in the way, and Loquara is useful from the shortcut
    /// alone.
    #[serde(default)]
    pub start_minimized: bool,
    #[serde(default = "default_active_mode")]
    pub active_mode: String,
    #[serde(default = "default_show_overlay")]
    pub show_overlay: bool,
    #[serde(default = "default_overlay_size")]
    pub overlay_size: OverlaySize,
    #[serde(default = "default_model")]
    pub model: String,
    #[serde(default = "default_streaming")]
    pub streaming: bool,
    #[serde(default)]
    pub realtime_by_engine: std::collections::BTreeMap<String, bool>,
    #[serde(default = "default_language")]
    pub language: LanguageChoice,
    /// How many seconds the loaded model stays in memory after the last use.
    /// 0 means "always" (never unload).
    #[serde(default)]
    pub model_keep_alive_secs: u64,
    #[serde(default)]
    pub paste_mode: PasteMode,
    /// Local engine or a cloud provider. Local is the default so an existing
    /// installation keeps transcribing on this machine after an update.
    #[serde(default)]
    pub transcription_source: TranscriptionSource,
    #[serde(default = "default_cloud_provider")]
    pub cloud_provider: String,
    /// Empty means "the provider's default model", which keeps the choice
    /// alive when a provider renames its models.
    #[serde(default)]
    pub cloud_model: String,
    /// Empty means "the provider's own address"; only custom providers need
    /// this to be filled in.
    #[serde(default)]
    pub cloud_base_url: String,
    /// ISO-639-1 code. Empty lets the model detect the language.
    #[serde(default)]
    pub cloud_language: String,
}

const fn default_launch_on_login() -> bool {
    false
}

const fn default_show_overlay() -> bool {
    true
}

const fn default_overlay_size() -> OverlaySize {
    OverlaySize::Mini
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

fn default_cloud_provider() -> String {
    cloud::default_provider().key.to_owned()
}

const fn default_language() -> LanguageChoice {
    LanguageChoice::System
}


#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ActivationMode {
    #[default]
    Toggle,
    PushToTalk,
}

/// The shortcut a fresh installation starts with.
///
/// `Ctrl+Space` is the Windows and Linux binding. macOS reserves it for
/// switching input sources, so the same chord is widened with Shift there
/// instead of fighting the system.
pub fn default_shortcut() -> &'static str {
    #[cfg(target_os = "macos")]
    {
        "Ctrl+Shift+Space"
    }
    #[cfg(not(target_os = "macos"))]
    {
        "Ctrl+Space"
    }
}

impl Default for AppSettings {
    fn default() -> Self {
        Self {
            input_device: None,
            microphones: None,
            shortcut: default_shortcut().into(),
            activation_mode: ActivationMode::Toggle,
            auto_paste: true,
            retention_days: Some(30),
            launch_on_login: false,
            start_minimized: false,
            active_mode: default_active_mode(),
            show_overlay: default_show_overlay(),
            overlay_size: default_overlay_size(),
            model: default_model(),
            streaming: default_streaming(),
            realtime_by_engine: Default::default(),
            language: default_language(),
            model_keep_alive_secs: 0,
            paste_mode: PasteMode::default(),
            transcription_source: TranscriptionSource::default(),
            cloud_provider: default_cloud_provider(),
            cloud_model: String::new(),
            cloud_base_url: String::new(),
            cloud_language: String::new(),
        }
    }
}

/// The model a cloud transcription will actually ask for.
fn resolve_cloud_model(settings: &AppSettings) -> String {
    if !settings.cloud_model.trim().is_empty() {
        return settings.cloud_model.trim().to_owned();
    }
    cloud::provider(&settings.cloud_provider)
        .unwrap_or_else(|| cloud::default_provider())
        .default_model
        .to_owned()
}

/// Vocabulary turned into a provider's own hint list.
///
/// The replacement is what the user wants written, so that is the spelling a
/// provider should be biased towards.
fn keyterms_for(vocabulary: &[VocabularyEntry]) -> Vec<String> {
    vocabulary
        .iter()
        .map(|entry| entry.replacement.clone())
        .collect()
}

/// Where provider keys live in the settings table, still encrypted.
fn stored_cloud_keys(storage: &Storage) -> std::collections::BTreeMap<String, String> {
    storage
        .get_setting::<std::collections::BTreeMap<String, String>>("cloud_keys")
        .ok()
        .flatten()
        .unwrap_or_default()
}

/// One provider's key, decrypted, or `None` when the user has not set one.
///
/// A key that cannot be decrypted — a database copied from another Windows
/// account, say — counts as absent, because a request sending it would only
/// fail at the provider.
fn cloud_api_key(storage: &Storage, provider: &str) -> Result<Option<String>, String> {
    let Some(stored) = stored_cloud_keys(storage).get(provider).cloned() else {
        return Ok(None);
    };
    match secret::unprotect(&stored) {
        Ok(key) if !key.trim().is_empty() => Ok(Some(key)),
        Ok(_) => Ok(None),
        Err(_) => Ok(None),
    }
}

/// Providers that have a usable key, for the interface to tick off.
fn available_cloud_keys(storage: &Storage) -> Vec<String> {
    stored_cloud_keys(storage)
        .keys()
        .filter(|provider| cloud_api_key(storage, provider).ok().flatten().is_some())
        .cloned()
        .collect()
}

/// Whether this take should be transcribed while the user is still speaking.
fn streaming_wanted(settings: &AppSettings) -> bool {
    match settings.transcription_source {
        TranscriptionSource::Local => settings.model == "parakeet"
            && settings.realtime_by_engine.get("local:parakeet").copied().unwrap_or(true),
        TranscriptionSource::Cloud => settings.realtime_by_engine
            .get(&format!("cloud:{}", settings.cloud_provider)).copied().unwrap_or(settings.streaming)
            && cloud::provider(&settings.cloud_provider).is_some_and(|provider| provider.streaming),
    }
}

/// "xAI (Grok) · grok-voice-transcribe-2.0", the label history stores.
fn cloud_model_label(settings: &AppSettings) -> String {
    let provider = cloud::provider(&settings.cloud_provider)
        .unwrap_or_else(|| cloud::default_provider());
    format!("{} · {}", provider.display, resolve_cloud_model(settings))
}

#[derive(Clone)]
struct FrozenTake {
    recording_id: String,
    settings: AppSettings,
    vocabulary: Vec<VocabularyEntry>,
    mode: Option<Mode>,
    target: Option<WindowTarget>,
    cancel: Arc<AtomicBool>,
    preview_stop: Arc<AtomicBool>,
    draft_fallback: Arc<AtomicBool>,
    hold: Option<Arc<HoldGesture>>,
}

impl FrozenTake {
    fn capture(state: &AppState, settings: AppSettings, target: Option<WindowTarget>, recording_id: String) -> Self {
        let vocabulary = state.storage.list_vocabulary().unwrap_or_default();
        let mode = state.storage.get_mode(&settings.active_mode).ok().flatten().filter(|mode| mode.enabled);
        Self { recording_id, settings, vocabulary, mode, target,
            hold: None, cancel: Arc::new(AtomicBool::new(false)), preview_stop: Arc::new(AtomicBool::new(false)), draft_fallback: Arc::new(AtomicBool::new(false)) }
    }
    fn owns(&self, state: &AppState) -> bool {
        !self.cancel.load(Ordering::SeqCst) && !self.hold.as_ref().is_some_and(|hold| hold.cancelled()) && self.is_current(state)
    }
    fn is_current(&self, state: &AppState) -> bool {
        state.active_take.lock().ok().and_then(|take| take.as_ref().map(|active| Arc::ptr_eq(&active.cancel, &self.cancel))).unwrap_or(false)
    }
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LiveTranscript {
    pub recording_id: String,
    pub text: String,
    pub revision: u64,
    pub status: String,
    pub clipboard_ready: bool,
}

/// Providers replace this pending value freely. A dedicated publisher coalesces
/// UI and clipboard effects; it never exposes keyboard or focus operations.
struct BufferedDraftSink { pending: Arc<Mutex<Option<String>>>, app: AppHandle, state: AppState, take: FrozenTake }
impl streaming::TranscriptSink for BufferedDraftSink {
    fn publish(&self, text: &str) -> Result<(), String> {
        if self.take.cancel.load(Ordering::SeqCst) || self.take.preview_stop.load(Ordering::SeqCst) { return Ok(()); }
        *self.pending.lock().map_err(|_| "draft lock poisoned")? = Some(text.to_owned());
        Ok(())
    }
    fn failed(&self, _message: &str) {
        self.take.draft_fallback.store(true, Ordering::SeqCst);
        // WAV recognition remains available; the scoped status explains the transition.
        if let Ok(_gate) = self.state.lifecycle.try_lock() && self.take.owns(&self.state) {
            if let Ok(mut live) = self.state.live_transcript.lock() && let Some(live) = live.as_mut() { live.status = "fallback".into(); }
            emit_state(&self.app, &self.state);
        }
    }
}

fn start_draft_publisher(app: &AppHandle, state: &AppState, take: &FrozenTake) -> Arc<dyn streaming::TranscriptSink> {
    let pending = Arc::new(Mutex::new(None::<String>));
    let sink = Arc::new(BufferedDraftSink { pending: pending.clone(), app: app.clone(), state: state.clone(), take: take.clone() });
    let state = state.clone(); let app = app.clone(); let take = take.clone();
    std::thread::spawn(move || {
        while !take.preview_stop.load(Ordering::SeqCst) && !take.cancel.load(Ordering::SeqCst) {
            std::thread::sleep(Duration::from_millis(350));
            if take.draft_fallback.load(Ordering::SeqCst) {
                let Ok(_gate) = state.lifecycle.try_lock() else { continue };
                if take.owns(&state) {
                    if let Ok(mut live) = state.live_transcript.lock() && let Some(live) = live.as_mut() { live.status = "fallback".into(); }
                    emit_state(&app, &state);
                }
                break;
            }
            let text = pending.lock().ok().and_then(|mut pending| pending.take());
            if let Some(text) = text {
                if publish_draft(&state, &take, &text, &mut SystemWindows) { emit_state(&app, &state); }
                // Retry a temporarily locked clipboard, without losing a newer revision.
                let ready = state.live_transcript.lock().ok().and_then(|live| live.as_ref().map(|live| live.clipboard_ready)).unwrap_or(false);
                if !ready && !text.trim().is_empty() {
                    if let Ok(mut pending) = pending.lock() && pending.is_none() { *pending = Some(text); }
                }
            }
        }
    });
    sink
}

/// Sole draft consumer. A cancellation/new take and a clipboard write are
/// serialized, so no old worker can overwrite a subsequent external copy.
fn publish_draft(state: &AppState, take: &FrozenTake, text: &str, windows: &mut impl WindowsApi) -> bool {
    let Ok(_gate) = state.lifecycle.try_lock() else { return false };
    if !take.owns(state) || take.preview_stop.load(Ordering::SeqCst) || take.draft_fallback.load(Ordering::SeqCst) { return false; }
    let id = match state.machine.lock().ok().map(|machine| machine.snapshot()) {
        Some(DictationState::Recording { recording_id, .. }) => recording_id,
        _ => return false,
    };
    let mut live = match state.live_transcript.lock() { Ok(live) => live, Err(_) => return false };
    if live.as_ref().is_some_and(|live| live.text == text && live.clipboard_ready) { return false; }
    let clipboard_ready = !text.trim().is_empty() && windows.write_clipboard(text).is_ok();
    let revision = live.as_ref().map_or(1, |live| live.revision + 1);
    *live = Some(LiveTranscript { recording_id: id, text: text.to_owned(), revision, status: "draft".into(), clipboard_ready });
    true
}

/// All live capture conversion is bounded and separate from native decode.
/// The WAV writer remains the source of truth for final recognition.
struct ActiveLiveSession {
    recording_id: String,
    session: Option<streaming::Session>,
    cancellation: Option<streaming::Cancellation>,
    pump: Arc<Mutex<Option<streaming::AudioPump>>>,
    sender: Option<tokio::sync::mpsc::Sender<Vec<i16>>>,
    failure: Arc<Mutex<Option<String>>>,
}
impl ActiveLiveSession {
    fn flush(&mut self) {
        let result = self.pump.lock().map_err(|_| "audio pump lock poisoned".to_owned())
            .and_then(|mut pump| pump.as_mut().map_or(Ok(Vec::new()), |pump| pump.flush()));
        match result {
            Ok(tail) if !tail.is_empty() => {
                if let Some(sender) = &self.sender && sender.try_send(tail).is_err() {
                    *self.failure.lock().unwrap() = Some("live audio queue overflowed; using complete recording".into());
                }
            }
            Err(error) => { *self.failure.lock().unwrap() = Some(error); }
            _ => {}
        }
        if let Some(session) = &self.session && let Some(error) = self.failure.lock().ok().and_then(|error| error.clone()) { session.invalidate(error); }
        self.sender.take();
    }
}

fn start_live_session(app: &AppHandle, state: &AppState, take: &FrozenTake) -> Option<ActiveLiveSession> {
    let settings = &take.settings;
    if !streaming_wanted(settings) { return None; }
    let sink = start_draft_publisher(app, state, take);
    let failure = Arc::new(Mutex::new(None));
    let pump = Arc::new(Mutex::new(None::<streaming::AudioPump>));
    let buffer = Arc::new(Mutex::new(crate::buffered::PreviewBuffer::default()));
    let (session, sender) = if settings.transcription_source == TranscriptionSource::Cloud {
        let provider = cloud::provider(&settings.cloud_provider)?;
        let api_key = cloud_api_key(&state.storage, provider.key).ok().flatten()?;
        let (sender, receiver) = tokio::sync::mpsc::channel(128);
        let url = streaming::xai_stream_url(&settings.cloud_base_url, &resolve_cloud_model(settings), &settings.cloud_language, &keyterms_for(&take.vocabulary));
        (Some(streaming::spawn(streaming::Config { url, api_key }, sink.clone(), receiver)), Some(sender))
    } else {
        let engine = state.engine.clone(); let model = settings.model.clone();
        let stop = take.preview_stop.clone(); let cancel = take.cancel.clone(); let buffer = buffer.clone(); let sink = sink.clone();
        std::thread::spawn(move || {
            let mut last = 0;
            let mut preview = crate::buffered::LocalPreview::default();
            while !stop.load(Ordering::SeqCst) && !cancel.load(Ordering::SeqCst) {
                std::thread::sleep(Duration::from_millis(1800));
                if stop.load(Ordering::SeqCst) || cancel.load(Ordering::SeqCst) { break; }
                let (generation, samples) = match buffer.lock() { Ok(buffer) => buffer.snapshot(), Err(_) => break };
                if generation == last || samples.len() < 8_000 { continue; }
                last = generation;
                // One decode in flight. Slow machines skip intermediate snapshots.
                match preview.update(generation, &samples, |chunk| engine.transcribe_audio_samples(&model, chunk, &stop).map_err(|error| error.to_string())) {
                    Ok(text) if !stop.load(Ordering::SeqCst) && !cancel.load(Ordering::SeqCst) => { let _ = sink.publish(&text); }
                    Err(error) if !stop.load(Ordering::SeqCst) && !cancel.load(Ordering::SeqCst) => { sink.failed(&error.to_string()); break; }
                    _ => {}
                }
            }
        });
        (None, None)
    };
    let capture_pump = pump.clone(); let capture_sender = sender.clone(); let capture_failure = failure.clone();
    state.audio.set_stream_sink(Some(Arc::new(move |pcm, format| {
        let result = capture_pump.lock().map_err(|_| "audio pump lock poisoned".to_owned()).and_then(|mut pump| {
            if pump.is_none() { *pump = Some(streaming::AudioPump::new(format.channels, format.sample_rate)?); }
            pump.as_mut().unwrap().push(pcm)
        });
        match result {
            Ok(frames) if !frames.is_empty() => {
                if let Some(sender) = &capture_sender {
                    if sender.try_send(frames).is_err() { *capture_failure.lock().unwrap() = Some("live audio queue overflowed; using complete recording".into()); }
                } else if let Ok(mut buffer) = buffer.lock() { buffer.push(&frames); }
            }
            Err(error) => { *capture_failure.lock().unwrap() = Some(error); }
            _ => {}
        }
    })));
    let cancellation = session.as_ref().map(|session| session.cancellation());
    Some(ActiveLiveSession { recording_id: String::new(), session, cancellation, pump, sender, failure })
}

fn abandon_live_session(state: &AppState) {
    state.audio.set_stream_sink(None);
    if let Ok(take) = state.active_take.lock() && let Some(take) = take.as_ref() {
        take.cancel.store(true, Ordering::SeqCst);
        take.preview_stop.store(true, Ordering::SeqCst);
    }
    if let Ok(mut live) = state.live_transcript.lock() { *live = None; }
    if let Ok(mut slot) = state.streaming.lock() && let Some(live) = slot.take() && let Some(cancellation) = live.cancellation { cancellation.cancel(); }
}

/// Opens the provider's connection while the user is talking.
///
/// A TLS handshake plus DNS is a few hundred milliseconds; paying it while
/// the recording runs is paying it when nobody is waiting. The request itself
/// is the cheapest one the provider offers — a model list — and its answer is
/// not used.
fn warm_cloud_connection(state: &AppState, settings: &AppSettings) {
    let Some(provider) = cloud::provider(&settings.cloud_provider) else {
        return;
    };
    if !provider.lists_models {
        return;
    }
    let Ok(Some(api_key)) = cloud_api_key(&state.storage, provider.key) else {
        return;
    };
    let provider_key = provider.key;
    let base_url = settings.cloud_base_url.clone();
    std::thread::spawn(move || {
        if let Some(provider) = cloud::provider(provider_key) {
            let _ = cloud::list_models(provider, &base_url, &api_key);
        }
    });
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

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum OverlayNoticeKind { PasteRequested, Copied, Cancelled, DeliveryFailed }

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OverlayNotice {
    pub key: String,
    pub generation: u64,
    pub recording_id: String,
    pub kind: OverlayNoticeKind,
    pub expires_at: Option<i64>,
    #[serde(skip)]
    expires_monotonic: Option<Instant>,
    pub detail: Option<String>,
}

#[derive(Default)]
struct OverlayFeedback { generation: u64, sequence: u64, notice: Option<OverlayNotice> }

// All mutations are made while the lifecycle gate is held. Generation changes
// even when a retry reuses the recording ID; notice keys identify one outcome.
fn begin_overlay_attempt(state: &AppState) {
    let mut feedback = state.overlay_feedback.lock().expect("feedback lock");
    feedback.generation = feedback.generation.wrapping_add(1);
    feedback.notice = None;
}

fn publish_overlay_notice(state: &AppState, recording_id: &str, kind: OverlayNoticeKind, duration_ms: Option<i64>, detail: Option<String>) -> OverlayNotice {
    let mut feedback = state.overlay_feedback.lock().expect("feedback lock");
    feedback.sequence = feedback.sequence.wrapping_add(1);
    let notice = OverlayNotice { key: format!("{}:{}", feedback.generation, feedback.sequence), generation: feedback.generation,
        recording_id: recording_id.into(), kind, expires_at: duration_ms.map(|ms| epoch_ms() + ms),
        expires_monotonic: duration_ms.map(|ms| Instant::now() + Duration::from_millis(ms.max(0) as u64)), detail };
    feedback.notice = Some(notice.clone());
    notice
}

// Caller owns lifecycle through clear AND native hide, so a newer recording
// cannot enter between the ownership check and the visible side effect.
fn clear_overlay_notice(state: &AppState, key: &str, generation: u64, now: Option<Instant>) -> bool {
    let mut machine = match state.machine.lock() { Ok(machine) => machine, Err(_) => return false };
    let mut feedback = match state.overlay_feedback.lock() { Ok(feedback) => feedback, Err(_) => return false };
    let Some(notice) = feedback.notice.as_ref() else { return false };
    if notice.key != key || notice.generation != generation || feedback.generation != generation { return false; }
    if let Some(now) = now {
        if notice.expires_monotonic.is_none_or(|expiry| now < expiry) { return false; }
    }
    match machine.snapshot() {
        DictationState::Idle => {},
        DictationState::Cancelling { recording_id, .. } if recording_id == notice.recording_id && notice.kind == OverlayNoticeKind::Cancelled => { let _ = machine.cancel(); },
        _ => return false,
    }
    feedback.notice = None;
    true
}

/// Monotonic lifetime runner, shared with injected-clock lifecycle tests.
/// Epoch expiry is snapshot metadata only. Re-wait after any early wake rather
/// than dropping the only task capable of completing the timed presentation.
async fn run_overlay_expiry<Now, Sleep, Sleeping, Expired>(state: &AppState, notice: &OverlayNotice, mut now: Now, mut sleep: Sleep, expired: Expired)
where
    Now: FnMut() -> Instant,
    Sleep: FnMut(Duration) -> Sleeping,
    Sleeping: std::future::Future<Output = ()>,
    Expired: FnOnce(),
{
    let Some(deadline) = notice.expires_monotonic else { return };
    loop {
        let remaining = deadline.saturating_duration_since(now());
        if !remaining.is_zero() {
            sleep(remaining).await;
            continue;
        }
        let Ok(_gate) = state.lifecycle.lock() else { return };
        if clear_overlay_notice(state, &notice.key, notice.generation, Some(now())) {
            expired();
        }
        return;
    }
}

fn schedule_overlay_expiry(app: &AppHandle, state: &AppState, notice: OverlayNotice) {
    if notice.expires_monotonic.is_none() { return; }
    let app = app.clone(); let state = state.clone();
    tauri::async_runtime::spawn(async move {
        run_overlay_expiry(&state, &notice, Instant::now, tokio::time::sleep, || {
            platform::hide_overlay(&app);
            emit_state(&app, &state);
        }).await;
    });
}

#[tauri::command(rename_all = "camelCase")]
pub fn dismiss_overlay_notice(app: AppHandle, state: State<'_, AppState>, key: String, generation: u64) -> Result<AppSnapshot, String> {
    let _gate = state.lifecycle.lock().map_err(|_| "lifecycle lock poisoned")?;
    if clear_overlay_notice(&state, &key, generation, None) { platform::hide_overlay(&app); emit_state(&app, &state); }
    state.snapshot()
}

#[tauri::command(rename_all = "camelCase")]
pub fn open_overlay_details(app: AppHandle, state: State<'_, AppState>, key: String, generation: u64) -> Result<(), String> {
    let _gate = state.lifecycle.lock().map_err(|_| "lifecycle lock poisoned")?;
    let detail = state.overlay_feedback.lock().map_err(|_| "feedback lock poisoned")?.notice.as_ref()
        .filter(|notice| notice.key == key && notice.generation == generation).and_then(|notice| notice.detail.clone());
    if let Some(detail) = detail {
        platform::show_main(&app).map_err(|error| error.to_string())?;
        let _ = app.emit("dictation://paste_error", detail);
    }
    Ok(())
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AppSnapshot {
    pub dictation: DictationState,
    pub settings: AppSettings,
    /// The download in flight, so a screen shows it even when it was started
    /// somewhere else or before the screen existed.
    pub download: Option<DownloadStatus>,
    /// Name, maker and readiness of the selected model.
    ///
    /// Carried by the snapshot rather than fetched on its own because the
    /// snapshot is broadcast on every state change: a one-off query that
    /// misses — because it ran before the window was ready, say — leaves the
    /// interface claiming there is no model while one is loading.
    pub model: ModelSummary,
    pub model_loading: bool,
    /// Epoch ms when the active recording started, so the UI can keep an
    /// accurate elapsed-time counter even across page remounts.
    pub recording_started_at: Option<i64>,
    pub live_transcript: Option<LiveTranscript>,
    pub overlay_notice: Option<OverlayNotice>,
}

/// A download in progress.
///
/// `cancel` is shared with the threads doing the fetching, which check it
/// between chunks.
#[derive(Debug, Clone)]
pub struct ActiveDownload {
    pub model: String,
    pub downloaded_bytes: u64,
    pub total_bytes: u64,
    pub cancel: Arc<AtomicBool>,
}

/// The part of a download the interface needs, in every snapshot, so any
/// screen can show it however it was arrived at.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DownloadStatus {
    pub model: String,
    pub downloaded_bytes: u64,
    pub total_bytes: u64,
}

/// What the interface needs to say about the selected engine.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelSummary {
    pub key: String,
    pub display: String,
    pub provider: String,
    /// Whether the selected engine can run right now: for a local model that
    /// means the files are here, for a cloud provider that the key is set.
    pub installed: bool,
    /// What it will cost to fetch, so the interface can say so before asking.
    pub total_bytes: u64,
    /// Which engine this summary is about, so the interface can say "no key"
    /// rather than "no model" when the cloud is selected.
    pub source: TranscriptionSource,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SettingsUpdateResult {
    pub settings: AppSettings,
    pub warning: Option<String>,
}

/// What a connection check found: the provider's answer and how long it took.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CloudCheck {
    /// Empty for silence, which is still a pass.
    pub text: String,
    pub elapsed_ms: u64,
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
    /// The download in flight, if any. One model at a time, owned here rather
    /// than by whichever screen happened to start it — a screen can be left,
    /// and leaving one must not lose the transfer or let a second one begin.
    pub download: Arc<Mutex<Option<ActiveDownload>>>,
    lifecycle: Arc<Mutex<()>>,
    hold: Arc<Mutex<HoldController>>,
    suppress_shortcut_until: Arc<Mutex<Option<Instant>>>,
    overlay_feedback: Arc<Mutex<OverlayFeedback>>,
    settings_update: Arc<Mutex<()>>,
    model_home: PathBuf,
    /// Set when processing is discarded so a transcription that finishes late
    /// does not paste into the target app.
    transcription_cancel: Arc<AtomicBool>,
    /// The live transcription in flight, if the take is streaming.
    streaming: Arc<Mutex<Option<ActiveLiveSession>>>,
    active_take: Arc<Mutex<Option<FrozenTake>>>,
    live_transcript: Arc<Mutex<Option<LiveTranscript>>>,
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
        duration_ms: Option<u64>,
    ) -> Result<(), String>;
    fn write_failed(&self, recording_id: &str, error: &str) -> Result<(), String>;
}

impl CompletionWriter for Storage {
    fn write_completed(
        &self,
        recording_id: &str,
        text: &str,
        model: &str,
        duration_ms: Option<u64>,
    ) -> Result<(), String> {
        match self.update_status(
            recording_id,
            RecordingStatus::Completed,
            Some(text),
            Some(model),
            duration_ms,
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
    pub duration_ms: Option<u64>,
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
        if settings.microphones.is_none() && let Ok(devices) = audio.list_devices() {
            settings.microphones = Some(crate::microphones::migrate_legacy(settings.input_device.as_deref(), &devices));
            let _ = storage.set_setting("app", &settings);
        }
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
            download: Arc::new(Mutex::new(None)),
            lifecycle: Arc::new(Mutex::new(())),
            hold: Arc::new(Mutex::new(HoldController::default())),
            suppress_shortcut_until: Arc::new(Mutex::new(None)),
            overlay_feedback: Arc::new(Mutex::new(OverlayFeedback::default())),
            settings_update: Arc::new(Mutex::new(())),
            model_home,
            transcription_cancel: Arc::new(AtomicBool::new(false)),
            streaming: Arc::new(Mutex::new(None)),
            active_take: Arc::new(Mutex::new(None)),
            live_transcript: Arc::new(Mutex::new(None)),
        }
    }


    fn download_status(&self) -> Option<DownloadStatus> {
        self.download
            .lock()
            .ok()
            .and_then(|guard| guard.as_ref().map(|active| DownloadStatus {
                model: active.model.clone(),
                downloaded_bytes: active.downloaded_bytes,
                total_bytes: active.total_bytes,
            }))
    }

    fn model_summary(&self) -> ModelSummary {
        let settings = self
            .settings
            .read()
            .map(|settings| settings.clone())
            .unwrap_or_default();
        if settings.transcription_source == TranscriptionSource::Cloud {
            let provider = cloud::provider(&settings.cloud_provider)
                .unwrap_or_else(|| cloud::default_provider());
            let model = resolve_cloud_model(&settings);
            // Asking whether the key decrypts is the cloud's version of
            // "are the model files here": both answer whether pressing the
            // shortcut can produce words right now. A custom provider also
            // needs an address before it can go anywhere.
            let has_address = !provider.custom || !settings.cloud_base_url.trim().is_empty();
            let ready = has_address
                && cloud_api_key(&self.storage, provider.key)
                    .ok()
                    .flatten()
                    .is_some();
            return ModelSummary {
                key: String::new(),
                display: format!("{} · {}", provider.display, model),
                provider: provider.display.to_owned(),
                installed: ready,
                total_bytes: 0,
                source: TranscriptionSource::Cloud,
            };
        }
        let key = settings.model.clone();
        let spec = crate::models::spec(&key);
        ModelSummary {
            display: spec.map(|spec| spec.display.to_owned()).unwrap_or_else(|| key.clone()),
            provider: spec.map(|spec| spec.provider.to_owned()).unwrap_or_default(),
            installed: self.engine.is_installed(&key),
            total_bytes: spec.map(|spec| spec.total_bytes()).unwrap_or(0),
            key,
            source: TranscriptionSource::Local,
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
            download: self.download_status(),
            model: self.model_summary(),
            model_loading: self.model_warm.is_loading(),
            recording_started_at,
            live_transcript: self.live_transcript.lock().ok().and_then(|live| live.clone()),
            overlay_notice: self.overlay_feedback.lock().ok().and_then(|feedback| feedback.notice.clone()),
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
    let settings = state
        .settings
        .read()
        .map(|settings| settings.clone())
        .unwrap_or_default();
    // A cloud engine has nothing to load, and keeping the local model warm
    // for it would hold gigabytes for no reason.
    if settings.transcription_source == TranscriptionSource::Cloud {
        return;
    }
    let warm = state.model_warm.clone();
    let engine = state.engine.clone();
    let engine_last_used = state.engine_last_used.clone();
    let model = settings.model.clone();
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
        let _ = platform::arm_recorder_keys(
            app,
            holds_discardable_audio(&snapshot.dictation) && state.hold.lock().is_ok_and(|hold| !hold.suspended),
            awaits_cancel_answer(&snapshot.dictation),
        );
        let _ = app.emit("dictation://state", snapshot);
    }
}

/// Whether there is a recording in progress that Escape could discard.
pub fn holds_discardable_audio(state: &DictationState) -> bool {
    matches!(
        state,
        DictationState::Recording { .. }
            | DictationState::Cancelling { .. }
            | DictationState::Processing { .. }
    )
}

/// Watches the idle worker and unloads the model from GPU/RAM after the
/// configured keep-alive timeout, freeing memory for other apps. With the
/// default setting (`0` = always) it never unloads.
/// How often the watchdog lets retention have another pass, in 10s ticks.
///
/// Loquara lives in the tray for weeks, so a cleanup that only ran at startup
/// would let "keep audio for a day" mean "keep it until the next reboot".
const RETENTION_TICKS: u64 = 180;

pub fn spawn_model_keep_alive_watchdog(state: &AppState) {
    let state = state.clone();
    std::thread::spawn(move || {
        let mut ticks: u64 = 0;
        loop {
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
                if idle_elapsed >= Duration::from_secs(keep)
                    && state.engine.loaded_model().is_some()
                {
                    state.engine.unload();
                    state.model_warm.forget();
                    if let Ok(mut last) = state.engine_last_used.lock() {
                        *last = Instant::now();
                    }
                }
            }
            if ticks.is_multiple_of(RETENTION_TICKS) {
                let _ = cleanup_retention(&state);
            }
            ticks = ticks.wrapping_add(1);
            std::thread::sleep(Duration::from_secs(10));
        }
    });
}

#[tauri::command]
pub fn get_app_snapshot(state: State<'_, AppState>) -> Result<AppSnapshot, String> {
    state.snapshot()
}

pub fn get_snapshot_for_events(state: &AppState) -> Result<AppSnapshot, String> { state.snapshot() }

pub fn set_input_device(state: &AppState, device: String) {
    let Ok(_update) = state.settings_update.lock() else { return; };
    let mut settings = state
        .settings
        .write()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let mut next = settings.clone();
    let preferences = next.microphones.get_or_insert_with(Default::default);
    preferences.selection = crate::microphones::Selection::Manual;
    preferences.selected_id = (!device.is_empty()).then_some(device.clone());
    preferences.unresolved_legacy_name = None;
    preferences.hidden_ids.retain(|id| id != &device);
    next.input_device = preferences.selected_id.clone();
    if state.storage.set_setting("app", &next).is_ok() { *settings = next; }
}

pub fn input_device_choices(state: &AppState) -> Result<Vec<InputDeviceInfo>, String> {
    let devices = state.audio.list_devices().map_err(|error| error.to_string())?;
    let settings = state.settings.read().map_err(|_| "settings lock poisoned")?;
    let preferences = settings.microphones.clone().unwrap_or_else(|| crate::microphones::migrate_legacy(settings.input_device.as_deref(), &devices));
    Ok(crate::microphones::device_choices(&preferences, devices))
}

#[tauri::command]
pub fn list_input_devices(state: State<'_, AppState>) -> Result<Vec<InputDeviceInfo>, String> {
    input_device_choices(&state)
}

fn hold_may_start(state: &AppState, gesture: &Arc<HoldGesture>) -> bool {
    !gesture.released() && !gesture.cancelled()
        && state.settings.read().is_ok_and(|settings| settings.activation_mode == ActivationMode::PushToTalk)
        && state.hold.lock().is_ok_and(|hold| !hold.suspended)
        && state.machine.lock().is_ok_and(|machine| matches!(machine.snapshot(), DictationState::Idle))
}
fn owns_held_capture(state: &AppState, gesture: &Arc<HoldGesture>) -> bool {
    state.active_take.lock().is_ok_and(|take| take.as_ref().is_some_and(|take|
        take.hold.as_ref().is_some_and(|owner| Arc::ptr_eq(owner, gesture))))
}

fn consume_hold_sample(state: &AppState, gesture: &Arc<HoldGesture>, sample: Result<platform::ShortcutKeys, platform::PlatformError>, cancellation_sent: &mut bool, cancel: impl FnOnce()) -> bool {
    let done = match sample {
        Ok(keys) => {
            if let Ok(mut holds) = state.hold.lock() { holds.sample(gesture, keys.chord_down, keys.main_down, keys.escape_down); }
            !keys.main_down
        }
        Err(_) => {
            // An inaccessible desktop cannot prove a main-key release. Cancel
            // and retire this identity rather than keeping an immortal latch.
            gesture.cancel();
            if let Ok(mut holds) = state.hold.lock() { holds.abandon(gesture); }
            true
        }
    };
    // Release may have already sent this take to Processing. Revocation must
    // still perform owner-checked cleanup independently of the start worker.
    if gesture.cancelled() && !*cancellation_sent {
        *cancellation_sent = true;
        cancel();
    }
    done
}

/// Runs inside the plugin callback: only try-locks, never registry changes or
/// lifecycle waits. Reservation precedes spawning, so repeats cannot race start.
pub fn dispatch_activation_press(app: AppHandle, state: AppState) {
    let Ok(update) = state.settings_update.try_lock() else { return };
    let Ok(suppressed) = state.suppress_shortcut_until.try_lock() else { return };
    if suppressed.is_some_and(|until| Instant::now() < until) { return; }
    drop(suppressed);
    let Ok(settings) = state.settings.try_read() else { return };
    let mode = settings.activation_mode;
    let Ok(shortcut) = platform::Shortcut::parse(&settings.shortcut) else { return };
    drop(settings);
    let Ok(mut holds) = state.hold.try_lock() else { return };
    if holds.suspended { return; }
    if mode == ActivationMode::Toggle {
        if holds.current().is_some() { return; }
        drop(holds);
        drop(update);
        tauri::async_runtime::spawn(async move { let _ = toggle_recording(app, state).await; });
        return;
    }
    let Ok(machine) = state.machine.try_lock() else { return };
    let idle = matches!(machine.snapshot(), DictationState::Idle);
    drop(machine);
    let Some(gesture) = holds.press(idle) else { return };
    drop(holds);
    drop(update);
    let monitor_state = state.clone();
    let monitor_gesture = gesture.clone();
    let monitor_app = app.clone();
    std::thread::spawn(move || {
        let mut cancellation_sent = false;
        loop {
            if consume_hold_sample(&monitor_state, &monitor_gesture, shortcut.key_state(), &mut cancellation_sent,
                || cancel_owned_hold(&monitor_app, &monitor_state, &monitor_gesture)) { break; }
            // Keep the main-key latch after an ordinary modifier release. A
            // held regular key must not autorepeat into a fresh recording.
            std::thread::sleep(Duration::from_millis(15));
        }
    });
    tauri::async_runtime::spawn(async move {
        let start_app = app.clone(); let start_state = state.clone(); let start_hold = gesture.clone();
        let _ = tauri::async_runtime::spawn_blocking(move || start_recording_with_hold(&start_app, &start_state, Some(start_hold))).await;
        while !gesture.released() { tokio::time::sleep(Duration::from_millis(10)).await; }
        if gesture.cancelled() {
            cancel_owned_hold(&app, &state, &gesture);
        } else {
            let _ = stop_recording_owned(app, state, Some(gesture)).await;
        }
    });
}

/// Escape from the registry callback binds to the observed hold, including
/// pending startup. It can never cancel a later UI-origin recording.
pub fn dispatch_cancel_press(app: AppHandle, state: AppState, key: RecorderKey) {
    let Ok(holds) = state.hold.try_lock() else { return };
    let pending = holds.current();
    drop(holds);
    let Ok(active) = state.active_take.try_lock() else { return };
    let active_hold = active.as_ref().and_then(|take| take.hold.clone());
    drop(active);
    // Capture notice identity before machine state, so a later same-ID Undo
    // cannot be paired with this Escape observation.
    let Ok(feedback) = state.overlay_feedback.try_lock() else { return };
    let observed_notice = feedback.notice.clone();
    drop(feedback);
    let Ok(machine) = state.machine.try_lock() else { return };
    let observed = machine.snapshot();
    drop(machine);
    if key == RecorderKey::Escape && let DictationState::Cancelling { recording_id, .. } = &observed {
        let Some(notice) = observed_notice else { return };
        let recording_id = recording_id.clone();
        tauri::async_runtime::spawn_blocking(move || {
            let Ok(_gate) = state.lifecycle.lock() else { return };
            if dismiss_cancel_undo_if_owned(&state, &recording_id, &notice) {
                hide_overlay_quietly(&app, &state);
                emit_state(&app, &state);
            }
        });
        return;
    }
    let gesture = if matches!(observed, DictationState::Idle) { pending } else { active_hold };
    if key == RecorderKey::Escape && let Some(gesture) = gesture {
        gesture.cancel();
        tauri::async_runtime::spawn_blocking(move || cancel_owned_hold(&app, &state, &gesture));
    } else {
        tauri::async_runtime::spawn(async move { handle_cancel_press(&app, &state, key); });
    }
}

/// Revocation is immediate; capture cleanup revalidates its owner under the
/// lifecycle lock even if startup is still opening the microphone.
fn invalidate_hold(app: &AppHandle, state: &AppState) {
    if let Ok(mut holds) = state.hold.lock() { holds.invalidate(); }
    let gesture = state.active_take.lock().ok().and_then(|take| take.as_ref().and_then(|take| take.hold.clone()));
    if let Some(gesture) = gesture {
        gesture.cancel();
        let app = app.clone(); let state = state.clone();
        tauri::async_runtime::spawn_blocking(move || cancel_owned_hold(&app, &state, &gesture));
    }
}

fn with_owned_capture<T>(state: &AppState, hold: Option<&Arc<HoldGesture>>, effect: impl FnOnce() -> Result<T, String>) -> Result<T, String> {
    let _gate = state.lifecycle.lock().map_err(|_| "lifecycle lock poisoned")?;
    if hold.is_some_and(|gesture| !owns_held_capture(state, gesture)) { return Err("hold no longer owns capture".into()); }
    effect()
}

fn consume_owned_hold_cancel<T>(state: &AppState, gesture: &Arc<HoldGesture>, cancel: impl FnOnce() -> Result<T, String>) -> Result<Option<T>, String> {
    with_owned_capture(state, Some(gesture), || {
        // Monitor and start/release workers can both deliver cleanup. Only
        // active capture/processing is automatic; Undo dismissal is explicit.
        if !matches!(state.machine.lock().map_err(|_| "coordinator lock poisoned")?.snapshot(),
            DictationState::Recording { .. } | DictationState::Processing { .. }) { return Ok(None); }
        cancel().map(Some)
    })
}

fn cancel_owned_hold(app: &AppHandle, state: &AppState, gesture: &Arc<HoldGesture>) {
    let _ = consume_owned_hold_cancel(state, gesture, || cancel_recording_locked(app, state));
}

pub fn start_recording_inner(app: &AppHandle, state: &AppState) -> Result<AppSnapshot, String> {
    start_recording_with_hold(app, state, None)
}

fn start_recording_with_hold(app: &AppHandle, state: &AppState, hold: Option<Arc<HoldGesture>>) -> Result<AppSnapshot, String> {
    with_capture_start(state, hold.as_ref(), || start_recording_locked(app, state, hold.clone()))
}

fn with_capture_start(state: &AppState, hold: Option<&Arc<HoldGesture>>, start: impl FnOnce() -> Result<AppSnapshot, String>) -> Result<AppSnapshot, String> {
    let _lifecycle = lifecycle_guard(&state.lifecycle).map_err(|error| error.to_string())?;
    if hold.is_some_and(|gesture| !hold_may_start(state, gesture)) { return state.snapshot(); }
    start()
}

fn start_recording_locked(app: &AppHandle, state: &AppState, hold: Option<Arc<HoldGesture>>) -> Result<AppSnapshot, String> {
    if !matches!(state.snapshot()?.dictation, DictationState::Idle) {
        return Err(CoordinatorError::InvalidState.to_string());
    }
    let settings = state
        .settings
        .read()
        .map_err(|_| "settings lock poisoned")?
        .clone();
    if settings.transcription_source == TranscriptionSource::Local
        && !state.engine.is_installed(&settings.model)
    {
        return Err("Najpierw pobierz wybrany model.".into());
    }
    // Warm the model — or the provider's connection — while the user talks,
    // so transcription does not pay the setup cost the moment they stop.
    warm_up_model(app, state);
    if settings.transcription_source == TranscriptionSource::Cloud {
        warm_cloud_connection(state, &settings);
    }
    let mut windows = SystemWindows;
    let target = windows.foreground_window();
    let devices = state.audio.list_devices().map_err(|error| error.to_string())?;
    let preferences = settings.microphones.clone().unwrap_or_else(|| crate::microphones::migrate_legacy(settings.input_device.as_deref(), &devices));
    let device = crate::microphones::resolve_preference(&preferences, &devices).map_err(|error| error.to_string())?.id.clone();
    // Opened before the capture starts, so the first packet already has
    // somewhere to go.
    begin_overlay_attempt(state);
    platform::hide_overlay(app);
    let mut take = FrozenTake::capture(state, settings.clone(), target, String::new());
    take.hold = hold;
    *state.active_take.lock().map_err(|_| "take lock poisoned")? = Some(take.clone());
    *state.live_transcript.lock().map_err(|_| "draft lock poisoned")? = None;
    let live = start_live_session(app, state, &take);
    let started = match state.audio.start(Some(&device)) {
        Ok(started) => started,
        Err(error) => {
            state.audio.set_stream_sink(None);
            take.cancel.store(true, Ordering::SeqCst);
            take.preview_stop.store(true, Ordering::SeqCst);
            if let Some(live) = live && let Some(cancellation) = live.cancellation { cancellation.cancel(); }
            return Err(error.to_string());
        }
    };
    take.recording_id = started.id.clone();
    *state.active_take.lock().map_err(|_| "take lock poisoned")? = Some(take.clone());
    if let Some(mut live) = live && let Ok(mut slot) = state.streaming.lock() {
        live.recording_id = started.id.clone();
        *slot = Some(live);
        *state.live_transcript.lock().map_err(|_| "draft lock poisoned")? = Some(LiveTranscript {
            recording_id: started.id.clone(), text: String::new(), revision: 0,
            status: if settings.transcription_source == TranscriptionSource::Cloud { "connecting" } else { "listening" }.into(), clipboard_ready: false,
        });
    }
    state.transcription_cancel.store(false, Ordering::SeqCst);
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
        abandon_live_session(state);
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
        // Shown without activation so the app the user is dictating into
        // keeps the keyboard. Esc and the configured shortcut are global.
        platform::show_overlay_without_focus(app, overlay_position(&state))
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
pub fn hide_overlay(app: AppHandle, state: State<'_, AppState>) {
    let Ok(_gate) = state.lifecycle.lock() else { return };
    if let Ok(snapshot) = state.snapshot() {
        if matches!(snapshot.dictation, DictationState::Idle) && snapshot.overlay_notice.is_none() { platform::hide_overlay(&app); }
    }
}

#[tauri::command]
pub fn set_shortcut_suspended(
    app: AppHandle,
    state: State<'_, AppState>,
    suspended: bool,
) -> Result<(), String> {
    if suspended {
        state.hold.lock().map_err(|_| "hold lock poisoned")?.suspended = true;
        invalidate_hold(&app, &state);
        platform::suspend_shortcuts(&app).map_err(|error| error.to_string())
    } else {
        let shortcut = state
            .settings
            .read()
            .map(|settings| settings.shortcut.clone())
            .unwrap_or_else(|_| default_shortcut().to_string());
        platform::register_shortcuts(&app, &shortcut).map_err(|error| error.to_string())?;
        state.hold.lock().map_err(|_| "hold lock poisoned")?.suspended = false;
        Ok(())
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
    stop_recording_owned(app, state, None).await
}

/// Runs under the same gate as start/stop. A delayed fault can only close its
/// own active capture and never schedules transcription or delivery.
pub fn interrupt_microphone_capture(state: &AppState, fault: &crate::audio::CaptureFault) -> Result<bool, String> {
    let _gate = state.lifecycle.lock().map_err(|_| "lifecycle lock poisoned")?;
    if !state.audio.owns_capture(&fault.recording_id) { return Ok(false); }
    if !matches!(state.snapshot()?.dictation, DictationState::Recording { ref recording_id, .. } | DictationState::Cancelling { ref recording_id, .. } if recording_id == &fault.recording_id) {
        return Ok(false);
    }
    // The sink retains the fault before queuing it, so committed stop takes
    // the interrupted branch even if the user pressed Stop first.
    let result = stop_capture_or_abandon(state);
    abandon_live_session(state);
    match result {
        Err(_) => Ok(true),
        Ok(_) => Err("capture fault did not retain interruption metadata".into()),
    }
}

pub fn spawn_microphone_fault_worker(app: &AppHandle, state: &AppState) {
    let (sender, receiver) = std::sync::mpsc::channel();
    state.audio.set_fault_sender(sender);
    let app = app.clone();
    let state = state.clone();
    std::thread::spawn(move || {
        while let Ok(fault) = receiver.recv() {
            if interrupt_microphone_capture(&state, &fault).unwrap_or(false) {
                emit_state(&app, &state);
            }
        }
    });
}

async fn stop_recording_owned(app: AppHandle, state: AppState, hold: Option<Arc<HoldGesture>>) -> Result<AppSnapshot, String> {
    let blocking_state = state.clone();
    let completed_result = tauri::async_runtime::spawn_blocking(move || with_owned_capture(&blocking_state, hold.as_ref(), || {
        if hold.as_ref().is_some_and(|gesture| gesture.cancelled()) {
            return Err("hold cancelled".into());
        }
        let completed = stop_capture_or_abandon(&blocking_state)?;
        // Detach this take while cancellation/new starts are still excluded.
        // Delaying this until after releasing the lock could steal a new take.
        blocking_state.audio.set_stream_sink(None);
        let take = blocking_state.active_take.lock().map_err(|_| "take lock poisoned")?
            .as_ref().filter(|take| take.recording_id == completed.id).cloned().ok_or("recording context missing")?;
        take.preview_stop.store(true, Ordering::SeqCst);
        let live = blocking_state.streaming.lock().ok().and_then(|mut slot| {
            slot.as_mut().filter(|live| live.recording_id == completed.id).and_then(|live| {
                live.flush();
                live.session.take()
            })
        });
        if let Ok(mut draft) = blocking_state.live_transcript.lock() && let Some(draft) = draft.as_mut() { draft.status = "finalizing".into(); }
        Ok::<_, String>((completed, live, take))
    }))
    .await
    .map_err(|error| error.to_string())?;
    let (completed, live, take) = match completed_result {
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
        match live {
            Some(session) => {
                finish_live_take(background_app, background_state, completed, session, take).await
            }
            None => {
                transcribe_recording(
                    background_app,
                    background_state,
                    completed.id,
                    completed.path,
                    take,
                )
                .await
            }
        }
    });
    Ok(snapshot)
}

/// A live result is authoritative only when the provider explicitly finished
/// and every audio packet arrived. Empty final is valid silence, not fallback.
async fn finish_live_take(app: AppHandle, state: AppState, completed: CompletedRecording, session: streaming::Session, take: FrozenTake) {
    let outcome = session.finish(Duration::from_secs(8)).await;
    if !take.owns(&state) { return; }
    if let Some(text) = outcome.authoritative_final() {
        finish_success(&app, &state, &completed.id, &completed.path, TranscriptionResult {
            text: text.to_owned(), model: cloud_model_label(&take.settings), language: None, duration_ms: 0,
        }, &take);
    } else {
        if let Ok(_gate) = state.lifecycle.lock() {
            if !take.owns(&state) { return; }
            if let Ok(mut draft) = state.live_transcript.lock() && let Some(draft) = draft.as_mut() { draft.status = "fallback".into(); }
            emit_state(&app, &state);
        }
        transcribe_recording(app, state, completed.id, completed.path, take).await;
    }
}

fn stop_capture_or_abandon(state: &AppState) -> Result<CompletedRecording, String> {
    // The caller holds the lifecycle gate. A duplicate/stale stop is not an
    // audio failure and must not revoke the already-processing take.
    if !matches!(state.snapshot()?.dictation, DictationState::Recording { .. } | DictationState::Cancelling { .. }) {
        return Err(CoordinatorError::InvalidState.to_string());
    }
    match stop_recording_committed(state) {
        Ok(completed) => Ok(completed),
        Err(error) => { abandon_live_session(state); Err(error) }
    }
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
            if !Path::new(&audio_path_string).is_file() { let _ = state.storage.clear_audio_path(&recording_id); }
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
    if let Some(message) = &completed.interrupted {
        let message = format!("Microphone interrupted: {message}. Retained audio is available in History; retry manually.");
        let _ = state.storage.update_status(&completed.id, RecordingStatus::Failed, None, None, Some(completed.duration_ms), Some(&message));
        let _ = recover_terminal_state(&state.machine, state.storage.as_ref(), &recording_id, &audio_path_string, TerminalOutcome::Failed(message.clone()));
        return Err(message);
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
    take: FrozenTake,
) {
    if !take.owns(&state) { return; }
    let settings = take.settings.clone();
    let vocabulary = take.vocabulary.clone();
    let started = Instant::now();
    let cancel = take.cancel.clone();
    let outcome = match settings.transcription_source {
        TranscriptionSource::Cloud => {
            let storage = state.storage.clone();
            let audio = audio_path.clone();
            tauri::async_runtime::spawn_blocking(move || {
                transcribe_in_cloud(&storage, &settings, &vocabulary, &audio, &cancel, started)
            })
            .await
        }
        TranscriptionSource::Local => {
            let engine = state.engine.clone();
            let engine_last_used = state.engine_last_used.clone();
            let audio_for_engine = audio_path.clone();
            let warm = state.model_warm.clone();
            let model = settings.model.clone();
            tauri::async_runtime::spawn_blocking(move || {
                // A warm-up started when recording began may still be
                // finishing; the engine would serialise on its own lock
                // anyway, but waiting here keeps the reported state honest.
                let _ = warm.wait_until_ready_for(&model, Duration::from_secs(240));
                let text = engine.transcribe(&model, &audio_for_engine, Some(&cancel))?;
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
            .await
            .map(|outcome| outcome.map_err(|error| error.to_string()))
        }
    };
    match outcome {
        Ok(Ok(result)) => finish_success(&app, &state, &recording_id, &audio_path, result, &take),
        Ok(Err(error)) => finish_failure(&app, &state, &recording_id, error, &take),
        Err(error) => finish_failure(&app, &state, &recording_id, error.to_string(), &take),
    }
}

/// One recording transcribed by the configured provider.
fn transcribe_in_cloud(
    storage: &Storage,
    settings: &AppSettings,
    vocabulary: &[VocabularyEntry],
    audio_path: &Path,
    cancel: &AtomicBool,
    started: Instant,
) -> Result<TranscriptionResult, String> {
    let provider = cloud::provider(&settings.cloud_provider)
        .unwrap_or_else(|| cloud::default_provider());
    let api_key = cloud_api_key(storage, provider.key)?.ok_or_else(|| {
        format!(
            "no API key is set for {}; add one in Settings",
            provider.display
        )
    })?;
    let model = resolve_cloud_model(settings);
    // The capture is routinely 48 kHz stereo; every recogniser here consumes
    // 16 kHz mono, so converting first makes the upload several times
    // smaller. The original file stays on disk untouched.
    let (audio, file_name) = match crate::engine::compact_wav(audio_path) {
        Ok(compact) => (compact, "loquara-16k-mono.wav".to_owned()),
        Err(_) => {
            let raw = std::fs::read(audio_path)
                .map_err(|error| format!("could not read the recording: {error}"))?;
            let name = audio_path
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or("recording.wav")
                .to_owned();
            (raw, name)
        }
    };
    let keyterms = keyterms_for(vocabulary);
    let transcript = cloud::transcribe(cloud::Request {
        provider: provider.key,
        model: &model,
        base_url: &settings.cloud_base_url,
        api_key: &api_key,
        language: &settings.cloud_language,
        keyterms: &keyterms,
        audio: &audio,
        file_name: &file_name,
        cancel: Some(cancel),
    })
    .map_err(|error| error.to_string())?;
    Ok(TranscriptionResult {
        text: transcript.text,
        model: format!("{} · {}", provider.display, model),
        language: transcript.language,
        duration_ms: started.elapsed().as_millis().try_into().unwrap_or(u64::MAX),
    })
}

/// Real completion consumer used by the desktop and lifecycle tests. Ownership
/// is checked under the same gate as cancellation, including history and input.
fn complete_take(state: &AppState, take: &FrozenTake, audio_path: &Path, result: &TranscriptionResult, windows: &mut impl WindowsApi) -> Result<Option<Result<(), platform::PlatformError>>, String> {
    if !take.owns(state) { return Ok(None); }
    let shortcut = take.hold.as_ref().map(|_| platform::Shortcut::parse(&take.settings.shortcut)).transpose().map_err(|error| error.to_string())?;
    let release = if take.settings.auto_paste && !result.text.trim().is_empty() {
        platform::wait_for_modifiers(windows, shortcut.as_ref(), || !take.owns(state), || take.hold.as_ref().is_none_or(|hold| hold.main_released()), Duration::from_millis(1500))
    } else { Ok(()) };
    let _lifecycle = state.lifecycle.lock().map_err(|_| "lifecycle lock poisoned")?;
    if !take.owns(state) { return Ok(None); }
    if !matches!(state.machine.lock().map_err(|_| "coordinator lock poisoned")?.snapshot(),
        DictationState::Processing { recording_id: active, .. } if active == take.recording_id) { return Ok(None); }
    // Silence is authoritative before mode decorations. A custom prefix or
    // suffix must never invent a deliverable transcript from an empty final.
    let transcript = if result.text.trim().is_empty() { String::new() }
        else { postprocess_with_mode(&result.text, &take.vocabulary, take.mode.as_ref()) };
    let mut delivery = Ok(());
    complete_transcription_durably(&state.machine, state.storage.as_ref(), DurableTranscript {
        recording_id: &take.recording_id, audio_path: &audio_path.to_string_lossy(), transcript: &transcript,
        model: &result.model, duration_ms: None,
    }, || {
        delivery = if take.settings.auto_paste && !transcript.trim().is_empty() {
            match &release {
                Ok(()) => {
                    if let Ok(mut until) = state.suppress_shortcut_until.lock() { *until = Some(Instant::now() + Duration::from_secs(2)); }
                    let result = platform::copy_and_paste_cancellable(windows, take.target, &transcript, take.settings.paste_mode, shortcut.as_ref(), || !take.owns(state));
                    if let Ok(mut until) = state.suppress_shortcut_until.lock() { *until = Some(Instant::now() + Duration::from_millis(250)); }
                    result
                }
                Err(error) => windows.write_clipboard(&transcript).and(Err(error.clone())),
            }
        } else { windows.write_clipboard(&transcript) };
    })?;
    if let Ok(mut live) = state.live_transcript.lock() { *live = None; }
    if let Ok(mut live) = state.streaming.lock() && live.as_ref().is_some_and(|live| live.recording_id == take.recording_id) { live.take(); }
    let (kind, duration, detail) = match &delivery {
        Err(error) => (OverlayNoticeKind::DeliveryFailed, None, Some(error.to_string())),
        Ok(()) if take.settings.auto_paste && !transcript.trim().is_empty() => (OverlayNoticeKind::PasteRequested, Some(1600), None),
        Ok(()) => (OverlayNoticeKind::Copied, Some(1600), None),
    };
    publish_overlay_notice(state, &take.recording_id, kind, duration, detail);
    Ok(Some(delivery))
}

/// A cancelled delivery can still have committed a durable transcript and
/// returned the machine to Idle. Publish that terminal result while this is
/// still the current attempt; cancellation alone must not suppress the UI.
fn with_completion_feedback(state: &AppState, take: &FrozenTake, publish: impl FnOnce()) -> bool {
    let Ok(_gate) = state.lifecycle.lock() else { return false };
    if !take.is_current(state) { return false; }
    publish();
    true
}

fn finish_success(app: &AppHandle, state: &AppState, _recording_id: &str, audio_path: &Path, result: TranscriptionResult, take: &FrozenTake) {
    match complete_take(state, take, audio_path, &result, &mut SystemWindows) {
        Ok(Some(delivery)) => {
            with_completion_feedback(state, take, || {
            if let Err(error) = delivery { let _ = app.emit("dictation://paste_error", error); }
            crate::sound::play_transcription_ready();
            if let Some(notice) = state.snapshot().ok().and_then(|snapshot| snapshot.overlay_notice) {
                schedule_overlay_expiry(app, state, notice);
            }
            emit_state(app, state);
            });
        }
        Ok(None) => {}
        Err(error) => { let _ = app.emit("dictation://persistence_error", error); emit_state(app, state); }
    }
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

fn finish_failure(app: &AppHandle, state: &AppState, recording_id: &str, error: String, take: &FrozenTake) {
    let Ok(_lifecycle) = state.lifecycle.lock() else { return };
    if !take.owns(state) {
        return;
    }
    {
        let Ok(machine) = state.machine.lock() else { return };
        match machine.snapshot() {
            DictationState::Processing { recording_id: active, .. } if active == recording_id => {}
            _ => return,
        }
    }
    if let Ok(mut live) = state.live_transcript.lock() { *live = None; }
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
    if let Ok(mut holds) = state.hold.lock() { holds.invalidate(); }
    let _lifecycle = state.lifecycle.lock().map_err(|_| "lifecycle lock poisoned")?;
    cancel_recording_locked(app, state)
}

fn cancel_recording_locked(app: &AppHandle, state: &AppState) -> Result<AppSnapshot, String> {
    match state.snapshot()?.dictation {
        DictationState::Recording {
            recording_id,
            audio_path,
        } => cancel_live_recording(app, state, recording_id, audio_path),
        DictationState::Processing {
            recording_id,
            audio_path,
        } => cancel_processing(app, state, recording_id, audio_path),
        DictationState::Cancelling { .. } => dismiss_undo(app, state),
        _ => Err(CoordinatorError::InvalidState.to_string()),
    }
}

/// Takes shorter than this are discarded immediately. Longer ones keep a 5s
/// undo chip on the overlay — the take is already cancelled, with audio in
/// history, and Undo transcribes it.
pub const CANCEL_UNDO_AFTER_MS: u64 = 8_000;

pub fn offers_cancel_undo(duration_ms: u64) -> bool {
    duration_ms >= CANCEL_UNDO_AFTER_MS
}

/// Short takes are discarded. Longer ones keep the finalized WAV so Undo and
/// History retry have a file — `audio.cancel()` used to delete `.part` while
/// the row still pointed at a `.wav` that was never written.
pub fn discard_cancelled_take_if_short(
    storage: &Storage,
    recording_id: &str,
    audio_path: &Path,
    duration_ms: u64,
) {
    if offers_cancel_undo(duration_ms) {
        return;
    }
    let _ = std::fs::remove_file(audio_path);
    let _ = storage.clear_audio_path(recording_id);
}

/// Publish only after native capture/processing cancellation has completed.
/// The same consumer is used by short, long and processing cancellation.
fn complete_cancel_feedback(state: &AppState, recording_id: &str, undo: bool) -> Result<OverlayNotice, String> {
    let mut machine = state.machine.lock().map_err(|_| "coordinator lock poisoned")?;
    let eligible = match machine.snapshot() {
        DictationState::Recording { recording_id: current, .. } => current == recording_id,
        DictationState::Processing { recording_id: current, .. } => !undo && current == recording_id,
        _ => false,
    };
    if !eligible { return Err(CoordinatorError::InvalidState.to_string()); }
    if undo { machine.request_cancel() } else { machine.cancel() }.map_err(|error| error.to_string())?;
    drop(machine);
    Ok(publish_overlay_notice(state, recording_id, OverlayNoticeKind::Cancelled, Some(if undo { 5000 } else { 900 }), None))
}

fn cancel_live_recording(
    app: &AppHandle,
    state: &AppState,
    recording_id: String,
    audio_path: String,
) -> Result<AppSnapshot, String> {
    // Whatever was typed stays typed — there is no taking text back out of an
    // arbitrary application — but nothing more will be sent.
    abandon_live_session(state);
    let cancelled = match state.audio.stop() {
        Ok(cancelled) => cancelled,
        Err(error) => {
            let message = error.to_string();
            let _ = cleanup_partial(Path::new(&audio_path));
            let _ = state.storage.clear_audio_path(&recording_id);
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
    if !cancelled.peaks.is_empty() {
        let _ = state
            .storage
            .store_peaks(&cancelled.id, &quantize_peaks(&cancelled.peaks));
    }
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
    discard_cancelled_take_if_short(
        state.storage.as_ref(),
        &cancelled.id,
        &cancelled.path,
        cancelled.duration_ms,
    );
    let notice = complete_cancel_feedback(state, &recording_id, offers_cancel_undo(cancelled.duration_ms))?;
    schedule_overlay_expiry(app, state, notice);
    emit_state(app, state);
    state.snapshot()
}

fn cancel_processing(
    app: &AppHandle,
    state: &AppState,
    recording_id: String,
    _audio_path: String,
) -> Result<AppSnapshot, String> {
    let notice = cancel_processing_state(state, &recording_id)?;
    schedule_overlay_expiry(app, state, notice);
    emit_state(app, state);
    state.snapshot()
}

/// Caller holds lifecycle; desktop feedback is deliberately separate from the
/// durable cancellation so unavailable-desktop cleanup does not depend on UI.
fn cancel_processing_state(state: &AppState, recording_id: &str) -> Result<OverlayNotice, String> {
    state.transcription_cancel.store(true, Ordering::SeqCst);
    abandon_live_session(state);
    let _ = state.storage.update_status(recording_id, RecordingStatus::Cancelled, None, None, None, None);
    complete_cancel_feedback(state, recording_id, false)
}

fn dismiss_undo(app: &AppHandle, state: &AppState) -> Result<AppSnapshot, String> {
    state.overlay_feedback.lock().map_err(|_| "feedback lock poisoned")?.notice = None;
    state
        .machine
        .lock()
        .map_err(|_| "coordinator lock poisoned")?
        .cancel()
        .map_err(|error| error.to_string())?;
    hide_overlay_quietly(app, state);
    emit_state(app, state);
    state.snapshot()
}

fn hide_overlay_quietly(app: &AppHandle, state: &AppState) {
    platform::hide_overlay(app);
    platform::restore_foreground_to(
        app,
        state.target_window.lock().ok().and_then(|target| *target),
    );
}

#[tauri::command]
pub fn request_cancel(app: AppHandle, state: State<'_, AppState>) -> Result<AppSnapshot, String> {
    request_cancel_inner(&app, &state)
}

/// What a press of the cancel key means, given what dictation is doing.
///
/// The overlay is shown without focus, so keyboard handling in its document
/// cannot be relied on. The global shortcut is the only path that fires
/// whatever has focus, which makes it the only place this decision can live.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CancelPress {
    /// Nothing has been asked yet: put the question on screen.
    Ask,
    /// Take the question back and carry on recording.
    Dismiss,
    /// Discard the recording (or dismiss the undo chip).
    Confirm,
    /// There is nothing to cancel.
    Ignore,
}

/// The two keys the recorder answers to while it is recording.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RecorderKey {
    Escape,
    Enter,
}

/// Escape discards immediately. Enter is no longer a recorder key: a second
/// press must not throw a take away, and stealing Enter from the target app
/// is worse than a confirm shortcut.
pub fn cancel_press_for(state: &DictationState, key: RecorderKey) -> CancelPress {
    match (state, key) {
        (
            DictationState::Recording { .. }
            | DictationState::Processing { .. }
            | DictationState::Cancelling { .. },
            RecorderKey::Escape,
        ) => CancelPress::Confirm,
        _ => CancelPress::Ignore,
    }
}

/// Applies a press of one of those keys to whatever dictation is doing.
pub fn handle_cancel_press(app: &AppHandle, state: &AppState, key: RecorderKey) {
    let Ok(snapshot) = state.snapshot() else { return };
    match cancel_press_for(&snapshot.dictation, key) {
        CancelPress::Ask | CancelPress::Dismiss => {
            let _ = request_cancel_inner(app, state);
        }
        CancelPress::Confirm => {
            let _ = cancel_recording_inner(app, state);
        }
        CancelPress::Ignore => {}
    }
}

/// Whether Enter should be claimed. It is not: cancel is undo-after, not a
/// two-key confirm, so Enter stays with the app the user is dictating into.
pub fn awaits_cancel_answer(state: &DictationState) -> bool {
    let _ = state;
    false
}

// Caller holds lifecycle. A delayed undo dismissal may only consume the
// cancellation notice it observed, even after a retry reuses the recording ID.
fn dismiss_cancel_undo_if_owned(state: &AppState, recording_id: &str, notice: &OverlayNotice) -> bool {
    if notice.kind != OverlayNoticeKind::Cancelled || notice.recording_id != recording_id { return false; }
    let current = match state.machine.lock() { Ok(machine) => machine.snapshot(), Err(_) => return false };
    if !matches!(current, DictationState::Cancelling { recording_id: current, .. } if current == recording_id) { return false; }
    clear_overlay_notice(state, &notice.key, notice.generation, None)
}

pub fn request_cancel_inner(app: &AppHandle, state: &AppState) -> Result<AppSnapshot, String> {
    // Capture the owner before reading machine state: a newer notice must not
    // be paired with an earlier Cancelling observation from a split snapshot.
    let observed_notice = state.overlay_feedback.lock().map_err(|_| "feedback lock poisoned")?.notice.clone();
    let snapshot = state.snapshot()?;
    match snapshot.dictation {
        DictationState::Cancelling { recording_id, .. } => {
            let Some(notice) = observed_notice else { return state.snapshot() };
            let _lifecycle = lifecycle_guard(&state.lifecycle).map_err(|error| error.to_string())?;
            if dismiss_cancel_undo_if_owned(state, &recording_id, &notice) {
                hide_overlay_quietly(app, state);
                emit_state(app, state);
            }
            state.snapshot()
        }
        DictationState::Recording { .. } => cancel_recording_inner(app, state),
        _ => state.snapshot(),
    }
}

#[tauri::command(rename_all = "camelCase")]
pub fn retry_transcription(
    recording_id: String,
    app: AppHandle,
    state: State<'_, AppState>,
) -> Result<AppSnapshot, String> {
    let (audio_path, take) = {
        let _lifecycle = lifecycle_guard(&state.lifecycle).map_err(|error| error.to_string())?;
        let mut machine = state
            .machine
            .lock()
            .map_err(|_| "coordinator lock poisoned")?;
        if !matches!(
            machine.snapshot(),
            DictationState::Idle
                | DictationState::Failed { .. }
                | DictationState::Cancelling { .. }
        ) {
            return Err(CoordinatorError::InvalidState.to_string());
        }
        let recording = state
            .storage
            .get_recording(&recording_id)
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "recording not found".to_owned())?;
        let audio_path = retry_audio_path(&recording).map_err(|error| error.to_string())?;
        let settings = state
            .settings
            .read()
            .map_err(|_| "settings lock poisoned")?
            .clone();
        match settings.transcription_source {
            TranscriptionSource::Local => {
                if !state.engine.is_installed(&settings.model) {
                    return Err(format!(
                        "The selected model is not ready. Download it before retrying: {}.",
                        settings.model
                    ));
                }
            }
            TranscriptionSource::Cloud => {
                let provider = cloud::provider(&settings.cloud_provider)
                    .unwrap_or_else(|| cloud::default_provider());
                if cloud_api_key(&state.storage, provider.key)?.is_none() {
                    return Err(format!(
                        "No API key is set for {}. Add one in Settings before retrying.",
                        provider.display
                    ));
                }
            }
        }
        match state.storage.mark_retrying(&recording_id) {
            Ok(true) => {}
            Ok(false) => return Err("recording disappeared before retry".into()),
            Err(error) => return Err(error.to_string()),
        }
        machine
            .retry_recording(&recording_id, audio_path.to_string_lossy().into_owned())
            .map_err(|error| error.to_string())?;
        if let Ok(previous) = state.active_take.lock() && let Some(previous) = previous.as_ref() {
            previous.cancel.store(true, Ordering::SeqCst); previous.preview_stop.store(true, Ordering::SeqCst);
        }
        let target = state.target_window.lock().ok().and_then(|target| *target);
        begin_overlay_attempt(&state);
        let take = FrozenTake::capture(&state, settings, target, recording_id.clone());
        *state.active_take.lock().map_err(|_| "take lock poisoned")? = Some(take.clone());
        *state.live_transcript.lock().map_err(|_| "draft lock poisoned")? = None;
        (audio_path, take)
    };
    emit_state(&app, &state);
    let background_state = state.inner().clone();
    let background_app = app.clone();
    tauri::async_runtime::spawn(async move {
        transcribe_recording(background_app, background_state, recording_id, audio_path, take).await;
    });
    state.snapshot()
}

/// Whether a history row is still waiting for a transcript.
///
/// Failed and cancelled takes were never transcribed. A completed take whose
/// transcript came back empty was transcribed into nothing, which reads as a
/// missing transcript rather than a finished one. Anything else already has
/// text worth keeping, so history does not offer to overwrite it.
pub fn needs_transcript(recording: &Recording) -> bool {
    match recording.status {
        RecordingStatus::Failed | RecordingStatus::Cancelled => true,
        RecordingStatus::Completed => recording
            .text
            .as_deref()
            .is_none_or(|text| text.trim().is_empty()),
        RecordingStatus::Recording | RecordingStatus::Processing => false,
    }
}

fn retry_audio_path(recording: &Recording) -> Result<PathBuf, AppError> {
    if !needs_transcript(recording) {
        return Err(AppError::NotRetryable {
            message: "the recording already has a transcript".into(),
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
    let paste_mode = state
        .settings
        .read()
        .map(|settings| settings.paste_mode)
        .unwrap_or(PasteMode::Auto);
    if let Ok(mut until) = state.suppress_shortcut_until.lock() { *until = Some(Instant::now() + Duration::from_secs(2)); }
    let result = platform::copy_and_paste(&mut SystemWindows, target, &text, paste_mode).map_err(|error| error.to_string());
    if let Ok(mut until) = state.suppress_shortcut_until.lock() { *until = Some(Instant::now() + Duration::from_millis(250)); }
    result
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
        // A GUI application on macOS is started by launchd, not by a login
        // shell, so LANG is usually empty there. AppleLocale is the setting
        // System Settings actually writes.
        #[cfg(target_os = "macos")]
        if let Ok(output) = std::process::Command::new("defaults")
            .args(["read", "-g", "AppleLocale"])
            .output()
            && output.status.success()
        {
            let locale = String::from_utf8_lossy(&output.stdout).trim().to_ascii_lowercase();
            let language = locale.split(['_', '-']).next().unwrap_or("");
            return match language {
                "pl" => "pl".to_string(),
                _ => "en".to_string(),
            };
        }
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



/// Answer to a request to start downloading.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Claim {
    /// The slot was free and is now held.
    Started,
    /// This very model is already arriving; asking twice is not an error.
    AlreadyRunning,
    /// Something else is arriving. Names it, so the refusal can say what.
    Busy(String),
}

/// Takes the download slot, or explains why it cannot be taken.
///
/// Two downloads of one model write the same files from four threads each,
/// which does not produce a model — it produces files of the right size full
/// of the wrong bytes. One slot, claimed before any byte moves.
pub fn claim_download(
    slot: &Mutex<Option<ActiveDownload>>,
    model: &str,
    total: u64,
    cancel: Arc<AtomicBool>,
) -> Claim {
    let Ok(mut guard) = slot.lock() else {
        return Claim::Busy(model.to_owned());
    };
    if let Some(active) = guard.as_ref() {
        return if active.model == model {
            Claim::AlreadyRunning
        } else {
            Claim::Busy(active.model.clone())
        };
    }
    *guard = Some(ActiveDownload {
        model: model.to_owned(),
        downloaded_bytes: 0,
        total_bytes: total,
        cancel,
    });
    Claim::Started
}

#[tauri::command]
pub async fn download_model(
    model: String,
    app: AppHandle,
    state: State<'_, AppState>,
) -> Result<(), String> {
    let spec = crate::models::spec(&model).ok_or_else(|| format!("Nieznany model: {model}."))?;
    let total = spec.total_bytes();
    let cancel = Arc::new(AtomicBool::new(false));

    // Refuse before the transfer rather than after 600 MB of it. The margin
    // covers the rest of the model plus room for the disk not to be left
    // completely full.
    let room = total + (64 << 20);
    if let Some(free) = platform::free_space_bytes(&state.model_home)
        && free < room
    {
        return Err(format!(
            "Za mało miejsca na dysku: model potrzebuje {} MB, wolne jest {} MB.",
            room / (1 << 20),
            free / (1 << 20)
        ));
    }

    match claim_download(&state.download, &model, total, cancel.clone()) {
        Claim::Started => {}
        Claim::AlreadyRunning => return Ok(()),
        Claim::Busy(other) => return Err(format!("Trwa już pobieranie modelu {other}.")),
    }
    emit_model_progress(&app, &model, "downloading", 0, Some(total));
    emit_state(&app, &state);

    let directory = state.engine.model_dir(&model);
    let progress_app = app.clone();
    let progress_model = model.clone();
    let progress_slot = state.download.clone();
    let fetch_model = model.clone();
    let fetch_cancel = cancel.clone();
    let outcome = tauri::async_runtime::spawn_blocking(move || {
        crate::models::download(
            &fetch_model,
            &directory,
            |done, total| {
                if let Ok(mut guard) = progress_slot.lock()
                    && let Some(active) = guard.as_mut()
                {
                    active.downloaded_bytes = done;
                }
                emit_model_progress(&progress_app, &progress_model, "downloading", done, Some(total));
            },
            || !fetch_cancel.load(Ordering::Relaxed),
        )
    })
    .await
    .map_err(|error| error.to_string())?;

    if let Ok(mut guard) = state.download.lock() {
        *guard = None;
    }
    let result = outcome.map_err(|error| error.to_string());
    // The interface is told either way: a snapshot with no download in it is
    // how every screen learns the transfer is over.
    if result.is_ok() {
        // A model that has just arrived is the one the user means to dictate
        // with, so it is worth loading before they press the shortcut.
        warm_up_model(&app, &state);
    }
    emit_state(&app, &state);
    result
}

/// Stops the download in flight, if there is one.
#[tauri::command]
pub fn cancel_download(state: State<'_, AppState>) -> Result<(), String> {
    if let Some(active) = state
        .download
        .lock()
        .map_err(|_| "download lock poisoned".to_owned())?
        .as_ref()
    {
        active.cancel.store(true, Ordering::Relaxed);
    }
    Ok(())
}

#[tauri::command]
pub fn delete_model(model: String, state: State<'_, AppState>) -> Result<(), String> {
    if crate::models::spec(&model).is_none() {
        return Err(format!("Nieznany model: {model}."));
    }
    if let Ok(guard) = state.download.lock()
        && guard.as_ref().is_some_and(|active| active.model == model)
    {
        return Err("Ten model właśnie się pobiera.".to_owned());
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
    mut settings: AppSettings,
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
    if settings.microphones.is_none() {
        let devices = state.audio.list_devices().map_err(|error| error.to_string())?;
        settings.microphones = Some(crate::microphones::migrate_legacy(settings.input_device.as_deref(), &devices));
    }
    if let Some(preferences) = &mut settings.microphones {
        crate::microphones::validate(preferences)?;
        if preferences.selection == crate::microphones::Selection::Manual && let Some(selected) = &preferences.selected_id {
            preferences.hidden_ids.retain(|id| id != selected);
        }
        settings.input_device = preferences.selected_id.clone();
    }
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
    let old_activation = (live.activation_mode, live.shortcut.clone());
    let old_language = live.language.clone();
    let old_source = live.transcription_source.clone();
    let result = apply_settings_change(
        &mut shortcut_effect,
        &mut autostart_effect,
        &mut storage_effect,
        &mut live,
        settings,
    )?;
    if old_activation != (live.activation_mode, live.shortcut.clone()) { invalidate_hold(&app, &state); }
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
    let source_changed = live.transcription_source != old_source;
    *state
        .settings
        .write()
        .unwrap_or_else(std::sync::PoisonError::into_inner) = live;
    if model_changed || source_changed {
        // The old model is of no further use, and holding two at once would
        // double the memory for as long as the new one takes to load. The
        // release waits behind any load already running, so it happens off
        // the thread that draws the window.
        let engine = state.engine.clone();
        std::thread::spawn(move || engine.unload());
        state.model_warm.forget();
        warm_up_model(&app, &state);
    }
    if language_changed || state.settings.read().is_ok_and(|settings| settings.microphones.is_some()) {
        let settings_guard = state
            .settings
            .read()
            .map_err(|_| "settings lock poisoned")?;
        let locale = crate::resolved_language(&settings_guard).to_owned();
        drop(settings_guard);
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

/// The providers the interface can offer, with their suggested models.
#[tauri::command]
pub fn list_cloud_providers() -> Vec<cloud::ProviderInfo> {
    cloud::catalog()
}

/// Providers that already have a key, so the settings screen can tick them
/// off without the key itself ever leaving the backend.
#[tauri::command]
pub fn list_cloud_keys(state: State<'_, AppState>) -> Vec<String> {
    available_cloud_keys(&state.storage)
}

/// Stores one provider's key, encrypted.
///
/// An empty key removes it, which is how the interface clears one without a
/// second command. The returned list is everything that now has a key, so a
/// caller never has to guess what changed.
#[tauri::command]
pub fn set_cloud_api_key(
    provider: String,
    api_key: String,
    app: AppHandle,
    state: State<'_, AppState>,
) -> Result<Vec<String>, String> {
    let keys = write_cloud_api_key(&state.storage, &provider, &api_key)?;
    // Whether the selected engine is ready now depends on this key, and the
    // snapshot is where every screen reads that from.
    emit_state(&app, &state);
    Ok(keys)
}

#[tauri::command]
pub fn clear_cloud_api_key(
    provider: String,
    app: AppHandle,
    state: State<'_, AppState>,
) -> Result<Vec<String>, String> {
    let keys = write_cloud_api_key(&state.storage, &provider, "")?;
    emit_state(&app, &state);
    Ok(keys)
}

fn write_cloud_api_key(storage: &Storage, provider: &str, api_key: &str) -> Result<Vec<String>, String> {
    if cloud::provider(provider).is_none() {
        return Err(format!("Nieznany dostawca: {provider}."));
    }
    let mut keys = stored_cloud_keys(storage);
    let api_key = api_key.trim();
    if api_key.is_empty() {
        keys.remove(provider);
    } else {
        keys.insert(provider.to_owned(), secret::protect(api_key)?);
    }
    storage
        .set_setting("cloud_keys", &keys)
        .map_err(|error| error.to_string())?;
    Ok(available_cloud_keys(storage))
}

/// Sends a second of silence to the configured provider.
///
/// An empty transcript is still an answer: it proves the address, the key and
/// the model were accepted. The time it took is reported too, because that is
/// the part a user cannot see any other way.
#[tauri::command]
pub async fn test_cloud_transcription(state: State<'_, AppState>) -> Result<CloudCheck, String> {
    let settings = state
        .settings
        .read()
        .map_err(|_| "settings lock poisoned")?
        .clone();
    let storage = state.storage.clone();
    tauri::async_runtime::spawn_blocking(move || {
        let provider = cloud::provider(&settings.cloud_provider)
            .unwrap_or_else(|| cloud::default_provider());
        let api_key = cloud_api_key(&storage, provider.key)?.ok_or_else(|| {
            format!(
                "no API key is set for {}; add one in Settings",
                provider.display
            )
        })?;
        let model = resolve_cloud_model(&settings);
        let audio = cloud::silence_wav();
        let started = Instant::now();
        let transcript = cloud::transcribe(cloud::Request {
            provider: provider.key,
            model: &model,
            base_url: &settings.cloud_base_url,
            api_key: &api_key,
            language: &settings.cloud_language,
            keyterms: &[],
            audio: &audio,
            file_name: "loquara-check.wav",
            cancel: None,
        })
        .map_err(|error| error.to_string())?;
        Ok(CloudCheck {
            text: transcript.text,
            elapsed_ms: started.elapsed().as_millis().try_into().unwrap_or(u64::MAX),
        })
    })
    .await
    .map_err(|error| error.to_string())?
}

/// The provider's own list of models, for the settings screen to offer.
#[tauri::command]
pub async fn list_cloud_models(state: State<'_, AppState>) -> Result<Vec<String>, String> {
    let settings = state
        .settings
        .read()
        .map_err(|_| "settings lock poisoned")?
        .clone();
    let storage = state.storage.clone();
    tauri::async_runtime::spawn_blocking(move || {
        let provider = cloud::provider(&settings.cloud_provider)
            .unwrap_or_else(|| cloud::default_provider());
        let api_key = cloud_api_key(&storage, provider.key)?.ok_or_else(|| {
            format!(
                "no API key is set for {}; add one in Settings",
                provider.display
            )
        })?;
        cloud::list_models(provider, &settings.cloud_base_url, &api_key)
            .map_err(|error| error.to_string())
    })
    .await
    .map_err(|error| error.to_string())?
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{DictationState, RecoveryRecording};
    use std::fs;
    use std::path::PathBuf;

    #[test]
    fn microphone_migration_is_persisted_once_and_reopening_preserves_the_endpoint() {
        let temp = tempfile::tempdir().unwrap();
        let recordings = temp.path().join("recordings");
        let storage = Storage::open_in_memory(&recordings).unwrap();
        let settings = AppSettings { input_device: Some("Fake microphone".into()), ..Default::default() };
        storage.set_setting("app", &settings).unwrap();
        let state = AppState::new(AudioRecorder::with_backend(&recordings, Arc::new(crate::audio::FakeInputBackend::default())), storage, temp.path());
        let saved = state.storage.get_setting::<AppSettings>("app").unwrap().unwrap();
        assert_eq!(saved.microphones.as_ref().unwrap().selected_id.as_deref(), Some("fake"));
        assert_eq!(saved.microphones.as_ref().unwrap().selection, crate::microphones::Selection::Manual);
        assert_eq!(saved.shortcut, default_shortcut());
        // Normalization is identity-based on reopen even when the old label
        // remains present for backwards compatibility.
        let serialized = serde_json::to_value(saved.clone()).unwrap();
        let reopened: AppSettings = serde_json::from_value(serialized).unwrap();
        assert_eq!(reopened, saved);
    }

    #[test]
    fn microphone_fault_commits_interrupted_audio_without_processing_and_stale_fault_cannot_stop_next_take() {
        let temp = tempfile::tempdir().unwrap();
        let recordings = temp.path().join("recordings");
        let storage = Storage::open_in_memory(&recordings).unwrap();
        let audio = AudioRecorder::with_backend(&recordings, Arc::new(crate::audio::FakeInputBackend::with_samples(vec![crate::audio::AudioSamples::I16(vec![12, 34])])));
        let state = AppState::new(audio, storage, temp.path());
        let (sender, receiver) = std::sync::mpsc::channel();
        state.audio.set_fault_sender(sender);
        let first = state.audio.start(Some("fake")).unwrap();
        state.storage.insert_recording(&history_recording(&first.id, RecordingStatus::Recording, &first.path)).unwrap();
        state.machine.lock().unwrap().started(&first.id, first.path.to_string_lossy()).unwrap();
        state.audio.invalidate_endpoint("fake");
        let fault = receiver.recv_timeout(Duration::from_secs(1)).unwrap();
        assert!(interrupt_microphone_capture(&state, &fault).unwrap());
        assert!(matches!(state.snapshot().unwrap().dictation, DictationState::Failed { .. }));
        let row = state.storage.get_recording(&first.id).unwrap().unwrap();
        assert_eq!(row.status, RecordingStatus::Failed);
        assert!(row.text.is_none());
        assert!(row.error.unwrap().contains("interrupted"));
        let mut wav = hound::WavReader::open(&first.path).unwrap();
        assert_eq!(wav.samples::<i16>().collect::<Result<Vec<_>, _>>().unwrap(), vec![12, 34]);
        *state.machine.lock().unwrap() = CoordinatorMachine::default();
        let second = state.audio.start(Some("fake")).unwrap();
        state.machine.lock().unwrap().started(&second.id, second.path.to_string_lossy()).unwrap();
        assert!(!interrupt_microphone_capture(&state, &fault).unwrap());
        assert!(state.audio.owns_capture(&second.id));
        assert!(matches!(state.snapshot().unwrap().dictation, DictationState::Recording { recording_id, .. } if recording_id == second.id));
        state.audio.cancel().unwrap();
    }

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

        let processing = DictationState::Processing {
            recording_id: "r".into(),
            audio_path: "r.wav".into(),
        };

        assert!(holds_discardable_audio(&recording));
        assert!(holds_discardable_audio(&cancelling));
        assert!(holds_discardable_audio(&processing));
    }

    #[test]
    fn escape_is_released_once_capture_is_over() {
        for state in [
            DictationState::Idle,
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
    fn a_cancel_press_discards_immediately() {
        let recording = DictationState::Recording {
            recording_id: "r".into(),
            audio_path: "r.wav".into(),
        };
        let processing = DictationState::Processing {
            recording_id: "r".into(),
            audio_path: "r.wav".into(),
        };

        assert_eq!(cancel_press_for(&recording, RecorderKey::Escape), CancelPress::Confirm);
        assert_eq!(cancel_press_for(&processing, RecorderKey::Escape), CancelPress::Confirm);
        assert_eq!(cancel_press_for(&recording, RecorderKey::Enter), CancelPress::Ignore);
        assert!(offers_cancel_undo(8_000));
        assert!(!offers_cancel_undo(7_999));
    }

    #[test]
    fn escape_dismisses_the_undo_chip_and_enter_stays_with_the_target_app() {
        let cancelling = DictationState::Cancelling {
            recording_id: "r".into(),
            audio_path: "r.wav".into(),
        };

        assert_eq!(cancel_press_for(&cancelling, RecorderKey::Enter), CancelPress::Ignore);
        assert_eq!(cancel_press_for(&cancelling, RecorderKey::Escape), CancelPress::Confirm);
        assert!(!awaits_cancel_answer(&cancelling));
    }

    #[test]
    fn cancel_does_nothing_when_there_is_nothing_to_discard() {
        for state in [
            DictationState::Idle,
            DictationState::Pasting {
                recording_id: "r".into(),
                audio_path: "r.wav".into(),
                transcript: "hello".into(),
            },
        ] {
            for key in [RecorderKey::Escape, RecorderKey::Enter] {
                assert_eq!(cancel_press_for(&state, key), CancelPress::Ignore, "{state:?}");
            }
            assert!(!awaits_cancel_answer(&state));
        }
    }

    #[tokio::test]
    async fn cancelling_processing_stops_a_live_session_already_moved_to_finish() {
        use futures_util::{SinkExt, StreamExt};
        use tokio_tungstenite::tungstenite::Message;

        struct SilentSink;
        impl streaming::TranscriptSink for SilentSink {
            fn publish(&self, _: &str) -> Result<(), String> { Ok(()) }
            fn failed(&self, _: &str) {}
        }
        let temp = tempfile::tempdir().unwrap();
        let recordings = temp.path().join("recordings");
        let state = AppState::new(
            AudioRecorder::new(&recordings),
            Storage::open_in_memory(&recordings).unwrap(),
            temp.path().join("models"),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let (flushing_tx, flushing_rx) = tokio::sync::oneshot::channel();
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut socket = tokio_tungstenite::accept_async(stream).await.unwrap();
            socket.send(Message::text(r#"{"type":"transcript.created"}"#)).await.unwrap();
            while let Some(Ok(message)) = socket.next().await {
                if matches!(message, Message::Text(ref text) if text.contains("audio.done")) {
                    let _ = flushing_tx.send(());
                    break;
                }
            }
            let _ = socket.next().await;
        });
        let (_audio_tx, audio_rx) = tokio::sync::mpsc::channel(1);
        let session = streaming::spawn(
            streaming::Config { url: format!("ws://127.0.0.1:{port}"), api_key: "test".into() },
            Arc::new(SilentSink), audio_rx,
        );
        *state.streaming.lock().unwrap() = Some(ActiveLiveSession { recording_id: "old".into(), cancellation: Some(session.cancellation()), session: Some(session), pump: Arc::new(Mutex::new(None)), sender: None, failure: Arc::new(Mutex::new(None)) });
        let session = state.streaming.lock().unwrap().as_mut().unwrap().session.take().unwrap();
        let finishing = tokio::spawn(session.finish(Duration::from_secs(10)));
        tokio::time::timeout(Duration::from_secs(2), flushing_rx).await.unwrap().unwrap();
        abandon_live_session(&state);
        assert!(tokio::time::timeout(Duration::from_secs(1), finishing).await.is_ok(),
            "processing cancellation must still own a way to stop the live writer");
        assert!(state.streaming.lock().unwrap().is_none());
        server.abort();
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
    fn retry_requires_a_missing_transcript_and_existing_finalized_audio() {
        let temp = tempfile::tempdir().unwrap();
        let final_audio = temp.path().join("final.wav");
        fs::write(&final_audio, b"wav").unwrap();
        let retryable =
            history_recording("retryable", RecordingStatus::Failed, final_audio.as_path());
        let cancelled =
            history_recording("cancelled", RecordingStatus::Cancelled, final_audio.as_path());
        let mut blank = history_recording("blank", RecordingStatus::Completed, final_audio.as_path());
        blank.text = Some("   ".into());
        let mut transcribed =
            history_recording("transcribed", RecordingStatus::Completed, final_audio.as_path());
        transcribed.text = Some("gotowe".into());
        let mut interrupted = history_recording(
            "interrupted",
            RecordingStatus::Failed,
            final_audio.as_path(),
        );
        interrupted.audio_path = None;

        assert_eq!(retry_audio_path(&retryable).unwrap(), final_audio);
        assert_eq!(retry_audio_path(&cancelled).unwrap(), final_audio);
        assert_eq!(retry_audio_path(&blank).unwrap(), final_audio);
        assert!(matches!(
            retry_audio_path(&interrupted),
            Err(AppError::NotRetryable { .. })
        ));
        assert!(matches!(
            retry_audio_path(&transcribed),
            Err(AppError::NotRetryable { message }) if message == "the recording already has a transcript"
        ));
    }

    #[test]
    fn a_completed_row_with_an_empty_transcript_still_needs_one() {
        // The model can finish without hearing anything, which stores an empty
        // transcript on a completed row. History has to keep offering to run
        // it again instead of treating it as done.
        let temp = tempfile::tempdir().unwrap();
        let audio_path = temp.path().join("empty.wav");
        let mut blank = history_recording("empty", RecordingStatus::Completed, &audio_path);
        blank.text = Some(String::new());

        assert!(needs_transcript(&blank));
        assert!(!needs_transcript(&Recording {
            text: Some("mamy tekst".into()),
            ..blank
        }));
        assert!(!needs_transcript(&history_recording(
            "active",
            RecordingStatus::Processing,
            &audio_path
        )));
    }

    #[test]
    fn retry_rejects_a_row_whose_audio_file_is_missing() {
        let missing = PathBuf::from("definitely-not-here.wav");
        let row = history_recording("missing", RecordingStatus::Failed, &missing);

        assert!(matches!(
            retry_audio_path(&row),
            Err(AppError::NotRetryable { message }) if message == "recording audio is missing"
        ));
    }

    #[test]
    fn short_cancel_deletes_audio_and_clears_the_stored_path() {
        let temp = tempfile::tempdir().unwrap();
        let storage = Storage::open_in_memory(temp.path().join("recordings")).unwrap();
        let audio_path = temp.path().join("short.wav");
        fs::write(&audio_path, b"wav").unwrap();
        let mut row = history_recording("short", RecordingStatus::Cancelled, &audio_path);
        row.duration_ms = 1_000;
        storage.insert_recording(&row).unwrap();

        discard_cancelled_take_if_short(&storage, "short", &audio_path, 1_000);

        assert!(!audio_path.exists());
        assert!(storage.get_recording("short").unwrap().unwrap().audio_path.is_none());
    }

    #[test]
    fn long_cancel_keeps_audio_for_undo_and_retry() {
        let temp = tempfile::tempdir().unwrap();
        let storage = Storage::open_in_memory(temp.path().join("recordings")).unwrap();
        let audio_path = temp.path().join("long.wav");
        fs::write(&audio_path, b"wav").unwrap();
        let mut row = history_recording("long", RecordingStatus::Cancelled, &audio_path);
        row.duration_ms = CANCEL_UNDO_AFTER_MS;
        storage.insert_recording(&row).unwrap();

        discard_cancelled_take_if_short(&storage, "long", &audio_path, CANCEL_UNDO_AFTER_MS);

        assert!(audio_path.is_file());
        assert_eq!(
            storage.get_recording("long").unwrap().unwrap().audio_path.as_deref(),
            Some(audio_path.to_string_lossy().as_ref())
        );
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
    fn request_cancel_opens_undo_then_dismisses_to_idle() {
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
        assert_eq!(machine.snapshot(), DictationState::Idle);
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
    fn a_second_download_never_starts_on_top_of_the_first() {
        // Leaving the dictation screen mid-download and pressing Download in
        // Settings used to start a second transfer over the first one's
        // files. Both wrote the same bytes from four threads each.
        let slot = Mutex::new(None);
        let cancel = Arc::new(AtomicBool::new(false));

        assert_eq!(
            claim_download(&slot, "parakeet", 100, cancel.clone()),
            Claim::Started
        );
        assert_eq!(
            claim_download(&slot, "parakeet", 100, cancel.clone()),
            Claim::AlreadyRunning,
            "asking again for what is already arriving is not an error"
        );
        assert_eq!(
            claim_download(&slot, "inny", 100, cancel.clone()),
            Claim::Busy("parakeet".into()),
            "and a different model has to wait its turn"
        );

        *slot.lock().unwrap() = None;
        assert_eq!(claim_download(&slot, "inny", 100, cancel), Claim::Started);
    }

    #[test]
    fn settings_default_to_not_launching_on_login() {
        assert!(!AppSettings::default().launch_on_login);
    }

    #[test]
    fn starting_in_the_tray_is_opt_in_and_survives_older_settings() {
        // The window is configured hidden, so a settings file written before
        // this option existed must still open it.
        assert!(!AppSettings::default().start_minimized);

        let older: AppSettings = serde_json::from_value(serde_json::json!({
            "inputDevice": null,
            "shortcut": "Ctrl+Space",
            "autoPaste": true,
            "retentionDays": 30
        }))
        .unwrap();

        assert!(!older.start_minimized);
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
    fn overlay_size_defaults_to_the_compact_pill() {
        assert_eq!(AppSettings::default().overlay_size, OverlaySize::Mini);

        let older: AppSettings = serde_json::from_value(serde_json::json!({
            "inputDevice": null,
            "shortcut": "Ctrl+Space",
            "autoPaste": true,
            "retentionDays": 30
        }))
        .unwrap();

        assert_eq!(older.overlay_size, OverlaySize::Mini);
    }

    #[test]
    fn settings_from_older_versions_default_to_the_local_engine() {
        // An installation that predates the cloud option has no provider
        // configured and must keep transcribing on this machine.
        let older: AppSettings = serde_json::from_value(serde_json::json!({
            "inputDevice": null,
            "shortcut": "Ctrl+Space",
            "autoPaste": true,
            "retentionDays": 30
        }))
        .unwrap();

        assert_eq!(older.transcription_source, TranscriptionSource::Local);
        assert_eq!(older.cloud_provider, cloud::default_provider().key);
        assert!(older.cloud_model.is_empty());
        assert!(older.cloud_base_url.is_empty());
        assert!(older.cloud_language.is_empty());
        assert_eq!(
            serde_json::to_value(&older).unwrap()["transcriptionSource"],
            serde_json::json!("local")
        );
    }

    #[test]
    fn a_cloud_model_falls_back_to_whatever_the_provider_calls_current() {
        let mut settings = AppSettings {
            cloud_provider: "xai".into(),
            cloud_model: "   ".into(),
            ..AppSettings::default()
        };

        assert_eq!(resolve_cloud_model(&settings), "grok-voice-transcribe-2.0");

        settings.cloud_model = "grok-voice-transcribe-1.0".into();
        assert_eq!(resolve_cloud_model(&settings), "grok-voice-transcribe-1.0");

        // A provider that no longer exists must not leave the app with no
        // model at all.
        settings.cloud_provider = "nope".into();
        settings.cloud_model.clear();
        assert_eq!(
            resolve_cloud_model(&settings),
            cloud::default_provider().default_model
        );
    }

    #[test]
    fn provider_keys_are_stored_encrypted_and_only_usable_ones_are_listed() {
        let temp = tempfile::tempdir().unwrap();
        let storage = Storage::open_in_memory(temp.path().join("recordings")).unwrap();

        write_cloud_api_key(&storage, "xai", "xai-secret").unwrap();

        assert_eq!(available_cloud_keys(&storage), vec!["xai"]);
        let stored: std::collections::BTreeMap<String, String> = storage
            .get_setting("cloud_keys")
            .unwrap()
            .unwrap();
        assert_ne!(stored["xai"], "xai-secret", "the database holds ciphertext");
        assert_eq!(
            cloud_api_key(&storage, "xai").unwrap().as_deref(),
            Some("xai-secret")
        );

        write_cloud_api_key(&storage, "xai", "").unwrap();

        assert!(available_cloud_keys(&storage).is_empty());
        assert!(cloud_api_key(&storage, "xai").unwrap().is_none());
    }

    #[test]
    fn an_unknown_provider_key_is_refused_rather_than_stored() {
        let temp = tempfile::tempdir().unwrap();
        let storage = Storage::open_in_memory(temp.path().join("recordings")).unwrap();

        assert!(write_cloud_api_key(&storage, "acme", "secret").is_err());
        assert!(available_cloud_keys(&storage).is_empty());
    }

    #[test]
    fn keyterms_are_the_spellings_the_user_wants_written() {
        let vocabulary = vec![
            VocabularyEntry { id: 1, heard: "parakit".into(), replacement: "Parakeet".into() },
            VocabularyEntry { id: 2, heard: "loquara".into(), replacement: "Loquara".into() },
        ];

        assert_eq!(keyterms_for(&vocabulary), vec!["Parakeet", "Loquara"]);
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
            _duration_ms: Option<u64>,
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
                duration_ms: None,
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
                duration_ms: None,
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
            activation_mode: ActivationMode::PushToTalk,
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

#[cfg(test)]
mod buffered_delivery_tests {
    use super::*;
    use crate::platform::PlatformError;
    #[derive(Default)]
    struct Target { clipboard: String, pastes: usize, focused: Option<WindowTarget>, fail_clipboard: bool, unavailable_keys: bool }
    impl WindowsApi for Target {
        fn modifiers_released(&mut self) -> Result<bool, PlatformError> {
            if self.unavailable_keys { Err(PlatformError::PasteFailed("keys unavailable".into())) } else { Ok(true) }
        }
        fn foreground_window(&mut self) -> Option<WindowTarget> { self.focused }
        fn restore_foreground(&mut self, target: WindowTarget) -> Result<(), PlatformError> { self.focused = Some(target); Ok(()) }
        fn write_clipboard(&mut self, text: &str) -> Result<(), PlatformError> {
            if self.fail_clipboard { return Err(PlatformError::ClipboardWrite("busy".into())); }
            self.clipboard = text.into(); Ok(())
        }
        fn send_ctrl_v(&mut self) -> Result<(), PlatformError> { self.pastes += 1; Ok(()) }
    }
    fn fixture() -> (tempfile::TempDir, AppState, FrozenTake) {
        let dir = tempfile::tempdir().unwrap();
        let storage = Storage::open_in_memory(dir.path()).unwrap();
        let state = AppState::new(AudioRecorder::new(dir.path()), storage, dir.path());
        state.storage.insert_recording(&Recording { id: "take".into(), created_at: 1, duration_ms: 0,
            status: RecordingStatus::Processing, text: None, model: None, audio_path: Some("take.wav".into()), source_app: None, error: None, peaks: None }).unwrap();
        state.machine.lock().unwrap().state = DictationState::Processing { recording_id: "take".into(), audio_path: "take.wav".into() };
        let take = FrozenTake::capture(&state, AppSettings::default(), Some(WindowTarget(42)), "take".into());
        *state.active_take.lock().unwrap() = Some(take.clone());
        (dir, state, take)
    }
    fn final_result(text: &str) -> TranscriptionResult { TranscriptionResult { text: text.into(), model: "parakeet".into(), language: None, duration_ms: 1 } }
    #[test]
    fn ptt_duplicate_automatic_cleanup_preserves_undo_until_explicit_owned_dismissal() {
        let (_dir, state, mut take) = fixture();
        let gesture = Arc::new(HoldGesture::default());
        take.hold = Some(gesture.clone());
        *state.active_take.lock().unwrap() = Some(take);
        state.machine.lock().unwrap().state = DictationState::Recording { recording_id: "take".into(), audio_path: "take.wav".into() };
        consume_owned_hold_cancel(&state, &gesture, || {
            complete_cancel_feedback(&state, "take", true)?;
            state.snapshot()
        }).unwrap().unwrap();
        let cancelled = state.machine.lock().unwrap().snapshot();
        let notice = state.overlay_feedback.lock().unwrap().notice.clone().unwrap();
        assert!(matches!(cancelled, DictationState::Cancelling { .. }));
        assert_eq!(notice.kind, OverlayNoticeKind::Cancelled);
        assert!(notice.expires_monotonic.is_some());
        let second = consume_owned_hold_cancel(&state, &gesture, || -> Result<AppSnapshot, String> {
            // This is the existing explicit-dismiss effect, which an automatic
            // second cancellation must never reach.
            state.overlay_feedback.lock().unwrap().notice = None;
            state.machine.lock().unwrap().cancel().unwrap();
            state.snapshot()
        }).unwrap();
        assert!(second.is_none(), "duplicate automatic cleanup must be inert");
        assert_eq!(state.machine.lock().unwrap().snapshot(), cancelled);
        let preserved = state.overlay_feedback.lock().unwrap().notice.clone().unwrap();
        assert_eq!(preserved.key, notice.key); assert_eq!(preserved.generation, notice.generation);
        assert_eq!(preserved.expires_at, notice.expires_at);
        assert_eq!(preserved.expires_monotonic, notice.expires_monotonic);
        {
            let _gate = state.lifecycle.lock().unwrap();
            assert!(dismiss_cancel_undo_if_owned(&state, "take", &notice), "explicit second Esc still dismisses its observed Undo");
        }
        assert_eq!(state.machine.lock().unwrap().snapshot(), DictationState::Idle);
        assert!(consume_owned_hold_cancel(&state, &gesture, || -> Result<(), String> { panic!("Idle auto cleanup") }).unwrap().is_none());
        *state.active_take.lock().unwrap() = Some(FrozenTake::capture(&state, AppSettings::default(), None, "new".into()));
        state.machine.lock().unwrap().state = DictationState::Recording { recording_id: "new".into(), audio_path: "new.wav".into() };
        assert!(consume_owned_hold_cancel(&state, &gesture, || -> Result<(), String> { panic!("old hold affected new take") }).is_err());
        let _gate = state.lifecycle.lock().unwrap();
        assert!(!dismiss_cancel_undo_if_owned(&state, "take", &notice), "delayed explicit Esc must not dismiss a new take");
    }

    #[test]
    fn ptt_desktop_loss_after_release_cleans_processing_and_rearms_without_touching_new_take() {
        let (_dir, state, mut take) = fixture();
        let gesture = state.hold.lock().unwrap().press(true).unwrap();
        state.hold.lock().unwrap().sample(&gesture, false, true, false);
        take.hold = Some(gesture.clone());
        *state.active_take.lock().unwrap() = Some(take.clone());
        let mut notified = false;
        let done = consume_hold_sample(&state, &gesture, Err(PlatformError::PasteFailed("desktop lost".into())), &mut notified, || {
            with_owned_capture(&state, Some(&gesture), || cancel_processing_state(&state, "take")).unwrap();
        });
        assert_eq!(state.machine.lock().unwrap().snapshot(), DictationState::Idle, "late loss must not strand Processing");
        assert_eq!(state.storage.get_recording("take").unwrap().unwrap().status, RecordingStatus::Cancelled);
        assert!(done && notified);
        assert!(!take.owns(&state));
        let next = state.hold.lock().unwrap().press(true).expect("unavailable keys must not leave a stranded latch");
        let mut next_take = FrozenTake::capture(&state, AppSettings::default(), None, "new".into());
        next_take.hold = Some(next.clone());
        *state.active_take.lock().unwrap() = Some(next_take);
        state.machine.lock().unwrap().state = DictationState::Recording { recording_id: "new".into(), audio_path: "new.wav".into() };
        let mut stale_notified = false;
        consume_hold_sample(&state, &gesture, Err(PlatformError::PasteFailed("late loss".into())), &mut stale_notified, || {
            assert!(with_owned_capture(&state, Some(&gesture), || -> Result<(), String> { panic!("old monitor cancelled new take") }).is_err());
        });
        assert!(!next.cancelled());
        assert!(state.hold.lock().unwrap().current().is_some_and(|current| Arc::ptr_eq(&current, &next)));
        assert!(matches!(state.machine.lock().unwrap().snapshot(), DictationState::Recording { recording_id, .. } if recording_id == "new"));
    }

    #[test]
    fn ptt_release_during_slow_start_stops_only_owned_capture_through_production_gates() {
        let (_dir, state, mut take) = fixture();
        state.settings.write().unwrap().activation_mode = ActivationMode::PushToTalk;
        state.machine.lock().unwrap().state = DictationState::Idle;
        let gesture = state.hold.lock().unwrap().press(true).unwrap();
        take.hold = Some(gesture.clone());
        let (entered_tx, entered_rx) = std::sync::mpsc::channel();
        let (resume_tx, resume_rx) = std::sync::mpsc::channel();
        let starter_state = state.clone(); let starter_hold = gesture.clone();
        let starter = std::thread::spawn(move || with_capture_start(&starter_state, Some(&starter_hold), || {
            entered_tx.send(()).unwrap();
            resume_rx.recv().unwrap(); // native microphone startup is slow
            *starter_state.active_take.lock().unwrap() = Some(take);
            starter_state.machine.lock().unwrap().started("take", "take.wav").unwrap();
            starter_state.snapshot()
        }));
        entered_rx.recv_timeout(Duration::from_secs(2)).unwrap();
        state.hold.lock().unwrap().sample(&gesture, false, false, false);
        assert!(gesture.released());
        let stopper_state = state.clone(); let stopper_hold = gesture.clone();
        let stopper = std::thread::spawn(move || with_owned_capture(&stopper_state, Some(&stopper_hold), || {
            stopper_state.machine.lock().unwrap().stopped().map_err(|e| e.to_string())
        }));
        resume_tx.send(()).unwrap();
        starter.join().unwrap().unwrap(); stopper.join().unwrap().unwrap();
        assert!(matches!(state.machine.lock().unwrap().snapshot(), DictationState::Processing { .. }));
        // A later GUI recording owns neither the old stop nor old Esc work.
        *state.active_take.lock().unwrap() = Some(FrozenTake::capture(&state, AppSettings::default(), None, "new".into()));
        assert!(with_owned_capture(&state, Some(&gesture), || -> Result<(), String> { panic!("stale release reached capture") }).is_err());
    }

    #[test]
    fn ptt_fast_release_cancel_and_suspension_never_open_microphone() {
        for reason in ["release", "escape", "suspended", "mode"] {
            let (_dir, state, _take) = fixture();
            state.settings.write().unwrap().activation_mode = ActivationMode::PushToTalk;
            state.machine.lock().unwrap().state = DictationState::Idle;
            let gesture = state.hold.lock().unwrap().press(true).unwrap();
            match reason {
                "release" => state.hold.lock().unwrap().sample(&gesture, false, false, false),
                "escape" => gesture.cancel(),
                "suspended" => state.hold.lock().unwrap().suspended = true,
                _ => state.settings.write().unwrap().activation_mode = ActivationMode::Toggle,
            }
            with_capture_start(&state, Some(&gesture), || panic!("invalid hold opened microphone: {reason}")).unwrap();
            assert_eq!(state.machine.lock().unwrap().snapshot(), DictationState::Idle);
        }
    }

    #[test]
    fn ptt_production_admission_and_owned_stop_reject_released_cancelled_and_foreign_takes() {
        let (_dir, state, mut take) = fixture();
        state.settings.write().unwrap().activation_mode = ActivationMode::PushToTalk;
        state.machine.lock().unwrap().state = DictationState::Idle;
        let gesture = state.hold.lock().unwrap().press(true).unwrap();
        assert!(hold_may_start(&state, &gesture));
        state.hold.lock().unwrap().sample(&gesture, false, true, false);
        assert!(!hold_may_start(&state, &gesture), "fast release must not start a stranded capture");
        state.machine.lock().unwrap().state = DictationState::Recording { recording_id: "take".into(), audio_path: "take.wav".into() };
        assert!(!owns_held_capture(&state, &gesture), "UI-origin capture has no hold owner");
        take.hold = Some(gesture.clone());
        *state.active_take.lock().unwrap() = Some(take.clone());
        assert!(owns_held_capture(&state, &gesture), "released hold still owns its capture cleanup");
        let newer = Arc::new(HoldGesture::default());
        take.hold = Some(newer);
        *state.active_take.lock().unwrap() = Some(take);
        assert!(!owns_held_capture(&state, &gesture), "stale key-up cannot stop newer capture");
        gesture.cancel();
        state.machine.lock().unwrap().state = DictationState::Idle;
        assert!(!hold_may_start(&state, &gesture));
    }

    #[test]
    fn ptt_modifier_wait_is_outside_lifecycle_and_cancel_prevents_all_delivery() {
        let (_dir, state, take) = fixture();
        struct CancelDuringWait<'a> { state: &'a AppState, take: &'a FrozenTake }
        impl WindowsApi for CancelDuringWait<'_> {
            fn foreground_window(&mut self) -> Option<WindowTarget> { None }
            fn restore_foreground(&mut self, _: WindowTarget) -> Result<(), PlatformError> { panic!("cancelled take focused") }
            fn write_clipboard(&mut self, _: &str) -> Result<(), PlatformError> { panic!("cancelled take copied") }
            fn send_ctrl_v(&mut self) -> Result<(), PlatformError> { panic!("cancelled take pasted") }
            fn modifiers_released(&mut self) -> Result<bool, PlatformError> {
                assert!(self.state.lifecycle.try_lock().is_ok(), "Esc must be able to acquire lifecycle while waiting");
                self.take.cancel.store(true, Ordering::SeqCst);
                Ok(false)
            }
            fn pause_for_modifiers(&mut self) {}
        }
        let mut target = CancelDuringWait { state: &state, take: &take };
        assert!(complete_take(&state, &take, Path::new("take.wav"), &final_result("cancelled"), &mut target).unwrap().is_none());
        assert_eq!(state.storage.get_recording("take").unwrap().unwrap().status, RecordingStatus::Processing);
    }

    #[test]
    fn ptt_ctrl_up_space_held_waits_for_main_release_before_final_paste() {
        let (_dir, state, mut take) = fixture();
        let gesture = state.hold.lock().unwrap().press(true).unwrap();
        state.hold.lock().unwrap().sample(&gesture, false, true, false);
        take.hold = Some(gesture.clone());
        *state.active_take.lock().unwrap() = Some(take.clone());
        struct ReleaseMain<'a> { state: &'a AppState, gesture: Arc<HoldGesture>, target: Target, waited: bool }
        impl WindowsApi for ReleaseMain<'_> {
            fn foreground_window(&mut self) -> Option<WindowTarget> { None }
            fn restore_foreground(&mut self, target: WindowTarget) -> Result<(), PlatformError> { self.target.restore_foreground(target) }
            fn write_clipboard(&mut self, text: &str) -> Result<(), PlatformError> { self.target.write_clipboard(text) }
            fn send_ctrl_v(&mut self) -> Result<(), PlatformError> {
                assert!(self.gesture.main_released(), "Ctrl release must not paste while Space is held");
                self.target.send_ctrl_v()
            }
            fn pause_for_modifiers(&mut self) {
                assert!(self.state.lifecycle.try_lock().is_ok());
                self.waited = true;
                self.state.hold.lock().unwrap().sample(&self.gesture, false, false, false);
            }
        }
        let mut target = ReleaseMain { state: &state, gesture, target: Target::default(), waited: false };
        complete_take(&state, &take, Path::new("take.wav"), &final_result("released"), &mut target).unwrap().unwrap().unwrap();
        assert!(target.waited); assert_eq!(target.target.pastes, 1);
    }

    #[test]
    fn ptt_repressed_main_key_waits_and_focus_time_cancel_or_repress_never_injects() {
        for action in ["wait_repress", "focus_repress", "focus_cancel"] {
            let (_dir, state, mut take) = fixture();
            let gesture = state.hold.lock().unwrap().press(true).unwrap();
            state.hold.lock().unwrap().sample(&gesture, false, false, false);
            take.hold = Some(gesture.clone());
            *state.active_take.lock().unwrap() = Some(take.clone());
            struct DeliveryTarget { gesture: Arc<HoldGesture>, action: &'static str, held: bool, waits: usize, copies: usize, pastes: usize }
            impl WindowsApi for DeliveryTarget {
                fn foreground_window(&mut self) -> Option<WindowTarget> { None }
                fn restore_foreground(&mut self, _: WindowTarget) -> Result<(), PlatformError> {
                    if self.action == "focus_cancel" { self.gesture.cancel(); }
                    if self.action == "focus_repress" { self.held = true; }
                    Ok(())
                }
                fn write_clipboard(&mut self, _: &str) -> Result<(), PlatformError> { self.copies += 1; Ok(()) }
                fn send_ctrl_v(&mut self) -> Result<(), PlatformError> {
                    assert!(!self.held && !self.gesture.cancelled(), "focus-time key/cancel change must block input");
                    self.pastes += 1; Ok(())
                }
                fn paste_keys_released(&mut self, shortcut: Option<&platform::Shortcut>) -> Result<bool, PlatformError> {
                    assert_eq!(shortcut.map(ToString::to_string).as_deref(), Some("Ctrl+Space"));
                    Ok(!self.held)
                }
                fn pause_for_modifiers(&mut self) { self.waits += 1; self.held = false; }
            }
            let mut target = DeliveryTarget { gesture, action, held: action == "wait_repress", waits: 0, copies: 0, pastes: 0 };
            let result = complete_take(&state, &take, Path::new("take.wav"), &final_result("complete"), &mut target).unwrap().unwrap();
            assert_eq!(state.machine.lock().unwrap().snapshot(), DictationState::Idle);
            assert_eq!(target.copies, 1);
            let mut terminal = None;
            assert!(with_completion_feedback(&state, &take, || terminal = Some(state.snapshot().unwrap().dictation)));
            assert_eq!(terminal, Some(DictationState::Idle), "cancelled delivery must publish terminal state");
            if action == "wait_repress" {
                result.unwrap(); assert_eq!(target.waits, 1); assert_eq!(target.pastes, 1);
            } else {
                assert!(result.is_err()); assert_eq!(target.pastes, 0);
                assert_eq!(state.snapshot().unwrap().overlay_notice.unwrap().kind, OverlayNoticeKind::DeliveryFailed);
            }
        }
    }

    #[test]
    fn ptt_unavailable_keys_complete_to_clipboard_with_delivery_error() {
        let (_dir, state, take) = fixture();
        let mut target = Target { unavailable_keys: true, ..Default::default() };
        assert!(complete_take(&state, &take, Path::new("take.wav"), &final_result("safe result"), &mut target).unwrap().unwrap().is_err());
        assert!(target.clipboard.contains("safe result"));
        assert_eq!(target.pastes, 0);
        assert_eq!(target.focused, None);
        assert_eq!(state.storage.get_recording("take").unwrap().unwrap().status, RecordingStatus::Completed);
        assert_eq!(state.snapshot().unwrap().overlay_notice.unwrap().kind, OverlayNoticeKind::DeliveryFailed);
    }

    #[test]
    fn delayed_undo_dismissal_cannot_cancel_retry_or_a_new_same_id_cancellation() {
        for (next_id, retry) in [("take", true), ("new-take", false)] {
            let (_dir, state, _take) = fixture();
            state.machine.lock().unwrap().state = DictationState::Recording { recording_id: "take".into(), audio_path: "take.wav".into() };
            let observed = complete_cancel_feedback(&state, "take", true).unwrap();
            // A request captured this notice before acquiring lifecycle. Undo
            // or a new take wins the gate before that request resumes.
            begin_overlay_attempt(&state);
            state.machine.lock().unwrap().state = if retry {
                DictationState::Processing { recording_id: next_id.into(), audio_path: "take.wav".into() }
            } else { DictationState::Recording { recording_id: next_id.into(), audio_path: "new.wav".into() } };
            let active = state.snapshot().unwrap().dictation;
            {
                let _gate = state.lifecycle.lock().unwrap();
                assert!(!dismiss_cancel_undo_if_owned(&state, "take", &observed));
            }
            assert_eq!(state.snapshot().unwrap().dictation, active);
            // Also protect a subsequent cancellation with the same ID: state
            // and ID alone are insufficient, the notice owner must match.
            state.machine.lock().unwrap().state = DictationState::Recording { recording_id: next_id.into(), audio_path: "take.wav".into() };
            let newer = complete_cancel_feedback(&state, next_id, true).unwrap();
            let _gate = state.lifecycle.lock().unwrap();
            assert!(!dismiss_cancel_undo_if_owned(&state, "take", &observed));
            assert_eq!(state.snapshot().unwrap().overlay_notice.unwrap().key, newer.key);
            assert!(dismiss_cancel_undo_if_owned(&state, next_id, &newer));
            assert!(matches!(state.snapshot().unwrap().dictation, DictationState::Idle));
            assert!(state.snapshot().unwrap().overlay_notice.is_none());
        }
    }

    #[tokio::test]
    async fn monotonic_expiry_rewaits_after_early_wake_and_ignores_backward_wall_clock() {
        // Cover all user-visible durations, including authoritative long Undo.
        for (duration, undo) in [(900, false), (1600, false), (5000, true)] {
            let (_dir, state, _take) = fixture();
            state.machine.lock().unwrap().state = if undo {
                DictationState::Cancelling { recording_id: "take".into(), audio_path: "take.wav".into() }
            } else { DictationState::Idle };
            let mut notice = publish_overlay_notice(&state, "take", if undo { OverlayNoticeKind::Cancelled } else { OverlayNoticeKind::Copied }, Some(duration), None);
            let deadline = notice.expires_monotonic.unwrap();
            // Equivalent to moving the OS clock backward by a day: the epoch
            // expiry is now far in the future but monotonic lifetime is intact.
            notice.expires_at = Some(epoch_ms() + 86_400_000);
            state.overlay_feedback.lock().unwrap().notice = Some(notice.clone());
            let mut clocks = [deadline - Duration::from_millis(duration as u64), deadline - Duration::from_millis(1), deadline, deadline].into_iter();
            let sleeps = std::cell::RefCell::new(Vec::new());
            let hides = std::cell::Cell::new(0);
            run_overlay_expiry(&state, &notice, || clocks.next().expect("bounded monotonic reads"), |delay| {
                sleeps.borrow_mut().push(delay);
                assert!(state.snapshot().unwrap().overlay_notice.is_some());
                std::future::ready(()) // first completion deliberately wakes early
            }, || {
                assert!(state.lifecycle.try_lock().is_err(), "native hide remains inside lifecycle ownership");
                assert!(state.snapshot().unwrap().overlay_notice.is_none());
                assert!(matches!(state.snapshot().unwrap().dictation, DictationState::Idle));
                hides.set(hides.get() + 1);
            }).await;
            assert_eq!(*sleeps.borrow(), vec![Duration::from_millis(duration as u64), Duration::from_millis(1)]);
            assert_eq!(hides.get(), 1, "early wake must eventually clear and hide once");
            assert!(!serde_json::to_value(&notice).unwrap().as_object().unwrap().contains_key("expiresMonotonic"));
        }
    }

    #[tokio::test]
    async fn monotonic_expiry_runner_never_hides_a_new_same_id_attempt() {
        let (_dir, state, _take) = fixture();
        state.machine.lock().unwrap().state = DictationState::Idle;
        let old = publish_overlay_notice(&state, "take", OverlayNoticeKind::Copied, Some(1600), None);
        let deadline = old.expires_monotonic.unwrap();
        begin_overlay_attempt(&state);
        let new = publish_overlay_notice(&state, "take", OverlayNoticeKind::Copied, Some(1600), None);
        run_overlay_expiry(&state, &old, || deadline, |_| std::future::ready(()), || panic!("stale timer hid a newer attempt")).await;
        assert_eq!(state.snapshot().unwrap().overlay_notice.unwrap().key, new.key);
    }

    #[test]
    fn terminal_notice_follows_durable_history_and_actual_delivery_then_expires() {
        struct ObservedTarget<'a> { state: &'a AppState, calls: Vec<&'static str> }
        impl WindowsApi for ObservedTarget<'_> {
            fn foreground_window(&mut self) -> Option<WindowTarget> { Some(WindowTarget(42)) }
            fn restore_foreground(&mut self, _: WindowTarget) -> Result<(), PlatformError> { self.calls.push("focus"); Ok(()) }
            fn write_clipboard(&mut self, _: &str) -> Result<(), PlatformError> {
                assert_eq!(self.state.storage.get_recording("take").unwrap().unwrap().status, RecordingStatus::Completed);
                assert!(self.state.overlay_feedback.lock().unwrap().notice.is_none());
                self.calls.push("copy"); Ok(())
            }
            fn send_ctrl_v(&mut self) -> Result<(), PlatformError> {
                assert!(self.state.overlay_feedback.lock().unwrap().notice.is_none());
                self.calls.push("paste"); Ok(())
            }
        }
        let (_dir, state, take) = fixture();
        let mut target = ObservedTarget { state: &state, calls: vec![] };
        complete_take(&state, &take, Path::new("take.wav"), &final_result("hello"), &mut target).unwrap();
        assert!(target.calls.iter().position(|call| *call == "copy").unwrap() < target.calls.iter().position(|call| *call == "paste").unwrap());
        let notice = state.snapshot().unwrap().overlay_notice.unwrap();
        assert_eq!(notice.kind, OverlayNoticeKind::PasteRequested);
        assert!(matches!(state.snapshot().unwrap().dictation, DictationState::Idle));
        assert!(!clear_overlay_notice(&state, &notice.key, notice.generation, Some(notice.expires_monotonic.unwrap() - Duration::from_millis(1))));
        assert!(clear_overlay_notice(&state, &notice.key, notice.generation, Some(notice.expires_monotonic.unwrap())));
        assert!(!clear_overlay_notice(&state, &notice.key, notice.generation, None));
    }

    #[test]
    fn copy_only_and_failed_delivery_have_truthful_notices() {
        let (_dir, state, mut take) = fixture(); take.settings.auto_paste = false;
        let mut target = Target::default();
        complete_take(&state, &take, Path::new("take.wav"), &final_result("hello"), &mut target).unwrap();
        assert_eq!(state.snapshot().unwrap().overlay_notice.unwrap().kind, OverlayNoticeKind::Copied);
        assert_eq!(target.pastes, 0);
        let (_dir, state, take) = fixture();
        let mut target = Target { fail_clipboard: true, ..Default::default() };
        complete_take(&state, &take, Path::new("take.wav"), &final_result("hello"), &mut target).unwrap();
        let notice = state.snapshot().unwrap().overlay_notice.unwrap();
        assert_eq!(notice.kind, OverlayNoticeKind::DeliveryFailed);
        assert!(notice.detail.is_some()); assert!(notice.expires_at.is_none());
        assert!(!clear_overlay_notice(&state, &notice.key, notice.generation, Some(Instant::now() + Duration::from_secs(86400))));
        assert!(clear_overlay_notice(&state, &notice.key, notice.generation, None));
    }

    #[test]
    fn stale_notice_dismiss_and_expiry_cannot_hide_new_take_or_same_id_retry() {
        for new_id in ["new-take", "take"] {
            let (_dir, state, take) = fixture();
            complete_take(&state, &take, Path::new("take.wav"), &final_result("hello"), &mut Target::default()).unwrap();
            let old = state.snapshot().unwrap().overlay_notice.unwrap();
            begin_overlay_attempt(&state);
            state.machine.lock().unwrap().state = DictationState::Processing { recording_id: new_id.into(), audio_path: "take.wav".into() };
            assert!(!clear_overlay_notice(&state, &old.key, old.generation, Some(Instant::now() + Duration::from_secs(86400))));
            assert!(!clear_overlay_notice(&state, &old.key, old.generation, None));
            state.machine.lock().unwrap().state = DictationState::Idle;
            let new = publish_overlay_notice(&state, new_id, OverlayNoticeKind::Copied, Some(1600), None);
            assert_ne!(new.generation, old.generation); assert_ne!(new.key, old.key);
            assert!(!clear_overlay_notice(&state, &old.key, old.generation, Some(Instant::now() + Duration::from_secs(86400))));
            assert_eq!(state.snapshot().unwrap().overlay_notice.unwrap().key, new.key);
            assert!(!clear_overlay_notice(&state, &new.key, old.generation, None));
        }
    }

    #[test]
    fn cancellation_feedback_expiry_preserves_long_undo_and_guards_active_work() {
        for (processing, undo, duration) in [(false, false, 900), (false, true, 5000), (true, false, 900)] {
            let (_dir, state, _take) = fixture();
            state.machine.lock().unwrap().state = if processing { DictationState::Processing { recording_id: "take".into(), audio_path: "take.wav".into() } } else { DictationState::Recording { recording_id: "take".into(), audio_path: "take.wav".into() } };
            let before = epoch_ms();
            let notice = complete_cancel_feedback(&state, "take", undo).unwrap();
            assert!(notice.expires_at.unwrap() >= before + duration);
            assert_eq!(notice.kind, OverlayNoticeKind::Cancelled);
            assert_eq!(matches!(state.snapshot().unwrap().dictation, DictationState::Cancelling { .. }), undo);
            assert!(!clear_overlay_notice(&state, &notice.key, notice.generation, Some(notice.expires_monotonic.unwrap() - Duration::from_millis(1))));
            assert!(clear_overlay_notice(&state, &notice.key, notice.generation, Some(notice.expires_monotonic.unwrap())));
            assert!(matches!(state.snapshot().unwrap().dictation, DictationState::Idle));
        }
        let (_dir, state, _take) = fixture();
        let notice = publish_overlay_notice(&state, "take", OverlayNoticeKind::Cancelled, Some(900), None);
        assert!(!clear_overlay_notice(&state, &notice.key, notice.generation, Some(Instant::now() + Duration::from_secs(86400))), "active processing never hides");
    }

    #[test]
    fn durable_final_copies_and_pastes_once_and_duplicate_completion_is_inert() {
        let (_dir, state, take) = fixture(); let mut target = Target::default();
        assert!(complete_take(&state, &take, Path::new("take.wav"), &final_result("Dzień dobry"), &mut target).unwrap().is_some());
        assert_eq!(state.storage.get_recording("take").unwrap().unwrap().status, RecordingStatus::Completed);
        assert_eq!(target.pastes, 1); assert_eq!(target.focused, Some(WindowTarget(42)));
        assert!(target.clipboard.contains("Dzień dobry"));
        assert!(complete_take(&state, &take, Path::new("take.wav"), &final_result("wrong"), &mut target).unwrap().is_none());
        assert_eq!(target.pastes, 1); assert!(!target.clipboard.contains("wrong"));
    }
    #[test]
    fn empty_final_clears_speculative_clipboard_and_never_pastes() {
        let (_dir, state, take) = fixture(); let mut target = Target { clipboard: "speculation".into(), ..Default::default() };
        complete_take(&state, &take, Path::new("take.wav"), &final_result(""), &mut target).unwrap();
        assert_eq!(target.clipboard, ""); assert_eq!(target.pastes, 0);
    }
    #[test]
    fn silent_final_with_custom_prefix_suffix_stays_empty_and_never_pastes() {
        for raw in ["", " \n\t "] {
            let (_dir, state, mut take) = fixture();
            take.mode = Some(Mode { id: "custom".into(), name: "custom".into(), description: String::new(),
                prompt: "prefix: NOTATKA\nsuffix: KONIEC".into(), enabled: true, is_default: false, created_at: 0 });
            let mut target = Target { clipboard: "speculative words".into(), ..Default::default() };
            complete_take(&state, &take, Path::new("take.wav"), &final_result(raw), &mut target).unwrap();
            assert_eq!(target.clipboard, ""); assert_eq!(target.pastes, 0);
            assert_eq!(state.storage.get_recording("take").unwrap().unwrap().text, Some(String::new()));
        }
    }
    #[test]
    fn duplicate_stop_during_processing_does_not_revoke_the_original_final() {
        let (_dir, state, take) = fixture();
        assert!(stop_capture_or_abandon(&state).is_err());
        assert!(take.owns(&state), "duplicate stop must leave the processing take alive");
        let mut target = Target::default();
        assert!(complete_take(&state, &take, Path::new("take.wav"), &final_result("original final"), &mut target).unwrap().is_some());
        assert_eq!(target.pastes, 1);
        assert_eq!(state.machine.lock().unwrap().snapshot(), DictationState::Idle);
    }
    #[test]
    fn frozen_auto_paste_and_target_survive_settings_edits() {
        let (_dir, state, mut take) = fixture(); take.settings.auto_paste = false;
        *state.active_take.lock().unwrap() = Some(take.clone());
        state.settings.write().unwrap().auto_paste = true;
        *state.target_window.lock().unwrap() = Some(WindowTarget(99));
        let mut target = Target::default();
        complete_take(&state, &take, Path::new("take.wav"), &final_result("hello"), &mut target).unwrap();
        assert_eq!(target.pastes, 0); assert!(!target.clipboard.is_empty());
    }
    #[test]
    fn cancelled_or_previous_attempt_cannot_copy_even_when_retry_has_same_recording_id() {
        let (_dir, state, take) = fixture(); let mut target = Target { clipboard: "external copy".into(), ..Default::default() };
        take.cancel.store(true, Ordering::SeqCst);
        let next = FrozenTake::capture(&state, AppSettings::default(), Some(WindowTarget(99)), "take".into());
        *state.active_take.lock().unwrap() = Some(next);
        assert!(complete_take(&state, &take, Path::new("take.wav"), &final_result("late"), &mut target).unwrap().is_none());
        assert_eq!(target.clipboard, "external copy"); assert_eq!(target.pastes, 0);
        assert_eq!(state.storage.get_recording("take").unwrap().unwrap().status, RecordingStatus::Processing);
    }
    #[test]
    fn clipboard_failure_preserves_successful_history_without_paste() {
        let (_dir, state, take) = fixture(); let mut target = Target { fail_clipboard: true, ..Default::default() };
        assert!(complete_take(&state, &take, Path::new("take.wav"), &final_result("recoverable"), &mut target).unwrap().unwrap().is_err());
        assert_eq!(target.pastes, 0);
        assert_eq!(state.storage.get_recording("take").unwrap().unwrap().status, RecordingStatus::Completed);
    }

    #[test]
    fn draft_revisions_update_clipboard_and_snapshot_without_any_target_input() {
        let (_dir, state, take) = fixture();
        state.machine.lock().unwrap().state = DictationState::Recording { recording_id: "take".into(), audio_path: "take.wav".into() };
        let mut target = Target::default();
        assert!(publish_draft(&state, &take, "draft one", &mut target));
        assert!(publish_draft(&state, &take, &"replacement ".repeat(500), &mut target));
        assert_eq!(target.pastes, 0); assert_eq!(target.focused, None);
        let live = state.live_transcript.lock().unwrap().clone().unwrap();
        assert_eq!(live.recording_id, "take"); assert_eq!(live.revision, 2);
        assert_eq!(live.text, target.clipboard); assert!(live.clipboard_ready);
        take.cancel.store(true, Ordering::SeqCst);
        target.clipboard = "user copied this".into();
        assert!(!publish_draft(&state, &take, "late", &mut target));
        assert_eq!(target.clipboard, "user copied this");
    }
    #[test]
    fn locked_draft_clipboard_is_retryable_without_disabling_final_delivery() {
        let (_dir, state, take) = fixture();
        state.machine.lock().unwrap().state = DictationState::Recording { recording_id: "take".into(), audio_path: "take.wav".into() };
        let mut target = Target { fail_clipboard: true, ..Default::default() };
        assert!(publish_draft(&state, &take, "draft", &mut target));
        assert!(!state.live_transcript.lock().unwrap().as_ref().unwrap().clipboard_ready);
        target.fail_clipboard = false;
        assert!(publish_draft(&state, &take, "draft", &mut target));
        assert_eq!(target.clipboard, "draft"); assert_eq!(target.pastes, 0);
        state.machine.lock().unwrap().stopped().unwrap();
        complete_take(&state, &take, Path::new("take.wav"), &final_result("final"), &mut target).unwrap();
        assert_eq!(target.pastes, 1); assert!(target.clipboard.contains("final"));
    }
    #[test]
    fn missing_target_keeps_final_in_clipboard_and_does_not_paste_elsewhere() {
        let (_dir, state, mut take) = fixture(); take.target = None;
        let mut target = Target::default();
        assert!(complete_take(&state, &take, Path::new("take.wav"), &final_result("retained"), &mut target).unwrap().unwrap().is_err());
        assert!(target.clipboard.contains("retained")); assert_eq!(target.pastes, 0);
    }
    #[test]
    fn frozen_vocabulary_and_mode_apply_once_despite_mid_take_edits() {
        let (_dir, state, mut take) = fixture();
        take.vocabulary = vec![VocabularyEntry { id: 1, heard: "alpha".into(), replacement: "beta".into() }, VocabularyEntry { id: 2, heard: "beta".into(), replacement: "gamma".into() }];
        let expected = postprocess_with_mode("alpha", &take.vocabulary, take.mode.as_ref());
        state.settings.write().unwrap().active_mode = "raw".into();
        let mut target = Target::default();
        complete_take(&state, &take, Path::new("take.wav"), &final_result("alpha"), &mut target).unwrap();
        assert_eq!(target.clipboard, expected);
        assert_eq!(state.storage.get_recording("take").unwrap().unwrap().text, Some(expected));
    }
    #[test]
    fn cancellation_waits_for_inflight_draft_and_revokes_all_later_effects() {
        let (_dir, state, take) = fixture();
        state.machine.lock().unwrap().state = DictationState::Recording { recording_id: "take".into(), audio_path: "take.wav".into() };
        struct BlockingTarget { entered: std::sync::mpsc::Sender<()>, release: std::sync::mpsc::Receiver<()>, copies: usize }
        impl WindowsApi for BlockingTarget {
            fn foreground_window(&mut self) -> Option<WindowTarget> { None }
            fn restore_foreground(&mut self, _: WindowTarget) -> Result<(), PlatformError> { panic!("draft must not focus") }
            fn write_clipboard(&mut self, _: &str) -> Result<(), PlatformError> { self.entered.send(()).unwrap(); self.release.recv().unwrap(); self.copies += 1; Ok(()) }
            fn send_ctrl_v(&mut self) -> Result<(), PlatformError> { panic!("draft must not paste") }
        }
        let (entered_tx, entered_rx) = std::sync::mpsc::channel(); let (release_tx, release_rx) = std::sync::mpsc::channel();
        let worker_state = state.clone(); let worker_take = take.clone();
        let worker = std::thread::spawn(move || {
            let mut target = BlockingTarget { entered: entered_tx, release: release_rx, copies: 0 };
            publish_draft(&worker_state, &worker_take, "in flight", &mut target);
            target.copies
        });
        entered_rx.recv_timeout(Duration::from_secs(2)).unwrap();
        let cancel_state = state.clone(); let (done_tx, done_rx) = std::sync::mpsc::channel();
        let canceller = std::thread::spawn(move || {
            let _gate = cancel_state.lifecycle.lock().unwrap(); abandon_live_session(&cancel_state); done_tx.send(()).unwrap();
        });
        assert!(done_rx.recv_timeout(Duration::from_millis(30)).is_err());
        release_tx.send(()).unwrap(); done_rx.recv_timeout(Duration::from_secs(2)).unwrap();
        canceller.join().unwrap(); assert_eq!(worker.join().unwrap(), 1);
        let mut target = Target { clipboard: "later external copy".into(), ..Default::default() };
        assert!(!publish_draft(&state, &take, "late", &mut target));
        assert_eq!(target.clipboard, "later external copy");
    }
    #[test]
    fn independent_engine_preferences_migrate_and_gate_unsupported_cloud() {
        let mut settings: AppSettings = serde_json::from_value(serde_json::json!({ "inputDevice": null, "shortcut":"Ctrl+Space", "autoPaste":true, "retentionDays":30, "streaming":true })).unwrap();
        assert!(streaming_wanted(&settings));
        settings.realtime_by_engine.insert("local:parakeet".into(), false);
        assert!(!streaming_wanted(&settings));
        settings.transcription_source = TranscriptionSource::Cloud; settings.cloud_provider = "xai".into();
        assert!(streaming_wanted(&settings));
        settings.realtime_by_engine.insert("cloud:xai".into(), false); assert!(!streaming_wanted(&settings));
        settings.cloud_provider = "openai".into(); settings.realtime_by_engine.insert("cloud:openai".into(), true);
        assert!(!streaming_wanted(&settings));
        assert_eq!(serde_json::from_value::<AppSettings>(serde_json::to_value(&settings).unwrap()).unwrap(), settings);
    }
}
#[cfg(test)]
mod ptt_settings_tests {
    use super::*;
    #[test]
    fn ptt_settings_migrate_and_persist_without_changing_shortcut() {
        let mut old = serde_json::to_value(AppSettings::default()).unwrap();
        old.as_object_mut().unwrap().remove("activationMode");
        let migrated: AppSettings = serde_json::from_value(old.clone()).unwrap();
        assert_eq!(serde_json::to_value(migrated).unwrap()["activationMode"], "toggle");
        old["activationMode"] = serde_json::json!("push_to_talk");
        old["shortcut"] = serde_json::json!("Alt+F8");
        let changed: AppSettings = serde_json::from_value(old).unwrap();
        let encoded = serde_json::to_value(changed).unwrap();
        assert_eq!(encoded["activationMode"], "push_to_talk");
        assert_eq!(encoded["shortcut"], "Alt+F8");
    }
}
