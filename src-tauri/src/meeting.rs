//! Meeting transcription: the microphone and the computer's output recorded
//! as two tracks, then transcribed and merged into one labelled transcript.
//!
//! Keeping the tracks apart is what makes the labels possible without any
//! speaker recognition: everything on the microphone is "me", everything the
//! computer played is "others". Both tracks are written to disk while the
//! meeting runs, so a crash loses at most the last buffer, and the transcript
//! is made once the meeting ends: the silence between turns is cut away by a
//! voice activity detector and each turn is decoded on its own.

use crate::audio::{self, AudioRecorder, CompletedRecording};
use crate::dictation::AppState;
use crate::engine::{EngineError, TARGET_SAMPLE_RATE};
use crate::loopback::LoopbackBackend;
use crate::storage::{Recording, RecordingKind, RecordingStatus, Storage};
use rubato::{Resampler, SincFixedIn, SincInterpolationParameters, SincInterpolationType, WindowFunction};
use serde::{Deserialize, Serialize};
use std::collections::{HashSet, VecDeque};
use std::fs;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Instant, SystemTime, UNIX_EPOCH};
use tauri::{AppHandle, Emitter, Manager, State};

/// Silero VAD v5, as published with sherpa-onnx.
const VAD_URL: &str = "https://github.com/k2-fsa/sherpa-onnx/releases/download/asr-models/silero_vad.onnx";
const VAD_BYTES: u64 = 643_854;
const VAD_DIR: &str = "silero-vad";
const VAD_FILE: &str = "silero_vad.onnx";

/// The longest turn handed to the recogniser in one piece. The engine refuses
/// anything over thirty seconds; the detector is asked to cut turns well
/// before that, and anything that still runs long is split here.
const MAX_CHUNK_SAMPLES: usize = TARGET_SAMPLE_RATE as usize * 25;

/// Audio read from each track per step, at 16 kHz.
const BLOCK_SAMPLES: usize = TARGET_SAMPLE_RATE as usize;

const CONSENT_SETTING: &str = "meeting_consent_acknowledged";

pub const STATUS_EVENT: &str = "meeting://status";

/// Shown in history for a meeting recovered after the app stopped mid-way.
pub const INTERRUPTED_MEETING_ERROR: &str =
    "The meeting recording was interrupted. Retry to transcribe what was saved.";

/// A track contained nothing but digital silence. On macOS that is what a
/// denied "System Audio Recording" permission looks like.
pub const WARNING_OTHERS_SILENT: &str = "others_silent";

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Speaker {
    Me,
    Others,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MeetingSegment {
    pub speaker: Speaker,
    pub start_ms: u64,
    pub end_ms: u64,
    pub text: String,
}

/// The two raw captures, kept until the meeting has been transcribed.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MeetingTracks {
    pub me: String,
    pub others: String,
    /// When each capture actually began, after the meeting's own start.
    pub me_offset_ms: u64,
    pub others_offset_ms: u64,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MeetingTranscript {
    pub segments: Vec<MeetingSegment>,
    #[serde(default)]
    pub tracks: Option<MeetingTracks>,
    #[serde(default)]
    pub warnings: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MeetingStatus {
    /// "idle", "recording" or "processing".
    pub phase: &'static str,
    pub meeting_id: Option<String>,
    pub started_at: Option<i64>,
    pub progress: Option<f32>,
    pub others_silent: bool,
    pub consent_acknowledged: bool,
}

struct ActiveMeeting {
    id: String,
    dir: PathBuf,
    started_at: i64,
    clock: Instant,
    mic: AudioRecorder,
    others: AudioRecorder,
    tracks: MeetingTracks,
    /// Whether the output track has carried a single non-zero sample.
    others_heard: Arc<AtomicBool>,
}

enum Phase {
    Idle,
    Recording(Box<ActiveMeeting>),
    Processing { id: String, progress: f32 },
}

/// Managed beside `AppState`; dictation and meetings run independently.
pub struct MeetingState {
    phase: Mutex<Phase>,
    cancel: Arc<AtomicBool>,
}

impl Default for MeetingState {
    fn default() -> Self {
        Self { phase: Mutex::new(Phase::Idle), cancel: Arc::new(AtomicBool::new(false)) }
    }
}

impl MeetingState {
    pub fn is_recording(&self) -> bool {
        self.phase.lock().is_ok_and(|phase| matches!(*phase, Phase::Recording(_)))
    }

    fn status(&self, storage: &Storage) -> MeetingStatus {
        let consent_acknowledged = storage
            .get_setting::<bool>(CONSENT_SETTING)
            .ok()
            .flatten()
            .unwrap_or(false);
        let phase = self.phase.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        match &*phase {
            Phase::Idle => MeetingStatus {
                phase: "idle",
                meeting_id: None,
                started_at: None,
                progress: None,
                others_silent: false,
                consent_acknowledged,
            },
            Phase::Recording(meeting) => MeetingStatus {
                phase: "recording",
                meeting_id: Some(meeting.id.clone()),
                started_at: Some(meeting.started_at),
                progress: None,
                // Only worth raising once there has been time to hear anyone.
                others_silent: meeting.clock.elapsed().as_secs() >= 20
                    && !meeting.others_heard.load(Ordering::Relaxed),
                consent_acknowledged,
            },
            Phase::Processing { id, progress } => MeetingStatus {
                phase: "processing",
                meeting_id: Some(id.clone()),
                started_at: None,
                progress: Some(*progress),
                others_silent: false,
                consent_acknowledged,
            },
        }
    }
}

fn epoch_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_millis() as i64)
        .unwrap_or_default()
}

fn emit_status(app: &AppHandle) {
    let (Some(meetings), Some(state)) = (app.try_state::<MeetingState>(), app.try_state::<AppState>()) else {
        return;
    };
    let status = meetings.status(&state.storage);
    let _ = app.emit(STATUS_EVENT, &status);
    crate::refresh_tray(app);
}

// ---------------------------------------------------------------------------
// Commands
// ---------------------------------------------------------------------------

#[tauri::command]
pub fn get_meeting_status(state: State<'_, AppState>, meetings: State<'_, MeetingState>) -> MeetingStatus {
    meetings.status(&state.storage)
}

/// Records that the user has read the notice about recording other people.
#[tauri::command]
pub fn acknowledge_meeting_consent(
    app: AppHandle,
    state: State<'_, AppState>,
    meetings: State<'_, MeetingState>,
) -> Result<MeetingStatus, String> {
    state
        .storage
        .set_setting(CONSENT_SETTING, &true)
        .map_err(|error| error.to_string())?;
    emit_status(&app);
    Ok(meetings.status(&state.storage))
}

#[tauri::command]
pub fn start_meeting(app: AppHandle) -> Result<MeetingStatus, String> {
    start_meeting_inner(&app)
}

pub fn start_meeting_inner(app: &AppHandle) -> Result<MeetingStatus, String> {
    let state = app.state::<AppState>();
    let meetings = app.state::<MeetingState>();
    if !meetings.status(&state.storage).consent_acknowledged {
        return Err("consent_required".into());
    }
    let settings = state.settings.read().map_err(|_| "settings lock poisoned")?.clone();
    if !state.engine.is_installed(&settings.model) {
        return Err("Download the transcription model before recording a meeting.".into());
    }
    let mut phase = meetings.phase.lock().map_err(|_| "meeting lock poisoned")?;
    if !matches!(*phase, Phase::Idle) {
        return Err("A meeting is already being recorded or transcribed.".into());
    }

    let devices = state.audio.list_devices().map_err(|error| error.to_string())?;
    let preferences = settings
        .microphones
        .clone()
        .unwrap_or_else(|| crate::microphones::migrate_legacy(settings.input_device.as_deref(), &devices));
    let device = crate::microphones::resolve_preference(&preferences, &devices)
        .map_err(|error| error.to_string())?
        .id
        .clone();

    let started_at = epoch_ms();
    let id = format!("meeting-{started_at:013}");
    let dir = state.storage.recordings_dir().join("meetings").join(&id);
    fs::create_dir_all(&dir).map_err(|error| error.to_string())?;
    let me_path = dir.join("me.wav");
    let others_path = dir.join("others.wav");

    let mic = AudioRecorder::new(&dir);
    let others = AudioRecorder::with_backend(&dir, Arc::new(LoopbackBackend));
    let others_heard = Arc::new(AtomicBool::new(false));
    let heard = others_heard.clone();
    others.set_stream_sink(Some(Arc::new(move |pcm: &[i16], _format| {
        if !heard.load(Ordering::Relaxed) && pcm.iter().any(|sample| *sample != 0) {
            heard.store(true, Ordering::Relaxed);
        }
    })));

    let clock = Instant::now();
    let fail = |error: String| {
        let _ = fs::remove_dir_all(&dir);
        error
    };
    mic.start_at(Some(&device), format!("{id}-me"), me_path.clone())
        .map_err(|error| fail(format!("Could not open the microphone: {error}")))?;
    let me_offset_ms = clock.elapsed().as_millis() as u64;
    if let Err(error) = others.start_at(None, format!("{id}-others"), others_path.clone()) {
        let _ = mic.cancel();
        return Err(fail(format!("Could not record system audio: {error}")));
    }
    let others_offset_ms = clock.elapsed().as_millis() as u64;

    let tracks = MeetingTracks {
        me: me_path.to_string_lossy().into_owned(),
        others: others_path.to_string_lossy().into_owned(),
        me_offset_ms,
        others_offset_ms,
    };
    let row = Recording {
        id: id.clone(),
        created_at: started_at,
        duration_ms: 0,
        status: RecordingStatus::Recording,
        text: None,
        model: Some(settings.model.clone()),
        audio_path: None,
        source_app: None,
        error: None,
        peaks: None,
        kind: RecordingKind::Meeting,
        meeting: Some(MeetingTranscript { segments: Vec::new(), tracks: Some(tracks.clone()), warnings: Vec::new() }),
    };
    if let Err(error) = state.storage.insert_recording(&row) {
        let _ = mic.cancel();
        let _ = others.cancel();
        return Err(fail(error.to_string()));
    }
    *phase = Phase::Recording(Box::new(ActiveMeeting {
        id,
        dir,
        started_at,
        clock,
        mic,
        others,
        tracks,
        others_heard,
    }));
    drop(phase);
    // Fetch the voice detector now, so the end of the meeting does not wait
    // on a download.
    let models = state.model_home().to_path_buf();
    std::thread::spawn(move || {
        let _ = ensure_vad_model(&models);
    });
    spawn_silence_watch(app.clone());
    emit_status(app);
    Ok(meetings.status(&state.storage))
}

/// Re-announces the status once the silence warning becomes meaningful.
fn spawn_silence_watch(app: AppHandle) {
    std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_secs(21));
        if app.try_state::<MeetingState>().is_some_and(|meetings| meetings.is_recording()) {
            emit_status(&app);
        }
    });
}

#[tauri::command]
pub fn stop_meeting(app: AppHandle) -> Result<MeetingStatus, String> {
    stop_meeting_inner(&app)
}

pub fn stop_meeting_inner(app: &AppHandle) -> Result<MeetingStatus, String> {
    let state = app.state::<AppState>();
    let meetings = app.state::<MeetingState>();
    let meeting = {
        let mut phase = meetings.phase.lock().map_err(|_| "meeting lock poisoned")?;
        let Phase::Recording(_) = &*phase else {
            return Err("No meeting is being recorded.".into());
        };
        let Phase::Recording(meeting) = std::mem::replace(&mut *phase, Phase::Idle) else {
            unreachable!()
        };
        *phase = Phase::Processing { id: meeting.id.clone(), progress: 0.0 };
        meeting
    };
    let duration_ms = meeting.clock.elapsed().as_millis() as u64;
    let me = meeting.mic.stop();
    let others = meeting.others.stop();
    let mut warnings = Vec::new();
    if !meeting.others_heard.load(Ordering::Relaxed) {
        warnings.push(WARNING_OTHERS_SILENT.to_owned());
    }
    let interrupted = [&me, &others]
        .iter()
        .filter_map(|result| match result {
            Ok(CompletedRecording { interrupted: Some(message), .. }) => Some(message.clone()),
            Err(error) => Some(error.to_string()),
            _ => None,
        })
        .collect::<Vec<_>>();
    let transcript = MeetingTranscript { segments: Vec::new(), tracks: Some(meeting.tracks.clone()), warnings };
    if let Ok(Some(mut row)) = state.storage.get_recording(&meeting.id) {
        row.status = RecordingStatus::Processing;
        row.duration_ms = duration_ms;
        row.meeting = Some(transcript.clone());
        row.error = (!interrupted.is_empty()).then(|| interrupted.join("; "));
        let _ = state.storage.update_recording(&row);
    }
    spawn_processing(app.clone(), meeting.id.clone());
    emit_status(app);
    Ok(meetings.status(&state.storage))
}

#[tauri::command]
pub fn cancel_meeting(app: AppHandle) -> Result<MeetingStatus, String> {
    let state = app.state::<AppState>();
    let meetings = app.state::<MeetingState>();
    {
        let mut phase = meetings.phase.lock().map_err(|_| "meeting lock poisoned")?;
        match std::mem::replace(&mut *phase, Phase::Idle) {
            Phase::Recording(meeting) => {
                let _ = meeting.mic.cancel();
                let _ = meeting.others.cancel();
                let _ = fs::remove_dir_all(&meeting.dir);
                let _ = state.storage.delete_recording(&meeting.id);
            }
            Phase::Processing { id, progress } => {
                // Transcription stops at the next turn; the tracks stay so it
                // can be retried from history.
                meetings.cancel.store(true, Ordering::SeqCst);
                *phase = Phase::Processing { id, progress };
            }
            Phase::Idle => {}
        }
    }
    emit_status(&app);
    Ok(meetings.status(&state.storage))
}

#[tauri::command(rename_all = "camelCase")]
pub fn retry_meeting(app: AppHandle, recording_id: String) -> Result<MeetingStatus, String> {
    let state = app.state::<AppState>();
    let meetings = app.state::<MeetingState>();
    let row = state
        .storage
        .get_recording(&recording_id)
        .map_err(|error| error.to_string())?
        .ok_or_else(|| "recording not found".to_owned())?;
    let tracks = row
        .meeting
        .as_ref()
        .and_then(|meeting| meeting.tracks.as_ref())
        .filter(|_| row.kind == RecordingKind::Meeting)
        .ok_or_else(|| "this meeting has no saved audio to transcribe".to_owned())?;
    if !Path::new(&tracks.me).is_file() && !Path::new(&tracks.others).is_file() {
        return Err("the meeting audio is missing".into());
    }
    {
        let mut phase = meetings.phase.lock().map_err(|_| "meeting lock poisoned")?;
        if !matches!(*phase, Phase::Idle) {
            return Err("A meeting is already being recorded or transcribed.".into());
        }
        *phase = Phase::Processing { id: recording_id.clone(), progress: 0.0 };
    }
    let _ = state.storage.update_status(&recording_id, RecordingStatus::Processing, None, None, None, None);
    spawn_processing(app.clone(), recording_id);
    emit_status(&app);
    Ok(meetings.status(&state.storage))
}

// ---------------------------------------------------------------------------
// Processing
// ---------------------------------------------------------------------------

fn spawn_processing(app: AppHandle, id: String) {
    std::thread::spawn(move || {
        let state = app.state::<AppState>().inner().clone();
        let meetings = app.state::<MeetingState>();
        meetings.cancel.store(false, Ordering::SeqCst);
        let cancel = meetings.cancel.clone();
        let progress_app = app.clone();
        let mut last_emit = Instant::now();
        let result = process_meeting(&state, &id, &cancel, &mut |progress| {
            if let Ok(mut phase) = progress_app.state::<MeetingState>().phase.lock()
                && let Phase::Processing { progress: current, .. } = &mut *phase
            {
                *current = progress;
            }
            if last_emit.elapsed().as_millis() >= 500 {
                last_emit = Instant::now();
                emit_status(&progress_app);
            }
        });
        if let Err(error) = result {
            let _ = state
                .storage
                .update_status(&id, RecordingStatus::Failed, None, None, None, Some(&error));
        }
        if let Ok(mut phase) = meetings.phase.lock() {
            *phase = Phase::Idle;
        }
        emit_status(&app);
        let _ = app.emit("meeting://finished", &id);
    });
}

fn process_meeting(
    state: &AppState,
    id: &str,
    cancel: &AtomicBool,
    progress: &mut dyn FnMut(f32),
) -> Result<(), String> {
    let mut row = state
        .storage
        .get_recording(id)
        .map_err(|error| error.to_string())?
        .ok_or("the meeting disappeared from history")?;
    let mut transcript = row.meeting.clone().unwrap_or_default();
    let tracks = transcript.tracks.clone().ok_or("the meeting has no audio tracks")?;
    let settings = state.settings.read().map_err(|_| "settings lock poisoned")?.clone();
    // Meetings are always transcribed on this computer, whatever source
    // dictation is set to use.
    if !state.engine.is_installed(&settings.model) {
        return Err("Meetings are transcribed on this computer. Download the local model first.".into());
    }
    let model = settings.model.clone();
    let vad_model = ensure_vad_model(state.model_home())?;

    let mix_path = state.storage.recordings_dir().join(format!("{id}.wav"));
    let mut sources = Vec::new();
    for (path, offset, speaker) in [
        (&tracks.me, tracks.me_offset_ms, Speaker::Me),
        (&tracks.others, tracks.others_offset_ms, Speaker::Others),
    ] {
        let path = Path::new(path);
        if !path.is_file() {
            continue;
        }
        let reader = TrackReader::open(path, offset).map_err(|error| error.to_string())?;
        let detector = SileroDetector::new(&vad_model)?;
        sources.push(TrackSource { speaker, reader, detector: Box::new(detector) });
    }
    if sources.is_empty() {
        return Err("the meeting audio is missing".into());
    }

    let engine = state.engine.clone();
    let mut recognise = |samples: &[f32]| -> Result<String, EngineError> {
        let text = engine.transcribe_audio_samples(&model, samples, cancel);
        state.touch_engine();
        text
    };
    let outcome = transcribe_tracks(sources, &mix_path, cancel, &mut recognise, progress)
        .map_err(|error| error.to_string())?;

    let vocabulary = state.storage.list_vocabulary().unwrap_or_default();
    let mut me = Vec::new();
    let mut others = Vec::new();
    for mut segment in outcome.segments {
        segment.text = crate::storage::apply_vocabulary_text(&segment.text, &vocabulary);
        match segment.speaker {
            Speaker::Me => me.push(segment),
            Speaker::Others => others.push(segment),
        }
    }
    transcript.segments = merge_segments(me, others);
    let labels = match crate::resolved_language(&settings) {
        "pl" => ("Ja", "Pozostali"),
        _ => ("Me", "Others"),
    };
    let text = format_transcript(&transcript.segments, labels);

    // The labelled transcript and the mixed recording replace the raw
    // tracks, which are only kept while there is still something to redo.
    transcript.tracks = None;
    row.status = RecordingStatus::Completed;
    row.text = Some(text);
    row.model = Some(model);
    row.audio_path = Some(mix_path.to_string_lossy().into_owned());
    row.peaks = Some(audio::quantize_peaks(&audio::downsample_peaks(&outcome.peaks)));
    row.error = None;
    row.meeting = Some(transcript);
    state.storage.update_recording(&row).map_err(|error| error.to_string())?;
    remove_tracks(&tracks);
    Ok(())
}

pub fn remove_tracks(tracks: &MeetingTracks) {
    for path in [&tracks.me, &tracks.others] {
        let path = Path::new(path);
        let _ = fs::remove_file(path);
        let _ = fs::remove_file(audio::part_path_for(path));
    }
    if let Some(dir) = Path::new(&tracks.me).parent() {
        // Only succeeds once the folder is empty, which is the point.
        let _ = fs::remove_dir(dir);
    }
}

/// Finalises tracks a crash left as `.part` files, so the meeting can be
/// transcribed from history. Runs once at startup, after storage has marked
/// the interrupted rows as failed.
pub fn recover_interrupted(storage: &Storage) {
    let Ok(rows) = storage.list_history(&crate::storage::HistoryQuery {
        search: None,
        status: Some(RecordingStatus::Failed),
    }) else {
        return;
    };
    for mut row in rows {
        if row.kind != RecordingKind::Meeting {
            continue;
        }
        let Some(tracks) = row.meeting.as_ref().and_then(|meeting| meeting.tracks.clone()) else {
            continue;
        };
        let mut recovered = false;
        for path in [&tracks.me, &tracks.others] {
            let path = Path::new(path);
            let part = audio::part_path_for(path);
            if !path.exists() && part.is_file() && repair_wav_header(&part).is_ok() && fs::rename(&part, path).is_ok() {
                recovered = true;
            }
        }
        if recovered || Path::new(&tracks.me).is_file() || Path::new(&tracks.others).is_file() {
            if row.duration_ms == 0 {
                row.duration_ms = wav_duration_ms(Path::new(&tracks.me))
                    .max(wav_duration_ms(Path::new(&tracks.others)));
            }
            row.error = Some(INTERRUPTED_MEETING_ERROR.into());
            let _ = storage.update_recording(&row);
        }
    }
}

fn wav_duration_ms(path: &Path) -> u64 {
    hound::WavReader::open(path)
        .map(|reader| u64::from(reader.duration()) * 1000 / u64::from(reader.spec().sample_rate.max(1)))
        .unwrap_or(0)
}

/// Rewrites the RIFF and data chunk sizes of a WAV whose writer never got to
/// finalise it, from the length the file actually has.
pub fn repair_wav_header(path: &Path) -> std::io::Result<()> {
    let mut file = fs::OpenOptions::new().read(true).write(true).open(path)?;
    let length = file.metadata()?.len();
    let mut riff = [0_u8; 12];
    file.read_exact(&mut riff)?;
    if &riff[0..4] != b"RIFF" || &riff[8..12] != b"WAVE" {
        return Err(std::io::Error::new(std::io::ErrorKind::InvalidData, "not a WAV file"));
    }
    let mut offset = 12_u64;
    let mut block_align = 1_u64;
    while offset + 8 <= length {
        file.seek(SeekFrom::Start(offset))?;
        let mut header = [0_u8; 8];
        file.read_exact(&mut header)?;
        let size = u64::from(u32::from_le_bytes([header[4], header[5], header[6], header[7]]));
        if &header[0..4] == b"fmt " {
            let mut fmt = [0_u8; 16];
            file.read_exact(&mut fmt)?;
            block_align = u64::from(u16::from_le_bytes([fmt[12], fmt[13]])).max(1);
        }
        if &header[0..4] == b"data" {
            let available = length - offset - 8;
            let data = available - available % block_align;
            file.seek(SeekFrom::Start(offset + 4))?;
            file.write_all(&(data.min(u64::from(u32::MAX)) as u32).to_le_bytes())?;
            let riff_size = (offset + 8 + data - 8).min(u64::from(u32::MAX)) as u32;
            file.seek(SeekFrom::Start(4))?;
            file.write_all(&riff_size.to_le_bytes())?;
            file.set_len(offset + 8 + data)?;
            return file.sync_all();
        }
        offset += 8 + size + (size & 1);
    }
    Err(std::io::Error::new(std::io::ErrorKind::InvalidData, "no data chunk"))
}

// ---------------------------------------------------------------------------
// Voice activity detection
// ---------------------------------------------------------------------------

pub fn vad_model_path(models: &Path) -> PathBuf {
    models.join(VAD_DIR).join(VAD_FILE)
}

/// Downloads the voice detector on first use. It is small enough not to be
/// worth a place in the model catalogue.
pub fn ensure_vad_model(models: &Path) -> Result<PathBuf, String> {
    let path = vad_model_path(models);
    if path.metadata().is_ok_and(|meta| meta.len() == VAD_BYTES) {
        return Ok(path);
    }
    static DOWNLOAD: Mutex<()> = Mutex::new(());
    let _guard = DOWNLOAD.lock().map_err(|_| "download lock poisoned")?;
    if path.metadata().is_ok_and(|meta| meta.len() == VAD_BYTES) {
        return Ok(path);
    }
    let dir = path.parent().ok_or("invalid model directory")?;
    fs::create_dir_all(dir).map_err(|error| error.to_string())?;
    let part = dir.join(format!("{VAD_FILE}.part"));
    let response = ureq::get(VAD_URL)
        .call()
        .map_err(|error| format!("Could not download the voice detector: {error}"))?;
    let mut bytes = Vec::with_capacity(VAD_BYTES as usize);
    response
        .into_reader()
        .take(VAD_BYTES * 2)
        .read_to_end(&mut bytes)
        .map_err(|error| format!("Could not download the voice detector: {error}"))?;
    if bytes.len() as u64 != VAD_BYTES {
        return Err("The voice detector download was incomplete.".into());
    }
    fs::write(&part, &bytes).map_err(|error| error.to_string())?;
    fs::rename(&part, &path).map_err(|error| error.to_string())?;
    Ok(path)
}

/// Splits a 16 kHz stream into the stretches where someone is speaking.
pub trait SpeechDetector {
    fn accept(&mut self, samples: &[f32]);
    fn flush(&mut self);
    /// The next finished turn: where it starts, in samples, and its audio.
    fn pop(&mut self) -> Option<(usize, Vec<f32>)>;
}

struct SileroDetector {
    vad: sherpa_rs::silero_vad::SileroVad,
}

impl SileroDetector {
    fn new(model: &Path) -> Result<Self, String> {
        if !model.is_file() {
            return Err("the voice detector model is missing".into());
        }
        let config = sherpa_rs::silero_vad::SileroVadConfig {
            model: model.to_string_lossy().into_owned(),
            threshold: 0.5,
            min_silence_duration: 0.5,
            min_speech_duration: 0.25,
            max_speech_duration: 20.0,
            sample_rate: TARGET_SAMPLE_RATE,
            window_size: 512,
            provider: Some("cpu".into()),
            num_threads: Some(1),
            debug: false,
        };
        let vad = sherpa_rs::silero_vad::SileroVad::new(config, 60.0)
            .map_err(|error| format!("could not start the voice detector: {error}"))?;
        Ok(Self { vad })
    }
}

impl SpeechDetector for SileroDetector {
    fn accept(&mut self, samples: &[f32]) {
        self.vad.accept_waveform(samples.to_vec());
    }

    fn flush(&mut self) {
        self.vad.flush();
    }

    fn pop(&mut self) -> Option<(usize, Vec<f32>)> {
        if self.vad.is_empty() {
            return None;
        }
        let segment = self.vad.front();
        self.vad.pop();
        Some((segment.start.max(0) as usize, segment.samples))
    }
}

// ---------------------------------------------------------------------------
// Reading tracks
// ---------------------------------------------------------------------------

/// Streams a WAV as 16 kHz mono, preceded by `offset_ms` of silence, without
/// holding the whole meeting in memory.
pub struct TrackReader {
    reader: hound::WavReader<std::io::BufReader<fs::File>>,
    channels: usize,
    resampler: Option<SincFixedIn<f32>>,
    pending: Vec<f32>,
    ready: VecDeque<f32>,
    lead_in: usize,
    total_frames: u64,
    read_frames: u64,
    exhausted: bool,
}

const RESAMPLE_CHUNK: usize = 4_096;

impl TrackReader {
    pub fn open(path: &Path, offset_ms: u64) -> Result<Self, hound::Error> {
        let reader = hound::WavReader::open(path)?;
        let spec = reader.spec();
        let resampler = if spec.sample_rate == TARGET_SAMPLE_RATE {
            None
        } else {
            let parameters = SincInterpolationParameters {
                sinc_len: 128,
                f_cutoff: 0.95,
                interpolation: SincInterpolationType::Linear,
                oversampling_factor: 128,
                window: WindowFunction::BlackmanHarris2,
            };
            Some(
                SincFixedIn::<f32>::new(
                    f64::from(TARGET_SAMPLE_RATE) / f64::from(spec.sample_rate),
                    1.0,
                    parameters,
                    RESAMPLE_CHUNK,
                    1,
                )
                .map_err(|error| hound::Error::IoError(std::io::Error::other(error.to_string())))?,
            )
        };
        Ok(Self {
            total_frames: u64::from(reader.duration()),
            channels: usize::from(spec.channels.max(1)),
            reader,
            resampler,
            pending: Vec::new(),
            ready: VecDeque::new(),
            lead_in: (offset_ms * u64::from(TARGET_SAMPLE_RATE) / 1000) as usize,
            read_frames: 0,
            exhausted: false,
        })
    }

    /// How far through the file reading has got, `0.0..=1.0`.
    pub fn progress(&self) -> f32 {
        if self.total_frames == 0 { 1.0 } else { (self.read_frames as f32 / self.total_frames as f32).min(1.0) }
    }

    pub fn is_done(&self) -> bool {
        self.exhausted && self.ready.is_empty() && self.lead_in == 0
    }

    /// Up to `count` samples; fewer only once the track has run out.
    pub fn next_block(&mut self, count: usize) -> Result<Vec<f32>, hound::Error> {
        let mut out = Vec::with_capacity(count);
        let silence = self.lead_in.min(count);
        out.resize(silence, 0.0);
        self.lead_in -= silence;
        while out.len() < count {
            if let Some(sample) = self.ready.pop_front() {
                out.push(sample);
                continue;
            }
            if self.exhausted {
                break;
            }
            self.fill()?;
        }
        Ok(out)
    }

    fn fill(&mut self) -> Result<(), hound::Error> {
        let wanted = RESAMPLE_CHUNK * self.channels;
        let mut frame = Vec::with_capacity(self.channels);
        let mut samples = self.reader.samples::<i16>();
        let mut mono = Vec::with_capacity(RESAMPLE_CHUNK);
        for _ in 0..wanted {
            match samples.next() {
                Some(sample) => {
                    frame.push(f32::from(sample?) / 32768.0);
                    if frame.len() == self.channels {
                        mono.push(frame.iter().sum::<f32>() / self.channels as f32);
                        frame.clear();
                    }
                }
                None => {
                    self.exhausted = true;
                    break;
                }
            }
        }
        self.read_frames += mono.len() as u64;
        match &mut self.resampler {
            None => self.ready.extend(mono),
            Some(resampler) => {
                self.pending.extend(mono);
                let resample_error = |error: rubato::ResampleError| hound::Error::IoError(std::io::Error::other(error.to_string()));
                while self.pending.len() >= RESAMPLE_CHUNK {
                    let chunk: Vec<f32> = self.pending.drain(..RESAMPLE_CHUNK).collect();
                    let waves = resampler.process(&[chunk], None).map_err(resample_error)?;
                    if let Some(channel) = waves.into_iter().next() {
                        self.ready.extend(channel);
                    }
                }
                if self.exhausted && !self.pending.is_empty() {
                    let rest = std::mem::take(&mut self.pending);
                    let waves = resampler.process_partial(Some(&[rest]), None).map_err(resample_error)?;
                    if let Some(channel) = waves.into_iter().next() {
                        self.ready.extend(channel);
                    }
                }
            }
        }
        Ok(())
    }
}

pub struct TrackSource {
    pub speaker: Speaker,
    pub reader: TrackReader,
    pub detector: Box<dyn SpeechDetector>,
}

#[derive(Debug)]
pub struct TranscribedTracks {
    pub segments: Vec<MeetingSegment>,
    /// Loudness per block of the mixed recording, before downsampling.
    pub peaks: Vec<f32>,
}

#[derive(Debug, thiserror::Error)]
pub enum ProcessError {
    #[error("could not read the meeting audio: {0}")]
    Audio(#[from] hound::Error),
    #[error("could not write the meeting audio: {0}")]
    Io(#[from] std::io::Error),
    #[error("{0}")]
    Engine(#[from] EngineError),
    #[error("transcription was cancelled")]
    Cancelled,
}

/// Walks every track in step: each block feeds its own detector, each
/// finished turn is transcribed, and the blocks are summed into one mono
/// recording for playback.
pub fn transcribe_tracks(
    mut sources: Vec<TrackSource>,
    mix_path: &Path,
    cancel: &AtomicBool,
    recognise: &mut dyn FnMut(&[f32]) -> Result<String, EngineError>,
    progress: &mut dyn FnMut(f32),
) -> Result<TranscribedTracks, ProcessError> {
    let part = audio::part_path_for(mix_path);
    let spec = hound::WavSpec {
        channels: 1,
        sample_rate: TARGET_SAMPLE_RATE,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };
    let mut mix = hound::WavWriter::create(&part, spec)?;
    let mut segments = Vec::new();
    let mut peaks = Vec::new();
    let mut flushed = vec![false; sources.len()];
    let mut histories: Vec<TrackHistory> = sources.iter().map(|_| TrackHistory::default()).collect();
    let result = (|| {
        loop {
            if cancel.load(Ordering::SeqCst) {
                return Err(ProcessError::Cancelled);
            }
            let mut mixed = vec![0.0_f32; BLOCK_SAMPLES];
            let mut longest = 0;
            for ((source, flushed), history) in sources.iter_mut().zip(flushed.iter_mut()).zip(histories.iter_mut()) {
                if *flushed {
                    continue;
                }
                let block = source.reader.next_block(BLOCK_SAMPLES)?;
                longest = longest.max(block.len());
                for (sum, sample) in mixed.iter_mut().zip(&block) {
                    *sum += sample;
                }
                history.push(&block);
                source.detector.accept(&block);
                if source.reader.is_done() {
                    // The last turn has no silence after it to end it.
                    source.detector.flush();
                    *flushed = true;
                }
                drain_turns(source, history, recognise, &mut segments, cancel)?;
            }
            mixed.truncate(longest);
            peaks.push(mixed.iter().fold(0.0_f32, |peak, sample| peak.max(sample.abs())).min(1.0));
            for sample in &mixed {
                mix.write_sample((sample.clamp(-1.0, 1.0) * 32767.0).round() as i16)?;
            }
            let done = sources.iter().all(|source| source.reader.is_done());
            let fraction = sources.iter().map(|source| source.reader.progress()).sum::<f32>() / sources.len() as f32;
            progress(fraction);
            if done || longest == 0 {
                break;
            }
        }
        Ok(())
    })();
    match result.and_then(|()| mix.finalize().map_err(ProcessError::from)) {
        Ok(()) => {
            if mix_path.exists() {
                fs::remove_file(mix_path)?;
            }
            audio::finalize_atomic(&part, mix_path).map_err(|error| ProcessError::Io(std::io::Error::other(error.to_string())))?;
            Ok(TranscribedTracks { segments, peaks })
        }
        Err(error) => {
            let _ = fs::remove_file(&part);
            Err(error)
        }
    }
}

/// Audio kept from before a turn starts. The detector only declares speech
/// once it is sure, which is a syllable or two in: without this the first
/// words of every turn ("Good morning", "Yes, the") were lost.
const PRE_ROLL_SAMPLES: usize = TARGET_SAMPLE_RATE as usize * 4 / 5;
/// And after it ends, for the trailing consonant.
const POST_ROLL_SAMPLES: usize = TARGET_SAMPLE_RATE as usize / 5;
/// How much of a track is remembered; a turn is at most twenty seconds and is
/// reported within a second of ending.
const HISTORY_SAMPLES: usize = TARGET_SAMPLE_RATE as usize * 40;

/// The recent audio of one track, so a turn can be cut with some margin.
#[derive(Default)]
struct TrackHistory {
    samples: VecDeque<f32>,
    /// Absolute position of `samples[0]` in the track.
    first: usize,
    /// Where the previous turn ended; a margin never reaches back into it.
    last_end: usize,
}

impl TrackHistory {
    fn push(&mut self, block: &[f32]) {
        self.samples.extend(block);
        let excess = self.samples.len().saturating_sub(HISTORY_SAMPLES);
        self.samples.drain(..excess);
        self.first += excess;
    }

    /// The turn at `start` widened by the margins, as far as the remembered
    /// audio allows; the detector's own cut when the turn is no longer here.
    fn widen(&mut self, start: usize, turn: Vec<f32>) -> (usize, Vec<f32>) {
        let end = start + turn.len();
        let available = self.first + self.samples.len();
        let from = start.saturating_sub(PRE_ROLL_SAMPLES).max(self.last_end).max(self.first);
        let to = (end + POST_ROLL_SAMPLES).min(available);
        if from > start || to < end {
            self.last_end = end;
            return (start, turn);
        }
        self.last_end = to;
        (from, self.samples.range(from - self.first..to - self.first).copied().collect())
    }
}

fn drain_turns(
    source: &mut TrackSource,
    history: &mut TrackHistory,
    recognise: &mut dyn FnMut(&[f32]) -> Result<String, EngineError>,
    segments: &mut Vec<MeetingSegment>,
    cancel: &AtomicBool,
) -> Result<(), ProcessError> {
    while let Some((start, samples)) = source.detector.pop() {
        let (start, samples) = history.widen(start, samples);
        for (index, piece) in samples.chunks(MAX_CHUNK_SAMPLES).enumerate() {
            if cancel.load(Ordering::SeqCst) {
                return Err(ProcessError::Cancelled);
            }
            let text = recognise(piece)?;
            let text = text.trim();
            if text.is_empty() {
                continue;
            }
            let first = start + index * MAX_CHUNK_SAMPLES;
            segments.push(MeetingSegment {
                speaker: source.speaker,
                start_ms: samples_to_ms(first),
                end_ms: samples_to_ms(first + piece.len()),
                text: text.to_owned(),
            });
        }
    }
    Ok(())
}

fn samples_to_ms(samples: usize) -> u64 {
    samples as u64 * 1000 / u64::from(TARGET_SAMPLE_RATE)
}

// ---------------------------------------------------------------------------
// Merging
// ---------------------------------------------------------------------------

/// How far apart a microphone turn and an output turn may be and still be the
/// same words heard twice: the speakers' sound reaching the microphone.
const ECHO_SLACK_MS: u64 = 1_000;

/// Share of a microphone turn's words that must also be in the overlapping
/// output turns for it to count as echo.
const ECHO_OVERLAP: f32 = 0.6;

fn words(text: &str) -> Vec<String> {
    text.split(|character: char| !character.is_alphanumeric())
        .filter(|word| !word.is_empty())
        .map(str::to_lowercase)
        .collect()
}

/// Whether a microphone turn only repeats what the output was playing, which
/// happens when the meeting is heard through speakers instead of headphones.
pub fn is_echo(me: &MeetingSegment, others: &[MeetingSegment]) -> bool {
    let mine = words(&me.text);
    if mine.is_empty() {
        return false;
    }
    let heard: HashSet<String> = others
        .iter()
        .filter(|other| {
            other.start_ms <= me.end_ms + ECHO_SLACK_MS && me.start_ms <= other.end_ms + ECHO_SLACK_MS
        })
        .flat_map(|other| words(&other.text))
        .collect();
    if heard.is_empty() {
        return false;
    }
    let shared = mine.iter().filter(|word| heard.contains(*word)).count();
    shared as f32 / mine.len() as f32 >= ECHO_OVERLAP
}

/// Interleaves both sides by start time, dropping microphone turns that are
/// only an echo of the other side.
pub fn merge_segments(me: Vec<MeetingSegment>, others: Vec<MeetingSegment>) -> Vec<MeetingSegment> {
    let mut merged: Vec<MeetingSegment> = me.into_iter().filter(|segment| !is_echo(segment, &others)).collect();
    merged.extend(others);
    merged.sort_by_key(|segment| (segment.start_ms, segment.speaker == Speaker::Me, segment.end_ms));
    merged
}

/// The transcript as plain text, one paragraph per change of speaker.
pub fn format_transcript(segments: &[MeetingSegment], labels: (&str, &str)) -> String {
    let mut paragraphs: Vec<(Speaker, String)> = Vec::new();
    for segment in segments {
        match paragraphs.last_mut() {
            Some((speaker, text)) if *speaker == segment.speaker => {
                text.push(' ');
                text.push_str(&segment.text);
            }
            _ => paragraphs.push((segment.speaker, segment.text.clone())),
        }
    }
    paragraphs
        .into_iter()
        .map(|(speaker, text)| {
            let label = if speaker == Speaker::Me { labels.0 } else { labels.1 };
            format!("{label}: {text}")
        })
        .collect::<Vec<_>>()
        .join("\n\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn segment(speaker: Speaker, start_ms: u64, end_ms: u64, text: &str) -> MeetingSegment {
        MeetingSegment { speaker, start_ms, end_ms, text: text.into() }
    }

    #[test]
    fn segments_are_interleaved_by_start_time() {
        let merged = merge_segments(
            vec![segment(Speaker::Me, 0, 1_000, "cześć"), segment(Speaker::Me, 5_000, 6_000, "jasne")],
            vec![segment(Speaker::Others, 2_000, 4_000, "dzień dobry wszystkim")],
        );

        let order: Vec<_> = merged.iter().map(|segment| segment.text.as_str()).collect();
        assert_eq!(order, ["cześć", "dzień dobry wszystkim", "jasne"]);
    }

    #[test]
    fn a_microphone_turn_that_repeats_the_speakers_is_dropped() {
        let merged = merge_segments(
            vec![
                segment(Speaker::Me, 2_300, 4_100, "Dzień dobry, wszystkim!"),
                segment(Speaker::Me, 4_500, 5_000, "Mam pytanie o budżet"),
            ],
            vec![segment(Speaker::Others, 2_000, 4_000, "dzień dobry wszystkim")],
        );

        assert_eq!(merged.len(), 2);
        assert_eq!(merged[0].speaker, Speaker::Others);
        assert_eq!(merged[1].text, "Mam pytanie o budżet");
    }

    #[test]
    fn the_same_words_far_apart_in_time_are_not_echo() {
        let me = segment(Speaker::Me, 60_000, 61_000, "dzień dobry");
        let others = [segment(Speaker::Others, 0, 1_000, "dzień dobry")];

        assert!(!is_echo(&me, &others));
    }

    #[test]
    fn the_transcript_text_joins_consecutive_turns_of_one_speaker() {
        let text = format_transcript(
            &[
                segment(Speaker::Me, 0, 1, "Cześć."),
                segment(Speaker::Me, 2, 3, "Zaczynamy?"),
                segment(Speaker::Others, 4, 5, "Tak."),
            ],
            ("Me", "Others"),
        );

        assert_eq!(text, "Me: Cześć. Zaczynamy?\n\nOthers: Tak.");
    }

    fn write_wav(path: &Path, rate: u32, channels: u16, samples: &[i16]) {
        let spec = hound::WavSpec { channels, sample_rate: rate, bits_per_sample: 16, sample_format: hound::SampleFormat::Int };
        let mut writer = hound::WavWriter::create(path, spec).unwrap();
        for sample in samples {
            writer.write_sample(*sample).unwrap();
        }
        writer.finalize().unwrap();
    }

    #[test]
    fn a_track_cut_off_by_a_crash_is_readable_after_repair() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("me.wav.part");
        {
            let spec = hound::WavSpec { channels: 2, sample_rate: 48_000, bits_per_sample: 16, sample_format: hound::SampleFormat::Int };
            let mut writer = hound::WavWriter::create(&path, spec).unwrap();
            for sample in 0..9_600_i16 {
                writer.write_sample(sample).unwrap();
            }
            writer.flush().unwrap();
            // The writer is never finalised, as in a crash.
            std::mem::forget(writer);
        }
        // A torn last frame, as a crash mid-write leaves it.
        fs::OpenOptions::new().append(true).open(&path).unwrap().write_all(&[1, 2, 3]).unwrap();

        repair_wav_header(&path).unwrap();

        let mut reader = hound::WavReader::open(&path).unwrap();
        assert_eq!(reader.duration(), 4_800);
        let samples: Vec<i16> = reader.samples::<i16>().collect::<Result<_, _>>().unwrap();
        assert_eq!(samples.len(), 9_600);
        assert_eq!(samples[9_599], 9_599);
    }

    #[test]
    fn a_track_is_read_as_16k_mono_after_its_offset() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("others.wav");
        // One second of 48 kHz stereo.
        write_wav(&path, 48_000, 2, &vec![8_000; 96_000]);

        let mut reader = TrackReader::open(&path, 500).unwrap();
        let mut all = Vec::new();
        while !reader.is_done() {
            let block = reader.next_block(4_000).unwrap();
            if block.is_empty() {
                break;
            }
            all.extend(block);
        }

        // Half a second of lead-in, then about a second of audio.
        assert!(all[..8_000].iter().all(|sample| *sample == 0.0));
        let drift = (all.len() as i64 - 24_000).abs();
        assert!(drift < 400, "got {} samples", all.len());
        assert!((all[16_000] - 8_000.0 / 32_768.0).abs() < 0.01);
        assert_eq!(reader.progress(), 1.0);
    }

    /// Reports every block above a threshold as its own turn.
    struct LoudBlocks {
        position: usize,
        turns: VecDeque<(usize, Vec<f32>)>,
    }

    impl SpeechDetector for LoudBlocks {
        fn accept(&mut self, samples: &[f32]) {
            if samples.iter().any(|sample| sample.abs() > 0.1) {
                self.turns.push_back((self.position, samples.to_vec()));
            }
            self.position += samples.len();
        }
        fn flush(&mut self) {}
        fn pop(&mut self) -> Option<(usize, Vec<f32>)> {
            self.turns.pop_front()
        }
    }

    #[test]
    fn both_tracks_are_transcribed_on_one_clock_and_mixed() {
        let temp = tempfile::tempdir().unwrap();
        let me = temp.path().join("me.wav");
        let others = temp.path().join("others.wav");
        // Me: a loud first second, then quiet. Others: quiet, then loud.
        let mut mine = vec![10_000_i16; 16_000];
        mine.extend(vec![0; 16_000]);
        let mut theirs = vec![0_i16; 16_000];
        theirs.extend(vec![-10_000; 16_000]);
        write_wav(&me, 16_000, 1, &mine);
        write_wav(&others, 16_000, 1, &theirs);

        let sources = vec![
            TrackSource {
                speaker: Speaker::Me,
                reader: TrackReader::open(&me, 0).unwrap(),
                detector: Box::new(LoudBlocks { position: 0, turns: VecDeque::new() }),
            },
            TrackSource {
                speaker: Speaker::Others,
                reader: TrackReader::open(&others, 0).unwrap(),
                detector: Box::new(LoudBlocks { position: 0, turns: VecDeque::new() }),
            },
        ];
        let mix = temp.path().join("meeting.wav");
        let cancel = AtomicBool::new(false);
        let mut calls = 0;
        let mut recognise = |samples: &[f32]| {
            calls += 1;
            let loudest = samples.iter().copied().fold(0.0_f32, |a, b| if b.abs() > a.abs() { b } else { a });
            Ok::<_, EngineError>(if loudest > 0.0 { "ja mówię".to_owned() } else { "oni mówią".to_owned() })
        };
        let mut last_progress = 0.0;
        let result = transcribe_tracks(sources, &mix, &cancel, &mut recognise, &mut |value| last_progress = value).unwrap();

        assert_eq!(calls, 2);
        let merged = merge_segments(
            result.segments.iter().filter(|s| s.speaker == Speaker::Me).cloned().collect(),
            result.segments.iter().filter(|s| s.speaker == Speaker::Others).cloned().collect(),
        );
        assert_eq!(
            merged,
            vec![
                // The second turn gains its pre-roll. The first is reported
                // before any audio after it exists, so it has no post-roll.
                segment(Speaker::Me, 0, 1_000, "ja mówię"),
                segment(Speaker::Others, samples_to_ms(16_000 - PRE_ROLL_SAMPLES), 2_000, "oni mówią"),
            ]
        );
        assert_eq!(last_progress, 1.0);
        let reader = hound::WavReader::open(&mix).unwrap();
        assert_eq!(reader.spec().sample_rate, 16_000);
        assert_eq!(reader.duration(), 32_000);
        assert!(!audio::part_path_for(&mix).exists());
    }

    #[test]
    fn cancelling_leaves_no_half_written_mix() {
        let temp = tempfile::tempdir().unwrap();
        let me = temp.path().join("me.wav");
        write_wav(&me, 16_000, 1, &vec![10_000; 32_000]);
        let sources = vec![TrackSource {
            speaker: Speaker::Me,
            reader: TrackReader::open(&me, 0).unwrap(),
            detector: Box::new(LoudBlocks { position: 0, turns: VecDeque::new() }),
        }];
        let mix = temp.path().join("meeting.wav");
        let cancel = AtomicBool::new(true);

        let error = transcribe_tracks(sources, &mix, &cancel, &mut |_| Ok::<_, EngineError>(String::new()), &mut |_| {}).unwrap_err();

        assert!(matches!(error, ProcessError::Cancelled));
        assert!(!mix.exists());
        assert!(!audio::part_path_for(&mix).exists());
    }

    /// The whole meeting pipeline on a real model, a real voice detector and
    /// two spoken tracks, the second starting after the first has finished.
    ///
    /// ```text
    /// LOQUARA_TEST_MODELS=<dir containing parakeet/>
    /// LOQUARA_TEST_ME=<a .wav> LOQUARA_TEST_OTHERS=<a .wav>
    ///   cargo test --lib meeting::tests::transcribes -- --ignored --nocapture
    /// ```
    #[test]
    #[ignore = "needs a downloaded model, two recordings and the network"]
    fn transcribes_a_real_meeting() {
        let models = PathBuf::from(std::env::var("LOQUARA_TEST_MODELS").expect("LOQUARA_TEST_MODELS"));
        let me = PathBuf::from(std::env::var("LOQUARA_TEST_ME").expect("LOQUARA_TEST_ME"));
        let others = PathBuf::from(std::env::var("LOQUARA_TEST_OTHERS").expect("LOQUARA_TEST_OTHERS"));
        let temp = tempfile::tempdir().unwrap();
        let vad = ensure_vad_model(temp.path()).unwrap();
        let engine = crate::engine::Engine::new(&models);
        let me_seconds = wav_duration_ms(&me);
        let others_offset = me_seconds + 1_000;
        let sources = vec![
            TrackSource {
                speaker: Speaker::Me,
                reader: TrackReader::open(&me, 0).unwrap(),
                detector: Box::new(SileroDetector::new(&vad).unwrap()),
            },
            TrackSource {
                speaker: Speaker::Others,
                reader: TrackReader::open(&others, others_offset).unwrap(),
                detector: Box::new(SileroDetector::new(&vad).unwrap()),
            },
        ];
        let cancel = AtomicBool::new(false);
        let mix = temp.path().join("meeting.wav");
        let started = Instant::now();
        let result = transcribe_tracks(
            sources,
            &mix,
            &cancel,
            &mut |samples| engine.transcribe_audio_samples("parakeet", samples, &cancel),
            &mut |_| {},
        )
        .unwrap();
        let (mine, theirs): (Vec<_>, Vec<_>) = result.segments.into_iter().partition(|segment| segment.speaker == Speaker::Me);
        let merged = merge_segments(mine, theirs);
        println!("{:?} on {:?}", started.elapsed(), engine.accelerator());
        println!("{}", format_transcript(&merged, ("Me", "Others")));
        for segment in &merged {
            println!("{:?} {}-{} ms", segment.speaker, segment.start_ms, segment.end_ms);
        }

        assert!(merged.len() >= 2);
        assert_eq!(merged[0].speaker, Speaker::Me);
        assert_eq!(merged.last().unwrap().speaker, Speaker::Others);
        let first_other = merged.iter().find(|segment| segment.speaker == Speaker::Others).unwrap();
        assert!(first_other.start_ms + 300 >= others_offset, "the offset has to carry into the timestamps");
        assert!(format_transcript(&merged, ("Me", "Others")).to_lowercase().contains("budget"));
        assert!(wav_duration_ms(&mix) >= others_offset);
    }

    #[test]
    fn a_turn_is_widened_without_reaching_into_the_previous_one() {
        let mut history = TrackHistory::default();
        let track: Vec<f32> = (0..HISTORY_SAMPLES + 16_000).map(|index| index as f32).collect();
        history.push(&track);
        let first = history.first;
        assert_eq!(first, 16_000, "only the last forty seconds are kept");

        let start = first + 32_000;
        let (from, audio) = history.widen(start, track[start..start + 8_000].to_vec());
        assert_eq!(from, start - PRE_ROLL_SAMPLES);
        assert_eq!(audio.len(), PRE_ROLL_SAMPLES + 8_000 + POST_ROLL_SAMPLES);
        assert_eq!(audio[0], from as f32);

        // The next turn starts right after: its margin stops at the last one.
        let next = start + 8_000 + POST_ROLL_SAMPLES + 1_000;
        let (from, _) = history.widen(next, track[next..next + 1_000].to_vec());
        assert_eq!(from, start + 8_000 + POST_ROLL_SAMPLES);

        // A turn that has already scrolled out keeps the detector's cut.
        let (from, audio) = history.widen(100, vec![0.5; 10]);
        assert_eq!((from, audio), (100, vec![0.5; 10]));
    }

    #[test]
    fn transcript_without_tracks_round_trips_through_json() {
        let transcript = MeetingTranscript {
            segments: vec![segment(Speaker::Others, 1, 2, "x")],
            tracks: None,
            warnings: vec![WARNING_OTHERS_SILENT.into()],
        };
        let json = serde_json::to_string(&transcript).unwrap();
        assert!(json.contains("\"speaker\":\"others\""));
        assert_eq!(serde_json::from_str::<MeetingTranscript>(&json).unwrap(), transcript);
    }
}
