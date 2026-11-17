//! Local transcription, running inside this process.
//!
//! Loquara used to shell out to a long-lived Python worker, which meant every
//! machine needed Python, PyTorch and a CUDA build matched to its driver
//! before the app could transcribe a word. This engine loads the same models
//! through ONNX Runtime instead: no interpreter, no framework, no vendor
//! toolkit, and a model that is a plain file rather than a gated download.
//!
//! The model stays resident between dictations. Loading costs several seconds
//! and decoding costs a fraction of the audio's length, so the difference
//! between keeping it and reloading it is the difference between a pause you
//! do not notice and one you do.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;

use parakeet_rs::{ExecutionConfig, ParakeetTDT, Transcriber};
use rubato::{Resampler, SincFixedIn, SincInterpolationParameters, SincInterpolationType, WindowFunction};

/// Every model Loquara ships is trained on 16 kHz mono audio.
pub const TARGET_SAMPLE_RATE: u32 = 16_000;

/// Rubato's sinc interpolator allocates filters per input chunk. Passing the
/// whole recording as one chunk (seven minutes of 48 kHz) exhausted memory
/// and crashed retry. A few thousand frames is enough for quality.
pub(crate) const RESAMPLE_INPUT_CHUNK: usize = 4_096;

/// Parakeet/NeMo offline decode aborts the whole process (Windows `0xc0000409`)
/// when given several minutes in one shot. Takes around 40s already succeed, so
/// windows stay under that.
pub(crate) const TRANSCRIBE_WINDOW_SAMPLES: usize = TARGET_SAMPLE_RATE as usize * 30;
/// Windows never end before this, so a long take is not chopped into
/// needlessly short pieces that lose sentence context.
pub(crate) const TRANSCRIBE_MIN_WINDOW_SAMPLES: usize = TARGET_SAMPLE_RATE as usize * 18;
/// Energy is measured over 20 ms frames, then averaged over 200 ms, so a cut
/// lands in a real pause rather than in the gap of a single plosive.
pub(crate) const CUT_FRAME_SAMPLES: usize = TARGET_SAMPLE_RATE as usize / 50;
const CUT_SMOOTH_FRAMES: usize = 10;

/// Which processor actually ran the last model load.
///
/// Reported to the interface so the user can see where the model runs. The
/// engine is CPU-only: DirectML on an RTX 5080 measured only 15-25% faster
/// than four CPU threads, while costing an 18 MB library, a slower load and
/// more memory. `Gpu` stays so the value the interface understands does not
/// change shape if an accelerator returns.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Accelerator {
    #[allow(dead_code)]
    Gpu,
    Cpu,
}

/// Threads to give ONNX Runtime.
///
/// Measured on the 8-bit Parakeet encoder, eight threads beat four by about
/// 15% on a 24-thread desktop, and more than eight bring nothing but
/// scheduling overhead. Half the logical processors approximates the physical
/// cores, so hyper-threads do not fight over the same core; small machines
/// still get at least four (or every processor they have).
fn decode_threads() -> usize {
    let logical = std::thread::available_parallelism()
        .map(|count| count.get())
        .unwrap_or(4);
    (logical / 2).clamp(4, 8).min(logical)
}

#[derive(Debug, thiserror::Error)]
pub enum EngineError {
    #[error("the model files for {model} are missing or incomplete")]
    ModelIncomplete { model: String },
    #[error("could not load the model: {0}")]
    Load(String),
    #[error("could not read the recording: {0}")]
    Audio(String),
    #[error("transcription failed: {0}")]
    Decode(String),
}

/// The recogniser for one model, plus which model it is.
struct Loaded {
    key: String,
    recogniser: ParakeetTDT,
}

/// Holds at most one loaded model.
///
/// Guarded by a mutex because dictation runs on a background thread while the
/// interface may ask to change models from another.
pub struct Engine {
    models_dir: PathBuf,
    loaded: Mutex<Option<Loaded>>,
    /// What the engine is doing, readable without waiting for it.
    ///
    /// Loading a model takes seconds and holds `loaded` for all of them. The
    /// interface asks what is loaded far more often than it loads anything,
    /// and it asks from the thread that draws the window: if that question
    /// queued behind a load, the whole app would stop repainting until the
    /// model was ready. This lock is only ever held for a field assignment.
    status: Mutex<Status>,
}

#[derive(Debug, Clone, Default)]
struct Status {
    key: Option<String>,
    accelerator: Option<Accelerator>,
    loading: bool,
}

impl Engine {
    pub fn new(models_dir: impl Into<PathBuf>) -> Self {
        Self {
            models_dir: models_dir.into(),
            loaded: Mutex::new(None),
            status: Mutex::new(Status::default()),
        }
    }

    /// Where a model's files live once unpacked.
    pub fn model_dir(&self, key: &str) -> PathBuf {
        self.models_dir.join(key)
    }

    /// Whether every file a model needs is present.
    pub fn is_installed(&self, key: &str) -> bool {
        is_complete(&self.model_dir(key))
    }

    /// The model currently held in memory, if any.
    pub fn loaded_model(&self) -> Option<String> {
        self.status.lock().ok().and_then(|status| status.key.clone())
    }

    /// What the loaded model is running on.
    pub fn accelerator(&self) -> Option<Accelerator> {
        self.status.lock().ok().and_then(|status| status.accelerator)
    }

    /// Whether a load is in flight.
    pub fn is_loading(&self) -> bool {
        self.status.lock().map(|status| status.loading).unwrap_or(false)
    }

    fn set_status(&self, status: Status) {
        if let Ok(mut guard) = self.status.lock() {
            *guard = status;
        }
    }

    /// Releases the model, returning its memory to the system.
    pub fn unload(&self) {
        if let Ok(mut guard) = self.loaded.lock() {
            *guard = None;
        }
        self.set_status(Status::default());
    }

    /// Loads a model, or does nothing if it is already the loaded one.
    pub fn ensure_loaded(&self, key: &str) -> Result<(), EngineError> {
        let mut guard = self
            .loaded
            .lock()
            .map_err(|_| EngineError::Load("engine lock poisoned".into()))?;
        if guard.as_ref().is_some_and(|loaded| loaded.key == key) {
            return Ok(());
        }
        let directory = self.model_dir(key);
        if !is_complete(&directory) {
            self.set_status(Status::default());
            return Err(EngineError::ModelIncomplete {
                model: key.to_owned(),
            });
        }
        self.set_status(Status { key: None, accelerator: None, loading: true });
        // Drop the previous model before building the next one, so two never
        // sit in memory at the same time.
        *guard = None;

        match load_recogniser(&directory) {
            Ok(recogniser) => {
                *guard = Some(Loaded { key: key.to_owned(), recogniser });
                self.set_status(Status {
                    key: Some(key.to_owned()),
                    accelerator: Some(Accelerator::Cpu),
                    loading: false,
                });
                Ok(())
            }
            Err(error) => {
                self.set_status(Status::default());
                Err(error)
            }
        }
    }

    /// Bounded local preview through the same resident native runtime as final
    /// WAV recognition. The model mutex permits only one inference at a time.
    pub fn transcribe_audio_samples(&self, key: &str, samples: &[f32], cancel: &AtomicBool) -> Result<String, EngineError> {
        if samples.len() > TRANSCRIBE_WINDOW_SAMPLES {
            return Err(EngineError::Decode("preview exceeds thirty seconds".into()));
        }
        if cancel.load(Ordering::SeqCst) { return Err(EngineError::Decode("preview cancelled".into())); }
        self.ensure_loaded(key)?;
        let mut guard = self.loaded.lock().map_err(|_| EngineError::Decode("engine lock poisoned".into()))?;
        let loaded = guard.as_mut().ok_or_else(|| EngineError::Decode("model unloaded".into()))?;
        let text = transcribe_samples(&mut loaded.recogniser, samples, Some(cancel))?;
        if cancel.load(Ordering::SeqCst) { return Err(EngineError::Decode("preview cancelled".into())); }
        Ok(text)
    }

    /// Decodes one window of a take that is still being recorded, for
    /// [`crate::progressive`]. Returns `None`, without waiting for the model,
    /// once `stop` is set: whatever is left is decoded by the final pass.
    pub fn transcribe_window(&self, key: &str, window: &[f32], stop: &AtomicBool) -> Result<Option<String>, EngineError> {
        if window.len() > TRANSCRIBE_WINDOW_SAMPLES {
            return Err(EngineError::Decode("window exceeds thirty seconds".into()));
        }
        if !has_speech(window) {
            return Ok(Some(String::new()));
        }
        if stop.load(Ordering::SeqCst) { return Ok(None); }
        self.ensure_loaded(key)?;
        if stop.load(Ordering::SeqCst) { return Ok(None); }
        let mut guard = self.loaded.lock().map_err(|_| EngineError::Decode("engine lock poisoned".into()))?;
        let loaded = guard.as_mut().ok_or_else(|| EngineError::Decode("model unloaded".into()))?;
        window_text(window, |window| decode_window(&mut loaded.recogniser, window)).map(Some)
    }

    /// The final pass over a take already converted to 16 kHz mono, reusing
    /// windows decoded while it was recorded. Identical to [`Self::transcribe`]
    /// on the same audio; the model lock is taken only if something is left
    /// to decode.
    pub fn transcribe_prepared(
        &self,
        key: &str,
        samples: &[f32],
        cache: &WindowCache,
        cancel: Option<&AtomicBool>,
    ) -> Result<String, EngineError> {
        let mut guard = None;
        final_text(samples, cache, cancel, |window| {
            if guard.is_none() {
                self.ensure_loaded(key)?;
                guard = Some(self.loaded.lock().map_err(|_| EngineError::Decode("engine lock poisoned".into()))?);
            }
            let loaded = guard
                .as_mut()
                .and_then(|guard| guard.as_mut())
                .ok_or_else(|| EngineError::Decode("model was unloaded mid-transcription".into()))?;
            decode_window(&mut loaded.recogniser, window)
        })
    }

    /// Transcribes a recording, loading the model first if necessary.
    ///
    /// Long takes are split into windows: feeding seven minutes to the encoder
    /// in one call aborts the process.
    pub fn transcribe(
        &self,
        key: &str,
        audio: &Path,
        cancel: Option<&AtomicBool>,
    ) -> Result<String, EngineError> {
        let samples = read_as_target_rate(audio)?;
        self.ensure_loaded(key)?;
        let mut guard = self
            .loaded
            .lock()
            .map_err(|_| EngineError::Decode("engine lock poisoned".into()))?;
        let loaded = guard
            .as_mut()
            .ok_or_else(|| EngineError::Decode("model was unloaded mid-transcription".into()))?;
        transcribe_samples(&mut loaded.recogniser, &samples, cancel)
    }
}

/// The files a Parakeet TDT model is made of, under the names parakeet-rs
/// looks for.
pub const MODEL_FILES: [&str; 3] = ["encoder-model.onnx", "decoder_joint-model.onnx", "vocab.txt"];

/// The encoder's weights, stored beside its graph.
///
/// Weights inside the `.onnx` protobuf are copied into private memory when
/// the session is built. Kept in a separate, 64 KiB-aligned file, ONNX Runtime
/// memory-maps them instead: the pages are backed by the file, so the system
/// can drop them under pressure without writing them to the page file.
/// Measured on the 8-bit encoder, private memory after a dictation fell from
/// 1376 MB to 749 MB (together with the arena below), at the same speed and
/// the same words.
pub const ENCODER_DATA_FILE: &str = "encoder-model.onnx.data";

/// An encoder graph whose weights live in [`ENCODER_DATA_FILE`] is under a
/// megabyte; one this large carries its weights inside. That is the layout
/// of the first 8-bit release, which installations downloaded before the
/// split and which still loads (only without the memory mapping).
const SELF_CONTAINED_ENCODER_MIN_BYTES: u64 = 100_000_000;

/// Whether `directory` holds a loadable model, in either layout.
fn is_complete(directory: &Path) -> bool {
    if !MODEL_FILES.iter().all(|name| directory.join(name).is_file()) {
        return false;
    }
    directory.join(ENCODER_DATA_FILE).is_file()
        || std::fs::metadata(directory.join(MODEL_FILES[0]))
            .is_ok_and(|meta| meta.len() >= SELF_CONTAINED_ENCODER_MIN_BYTES)
}

/// ONNX Runtime allows a single environment per process, so it is built once
/// and shared by every load (a model reloaded after unload or a model switch).
fn ort_environment() -> Result<parakeet_rs::ort::environment::Environment, EngineError> {
    static ENVIRONMENT: std::sync::OnceLock<Result<parakeet_rs::ort::environment::Environment, String>> = std::sync::OnceLock::new();
    ENVIRONMENT
        .get_or_init(|| parakeet_rs::ort::init().with_name("loquara").build().map_err(|error| error.to_string()))
        .clone()
        .map_err(EngineError::Load)
}

/// Builds the recogniser on the CPU, then runs it once.
///
/// The CPU memory arena is off: it kept the largest activations of the
/// longest window reserved after every dictation (about 240 MB more after a
/// 30 s window), and turning it off measured no slower.
///
/// The warm-up decode touches every memory-mapped weight page and every
/// kernel once, so the first real dictation does not pay for it (measured:
/// 241 ms instead of 52 ms for a short take straight after load).
fn load_recogniser(directory: &Path) -> Result<ParakeetTDT, EngineError> {
    let config = ExecutionConfig::new()
        .with_environment(ort_environment()?)
        .with_intra_threads(decode_threads())
        .with_custom_configure(|builder| {
            Ok(builder.with_execution_providers([parakeet_rs::ort::ep::CPU::default().with_arena_allocator(false).build()])?)
        });
    let mut recogniser =
        ParakeetTDT::from_pretrained(directory, Some(config)).map_err(|error| EngineError::Load(error.to_string()))?;
    warm_up(&mut recogniser);
    Ok(recogniser)
}

/// One second of a quiet tone, decoded and thrown away.
fn warm_up(recogniser: &mut ParakeetTDT) {
    let tone: Vec<f32> = (0..TARGET_SAMPLE_RATE as usize)
        .map(|index| 0.02 * (index as f32 * 0.07).sin())
        .collect();
    let _ = recogniser.transcribe_samples(tone, TARGET_SAMPLE_RATE, 1, None);
}

/// Encodes 16 kHz mono samples as a WAV, in memory.
pub fn encode_target_rate_wav(samples: &[f32]) -> Vec<u8> {
    let spec = hound::WavSpec {
        channels: 1,
        sample_rate: TARGET_SAMPLE_RATE,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };
    let mut cursor = std::io::Cursor::new(Vec::new());
    {
        let mut writer =
            hound::WavWriter::new(&mut cursor, spec).expect("cursor writes cannot fail");
        for sample in samples {
            let value = (sample.clamp(-1.0, 1.0) * 32767.0).round() as i16;
            writer
                .write_sample(value)
                .expect("cursor writes cannot fail");
        }
        writer.finalize().expect("cursor writes cannot fail");
    }
    cursor.into_inner()
}

/// The smallest WAV worth uploading to a provider.
///
/// A capture is routinely 48 kHz stereo — six times the bytes of the 16 kHz
/// mono every recogniser actually consumes — and those extra bytes are paid
/// for on the upload, which is where a slow take is slow.
pub fn compact_wav(path: &Path) -> Result<Vec<u8>, EngineError> {
    read_as_target_rate(path).map(|samples| encode_target_rate_wav(&samples))
}

/// Reads a WAV file as 16 kHz mono float samples.
///
/// Recordings are captured in whatever format the input device offers, which
/// is routinely 48 kHz stereo, while the models are trained on 16 kHz mono.
pub fn read_as_target_rate(path: &Path) -> Result<Vec<f32>, EngineError> {
    let mut reader =
        hound::WavReader::open(path).map_err(|error| EngineError::Audio(error.to_string()))?;
    let spec = reader.spec();
    let interleaved: Vec<f32> = match spec.sample_format {
        hound::SampleFormat::Int => reader
            .samples::<i32>()
            .map(|sample| {
                sample
                    .map(|value| value as f32 / i32::pow(2, spec.bits_per_sample as u32 - 1) as f32)
                    .map_err(|error| EngineError::Audio(error.to_string()))
            })
            .collect::<Result<_, _>>()?,
        hound::SampleFormat::Float => reader
            .samples::<f32>()
            .map(|sample| sample.map_err(|error| EngineError::Audio(error.to_string())))
            .collect::<Result<_, _>>()?,
    };
    let mono = downmix(&interleaved, spec.channels);
    resample(mono, spec.sample_rate)
}

/// Averages channels into one. Speech is centred in both, so a mean keeps it
/// and cancels a little of whatever is only on one side.
pub(crate) fn downmix(interleaved: &[f32], channels: u16) -> Vec<f32> {
    if channels <= 1 {
        return interleaved.to_vec();
    }
    let channels = channels as usize;
    interleaved
        .chunks_exact(channels)
        .map(|frame| frame.iter().sum::<f32>() / channels as f32)
        .collect()
}

/// Converts to 16 kHz.
///
/// Uses a windowed-sinc resampler rather than dropping samples: plain
/// decimation folds everything above 8 kHz back down into the speech band as
/// aliasing, which the acoustic model then has to hear through.
fn resample(samples: Vec<f32>, from_rate: u32) -> Result<Vec<f32>, EngineError> {
    if from_rate == TARGET_SAMPLE_RATE || samples.is_empty() {
        return Ok(samples);
    }
    let ratio = TARGET_SAMPLE_RATE as f64 / from_rate as f64;
    let mut resampler = target_rate_resampler(from_rate)?;
    let mut output = Vec::with_capacity(
        ((samples.len() as f64 * ratio).ceil() as usize).saturating_add(RESAMPLE_INPUT_CHUNK),
    );
    let mut pos = 0;
    while pos + RESAMPLE_INPUT_CHUNK <= samples.len() {
        let chunk = &samples[pos..pos + RESAMPLE_INPUT_CHUNK];
        let waves = resampler
            .process(&[chunk], None)
            .map_err(|error| EngineError::Audio(error.to_string()))?;
        if let Some(channel) = waves.first() {
            output.extend_from_slice(channel);
        }
        pos += RESAMPLE_INPUT_CHUNK;
    }
    let remainder = &samples[pos..];
    let waves = if remainder.is_empty() {
        resampler.process_partial::<&[f32]>(None, None)
    } else {
        resampler.process_partial(Some(&[remainder]), None)
    }
    .map_err(|error| EngineError::Audio(error.to_string()))?;
    if let Some(channel) = waves.first() {
        output.extend_from_slice(channel);
    }
    let expected = (samples.len() as u64)
        .saturating_mul(u64::from(TARGET_SAMPLE_RATE))
        / u64::from(from_rate);
    output.truncate(expected as usize);
    Ok(output)
}

/// The resampler [`resample`] uses, fed [`RESAMPLE_INPUT_CHUNK`] frames at a
/// time. Progressive decoding builds the same one, so feeding it the capture
/// as it arrives gives exactly the samples reading the finished WAV would.
pub(crate) fn target_rate_resampler(from_rate: u32) -> Result<SincFixedIn<f32>, EngineError> {
    let parameters = SincInterpolationParameters {
        sinc_len: 128,
        f_cutoff: 0.95,
        interpolation: SincInterpolationType::Linear,
        oversampling_factor: 128,
        window: WindowFunction::BlackmanHarris2,
    };
    let ratio = TARGET_SAMPLE_RATE as f64 / from_rate as f64;
    SincFixedIn::<f32>::new(ratio, 1.0, parameters, RESAMPLE_INPUT_CHUNK, 1)
        .map_err(|error| EngineError::Audio(error.to_string()))
}

/// Splits a take into contiguous, non-overlapping windows the encoder can
/// handle, cutting each one in the quietest stretch between 18 and 30 s.
///
/// Windows used to be fixed 30 s slices hopping by 29 s. The second window
/// then started in the middle of whatever word was being spoken at 29 s, and
/// the overlap was joined back without being removed, so every long take got
/// a duplicated, half-cut phrase at each boundary ("coś poszło nie tak. Zło
/// nie tak."). Cutting in a pause, with no overlap, means no word is ever
/// split and nothing is transcribed twice.
pub(crate) fn transcription_windows(samples: &[f32]) -> Vec<(usize, usize)> {
    let sample_count = samples.len();
    if sample_count == 0 {
        return Vec::new();
    }
    let mut windows = Vec::new();
    let mut start = 0;
    while sample_count - start > TRANSCRIBE_WINDOW_SAMPLES {
        let end = quietest_cut(
            samples,
            start + TRANSCRIBE_MIN_WINDOW_SAMPLES,
            start + TRANSCRIBE_WINDOW_SAMPLES,
        );
        windows.push((start, end));
        start = end;
    }
    windows.push((start, sample_count));
    windows
}

/// The centre of the quietest ~200 ms stretch within `from..to`.
///
/// Ties go to the latest candidate, so steady silence (or a synthetic take of
/// constant level) still produces windows as long as allowed.
pub(crate) fn quietest_cut(samples: &[f32], from: usize, to: usize) -> usize {
    let to = to.min(samples.len());
    let frames: Vec<f32> = samples[from..to]
        .chunks(CUT_FRAME_SAMPLES)
        .map(|frame| {
            frame
                .iter()
                .filter(|sample| sample.is_finite())
                .map(|sample| sample * sample)
                .sum::<f32>()
                / frame.len() as f32
        })
        .collect();
    if frames.len() <= CUT_SMOOTH_FRAMES {
        return to;
    }
    let mut best = (f32::INFINITY, frames.len() - CUT_SMOOTH_FRAMES);
    for (first, run) in frames.windows(CUT_SMOOTH_FRAMES).enumerate() {
        let energy: f32 = run.iter().sum();
        if energy <= best.0 {
            best = (energy, first);
        }
    }
    let centre_frame = best.1 + CUT_SMOOTH_FRAMES / 2;
    (from + centre_frame * CUT_FRAME_SAMPLES).min(to)
}

/// Frames quieter than this (about -66 dBFS) are digital silence: what a
/// noise gate or Windows voice processing leaves between phrases. A real room
/// is louder than this even when nobody speaks, so ordinary pauses are kept.
const DIGITAL_SILENCE_RMS: f32 = 0.0005;
/// Silent runs longer than 2 s are shortened...
const MAX_SILENCE_FRAMES: usize = 100;
/// ...to 0.8 s at each end, so the pause still reads as a pause.
const KEPT_SILENCE_FRAMES: usize = 40;

/// Shortens long stretches of gated, near-zero silence to 1.6 s.
///
/// Measured on real takes from a gated USB microphone: when a window started
/// with 3-5 s of digital silence, Parakeet dropped the first words after it
/// ("Nowy zrzut ekranu nagrywania" came back as "Nrywania"), and long takes
/// lost whole sentences. Such silence is unlike anything the model was trained
/// on, so recognition sees at most 1.6 s of it. The WAV on disk is not touched.
///
/// The cap used to be 0.4 s for any run over 0.6 s. That squeezed ordinary
/// thinking pauses too, and Parakeet, which reads the whole window at once,
/// then lost its sentence boundaries: a take opening "A próbowałeś…" came
/// back as "a próbowałeś…", words merged across the splice ("żeby" → "żebyśś")
/// and silent takes produced "Okay." from nothing. Over 80 of the user's takes
/// the 2 s / 1.6 s setting kept every word the 0.4 s cap recovered in long
/// takes, while short takes come out as they would from the untouched audio.
fn shorten_digital_silence(samples: &[f32]) -> Vec<f32> {
    shorten_digital_silence_tracked(samples).samples
}

/// [`shorten_digital_silence`] applied to audio that is still growing.
pub(crate) struct Shortened {
    pub samples: Vec<f32>,
    /// How many leading output samples no later audio can change: everything
    /// before a trailing silent run (which may still grow past 2 s and lose
    /// its middle) and before a trailing, still-partial frame. The first
    /// 0.8 s of that run is kept whatever its final length, so it counts.
    pub stable: usize,
    /// Where silence was dropped: (output index, input samples removed).
    pub removed: Vec<(usize, usize)>,
}

impl Shortened {
    /// The input index an output index came from.
    pub fn source_index(&self, output_index: usize) -> usize {
        output_index
            + self
                .removed
                .iter()
                .filter(|(at, _)| *at <= output_index)
                .map(|(_, removed)| removed)
                .sum::<usize>()
    }
}

pub(crate) fn shorten_digital_silence_tracked(samples: &[f32]) -> Shortened {
    let frames: Vec<&[f32]> = samples.chunks(CUT_FRAME_SAMPLES).collect();
    let silent = |frame: &[f32]| {
        frame.len() == CUT_FRAME_SAMPLES
            && frame.iter().map(|sample| if sample.is_finite() { sample * sample } else { 0.0 }).sum::<f32>()
                / (frame.len() as f32)
                < DIGITAL_SILENCE_RMS * DIGITAL_SILENCE_RMS
    };
    let mut output = Vec::with_capacity(samples.len());
    let mut removed = Vec::new();
    let mut open_run_stable = None;
    let mut index = 0;
    while index < frames.len() {
        if !silent(frames[index]) {
            output.extend_from_slice(frames[index]);
            index += 1;
            continue;
        }
        let run_start = index;
        while index < frames.len() && silent(frames[index]) {
            index += 1;
        }
        let run = &frames[run_start..index];
        // Followed by nothing, or only by a frame still being filled: more
        // audio may extend this run.
        if frames[index..].iter().all(|frame| frame.len() < CUT_FRAME_SAMPLES) {
            open_run_stable = Some(output.len() + run.len().min(KEPT_SILENCE_FRAMES) * CUT_FRAME_SAMPLES);
        }
        if run.len() > MAX_SILENCE_FRAMES {
            for frame in &run[..KEPT_SILENCE_FRAMES] {
                output.extend_from_slice(frame);
            }
            removed.push((output.len(), (run.len() - 2 * KEPT_SILENCE_FRAMES) * CUT_FRAME_SAMPLES));
            for frame in &run[run.len() - KEPT_SILENCE_FRAMES..] {
                output.extend_from_slice(frame);
            }
        } else {
            for frame in run {
                output.extend_from_slice(frame);
            }
        }
    }
    let stable = open_run_stable.unwrap_or(output.len() - samples.len() % CUT_FRAME_SAMPLES);
    Shortened { samples: output, stable, removed }
}

fn join_window_transcripts(parts: &[String]) -> String {
    parts
        .iter()
        .map(|part| part.trim())
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

/// A 20 ms frame at or above this level (-40 dBFS) counts as possible speech.
const SPEECH_FRAME_RMS: f32 = 0.01;
/// How much possible speech a window needs before it is worth decoding.
const MIN_SPEECH_FRAMES: usize = 5;

/// Whether `samples` contain anything louder than a silent room.
///
/// Parakeet invents a word for silence: the user's empty takes, whose loudest
/// 20 ms frame stays between -51 and -40 dBFS, came back as "Okay.", "Yeah."
/// or "Mm.". Across all 267 of the user's recordings, the quietest take with
/// real speech peaked at -23 dBFS and held over a second above -50 dBFS, so
/// asking for 100 ms at -40 dBFS or louder leaves a 17 dB margin on the speech
/// side while every silent take falls below it.
pub(crate) fn has_speech(samples: &[f32]) -> bool {
    samples
        .chunks(CUT_FRAME_SAMPLES)
        .filter(|frame| {
            let energy = frame
                .iter()
                .map(|sample| if sample.is_finite() { sample * sample } else { 0.0 })
                .sum::<f32>()
                / frame.len() as f32;
            energy >= SPEECH_FRAME_RMS * SPEECH_FRAME_RMS
        })
        .nth(MIN_SPEECH_FRAMES - 1)
        .is_some()
}

fn transcribe_samples(
    recogniser: &mut ParakeetTDT,
    samples: &[f32],
    cancel: Option<&AtomicBool>,
) -> Result<String, EngineError> {
    final_text(samples, &WindowCache::new(), cancel, |window| decode_window(recogniser, window))
}

fn decode_window(recogniser: &mut ParakeetTDT, window: &[f32]) -> Result<String, EngineError> {
    recogniser
        .transcribe_samples(window.to_vec(), TARGET_SAMPLE_RATE, 1, None)
        .map(|result| result.text)
        .map_err(|error| EngineError::Decode(error.to_string()))
}

/// Text already decoded for a window, keyed by where the window lies in the
/// silence-shortened take and a hash of its samples.
pub type WindowCache = std::collections::HashMap<WindowKey, String>;
pub type WindowKey = (usize, usize, u64);

/// FNV-1a over the samples' bits: a window is reused only if every sample
/// matches what the final pass would decode.
pub(crate) fn window_hash(window: &[f32]) -> u64 {
    window.iter().fold(0xcbf2_9ce4_8422_2325_u64, |hash, sample| {
        sample
            .to_bits()
            .to_le_bytes()
            .iter()
            .fold(hash, |hash, byte| (hash ^ u64::from(*byte)).wrapping_mul(0x0100_0000_01b3))
    })
}

/// What one window contributes to the transcript: nothing without speech,
/// otherwise the model's trimmed text.
pub(crate) fn window_text(
    window: &[f32],
    decode: impl FnOnce(&[f32]) -> Result<String, EngineError>,
) -> Result<String, EngineError> {
    if !has_speech(window) {
        return Ok(String::new());
    }
    Ok(decode(window)?.trim().to_owned())
}

/// The final transcript of a whole take (16 kHz mono): shorten gated
/// silence, cut into windows, decode each, join.
///
/// The one definition of the final pass. A window found in `cache` under the
/// same position and sample hash is not decoded again; progressive decoding
/// fills the cache while the take is still being recorded, so at stop only the
/// windows it has not seen are decoded. An empty cache is the batch pass.
pub(crate) fn final_text(
    samples: &[f32],
    cache: &WindowCache,
    cancel: Option<&AtomicBool>,
    mut decode: impl FnMut(&[f32]) -> Result<String, EngineError>,
) -> Result<String, EngineError> {
    let samples = &shorten_digital_silence(samples);
    let mut parts = Vec::new();
    for (start, end) in transcription_windows(samples) {
        if cancel.is_some_and(|flag| flag.load(Ordering::SeqCst)) {
            return Err(EngineError::Decode("transcription cancelled".into()));
        }
        let window = &samples[start..end];
        let cached = if cache.is_empty() { None } else { cache.get(&(start, end, window_hash(window))) };
        let text = match cached {
            Some(text) => text.clone(),
            None => window_text(window, &mut decode)?,
        };
        if !text.is_empty() {
            parts.push(text);
        }
    }
    Ok(join_window_transcripts(&parts))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The interface asks these questions from the thread that draws the
    /// window. Answering them used to mean waiting for the model to finish
    /// loading, which froze Loquara for as long as the load took.
    #[test]
    fn status_is_readable_while_a_model_is_being_loaded() {
        use std::sync::mpsc;
        use std::sync::Arc;

        let temp = tempfile::tempdir().unwrap();
        let engine = Arc::new(Engine::new(temp.path()));
        let held = engine.loaded.lock().unwrap();

        let asking = Arc::clone(&engine);
        let (answered, answer) = mpsc::channel();
        std::thread::spawn(move || {
            let status = (asking.loaded_model(), asking.accelerator(), asking.is_installed("parakeet"));
            let _ = answered.send(status);
        });

        let status = answer
            .recv_timeout(std::time::Duration::from_secs(5))
            .expect("status must not queue behind the load lock");
        assert_eq!(status, (None, None, false));
        drop(held);
    }

    #[test]
    fn stereo_is_averaged_into_one_channel() {
        let interleaved = [1.0, 0.0, 0.5, 0.5, -1.0, 1.0];

        assert_eq!(downmix(&interleaved, 2), vec![0.5, 0.5, 0.0]);
    }

    #[test]
    fn mono_audio_is_left_alone() {
        let mono = [0.1, 0.2, 0.3];

        assert_eq!(downmix(&mono, 1), mono.to_vec());
    }

    #[test]
    fn audio_already_at_the_target_rate_is_not_resampled() {
        let samples = vec![0.1, 0.2, 0.3];

        assert_eq!(resample(samples.clone(), TARGET_SAMPLE_RATE).unwrap(), samples);
    }

    #[test]
    fn resampling_produces_the_expected_number_of_samples() {
        // One second at 48 kHz has to come out as roughly one second at 16 kHz;
        // a length that drifts means the audio would be played back at the
        // wrong speed and the model would hear the wrong thing.
        let one_second = vec![0.0_f32; 48_000];

        let out = resample(one_second, 48_000).unwrap();

        let drift = (out.len() as i64 - TARGET_SAMPLE_RATE as i64).abs();
        assert!(drift < 200, "expected about 16000 samples, got {}", out.len());
    }

    #[test]
    fn resampling_a_long_take_uses_bounded_chunks() {
        assert!(
            RESAMPLE_INPUT_CHUNK <= 8_192,
            "a chunk the size of the whole file is what crashed 7-minute retries"
        );
        // Ten seconds at 48 kHz is longer than one chunk, so this actually
        // walks the loop instead of treating the file as a single buffer.
        let ten_seconds = vec![0.0_f32; 48_000 * 10];

        let out = resample(ten_seconds, 48_000).unwrap();

        let expected = i64::from(TARGET_SAMPLE_RATE) * 10;
        let drift = (out.len() as i64 - expected).abs();
        assert!(
            drift < 400,
            "expected about {expected} samples, got {}",
            out.len()
        );
    }

    #[test]
    fn a_compact_wav_is_16k_mono_and_six_times_smaller_than_the_capture() {
        // A second of 48 kHz stereo, the shape most microphones produce.
        let samples: Vec<f32> = (0..48_000 * 2)
            .map(|index| ((index % 100) as f32 / 100.0) - 0.5)
            .collect();
        let path = std::env::temp_dir().join("loquara-compact-test.wav");
        {
            let spec = hound::WavSpec {
                channels: 2,
                sample_rate: 48_000,
                bits_per_sample: 16,
                sample_format: hound::SampleFormat::Int,
            };
            let mut writer = hound::WavWriter::create(&path, spec).unwrap();
            for sample in &samples {
                writer.write_sample((sample * 32767.0) as i16).unwrap();
            }
            writer.finalize().unwrap();
        }

        let compact = compact_wav(&path).unwrap();
        let _ = std::fs::remove_file(&path);

        let reader = hound::WavReader::new(std::io::Cursor::new(&compact)).unwrap();
        assert_eq!(reader.spec().channels, 1);
        assert_eq!(reader.spec().sample_rate, 16_000);
        assert_eq!(reader.duration(), 16_000);
        // 48 kHz stereo is 192 000 bytes per second; this is 32 000.
        assert!(compact.len() < 40_000, "got {} bytes", compact.len());
    }

    #[test]
    fn an_empty_recording_resamples_to_nothing() {
        assert!(resample(Vec::new(), 48_000).unwrap().is_empty());
    }

    #[test]
    fn a_short_take_is_a_single_transcription_window() {
        assert_eq!(transcription_windows(&vec![0.1; 16_000]), vec![(0, 16_000)]);
        assert!(transcription_windows(&[]).is_empty());
        assert_eq!(
            transcription_windows(&vec![0.1; TRANSCRIBE_WINDOW_SAMPLES]),
            vec![(0, TRANSCRIBE_WINDOW_SAMPLES)]
        );
    }

    /// Steady "speech" with a 300 ms pause at `pause_at` seconds.
    fn speech_with_pause(seconds: usize, pause_at: f32) -> Vec<f32> {
        let rate = TARGET_SAMPLE_RATE as usize;
        let mut samples: Vec<f32> = (0..seconds * rate)
            .map(|index| 0.3 * ((index as f32) * 0.07).sin())
            .collect();
        let pause = (pause_at * rate as f32) as usize;
        samples[pause..pause + rate * 3 / 10].fill(0.0);
        samples
    }

    #[test]
    fn a_seven_minute_take_is_split_into_encoder_sized_windows() {
        // This is the 6:49 recording: one-shot decode aborted the process
        // (Windows 0xc0000409). Windows must cover every sample exactly once
        // and never exceed the length that already transcribes successfully.
        let samples = vec![0.2_f32; TARGET_SAMPLE_RATE as usize * 409];
        let windows = transcription_windows(&samples);

        assert!(windows.len() > 1);
        assert_eq!(windows[0].0, 0);
        assert_eq!(windows.last().map(|(_, end)| *end), Some(samples.len()));
        assert!(windows.iter().all(|(start, end)| end - start <= TRANSCRIBE_WINDOW_SAMPLES));
        assert!(windows.windows(2).all(|pair| pair[0].1 == pair[1].0), "{windows:?}");
    }

    #[test]
    fn long_takes_are_cut_in_a_pause_not_in_the_middle_of_a_word() {
        // Regression: fixed 30 s windows hopping by 29 s split the word
        // spoken at 29-30 s and transcribed that second twice, so the user's
        // 48 s take came back as "coś poszło nie tak. Zło nie tak.".
        let rate = TARGET_SAMPLE_RATE as usize;
        let samples = speech_with_pause(48, 23.5);

        let windows = transcription_windows(&samples);

        assert_eq!(windows.len(), 2, "{windows:?}");
        let cut = windows[0].1;
        let pause = (23.5 * rate as f32) as usize..(23.8 * rate as f32) as usize;
        assert!(pause.contains(&cut), "cut at {cut} missed the pause {pause:?}");
        assert_eq!(windows[1], (cut, samples.len()), "windows must not overlap");
    }

    #[test]
    fn long_gated_silence_is_shortened_but_speech_and_short_pauses_are_kept() {
        let rate = TARGET_SAMPLE_RATE as usize;
        let speech: Vec<f32> = (0..rate).map(|index| 0.2 * ((index as f32) * 0.05).sin()).collect();
        let short_pause = vec![0.0_f32; rate / 2];
        let gated = vec![0.00001_f32; rate * 5];
        let input = [gated.clone(), speech.clone(), short_pause.clone(), speech.clone(), gated, speech.clone()].concat();

        let output = shorten_digital_silence(&input);

        let kept = 2 * KEPT_SILENCE_FRAMES * CUT_FRAME_SAMPLES;
        assert_eq!(output.len(), 3 * speech.len() + short_pause.len() + 2 * kept);
        // Speech survives sample for sample.
        assert_eq!(&output[kept..kept + speech.len()], speech.as_slice());
        assert_eq!(&output[output.len() - speech.len()..], speech.as_slice());
    }

    #[test]
    fn a_gated_thinking_pause_is_left_intact() {
        // Regression: a 1.5 s pause was squeezed to 0.4 s, which made
        // Parakeet open the real take "A próbowałeś…" in lower case.
        let rate = TARGET_SAMPLE_RATE as usize;
        let speech: Vec<f32> = (0..rate).map(|index| 0.2 * ((index as f32) * 0.05).sin()).collect();
        let pause = vec![0.00001_f32; rate * 3 / 2];
        let input = [speech.clone(), pause, speech].concat();

        assert_eq!(shorten_digital_silence(&input), input);
    }

    #[test]
    fn room_noise_is_not_mistaken_for_digital_silence() {
        // -54 dBFS hiss: an ungated microphone in a quiet room.
        let hiss: Vec<f32> = (0..TARGET_SAMPLE_RATE as usize * 5)
            .map(|index| if index % 2 == 0 { 0.002 } else { -0.002 })
            .collect();

        assert_eq!(shorten_digital_silence(&hiss).len(), hiss.len());
    }

    #[test]
    fn a_pause_before_the_minimum_window_is_not_used_as_a_cut() {
        // A pause at 5 s would make a needlessly short first window.
        let samples = speech_with_pause(40, 5.0);

        let windows = transcription_windows(&samples);

        assert!(windows[0].1 >= TRANSCRIBE_MIN_WINDOW_SAMPLES, "{windows:?}");
        assert!(windows[0].1 <= TRANSCRIBE_WINDOW_SAMPLES, "{windows:?}");
    }

    #[test]
    fn window_transcripts_are_joined_without_empty_pieces() {
        assert_eq!(
            join_window_transcripts(&[
                "  pierwsza  ".into(),
                String::new(),
                "druga".into(),
            ]),
            "pierwsza druga"
        );
    }

    #[test]
    fn decode_threads_stay_in_the_measured_range() {
        // More than eight only added scheduling overhead; fewer than four
        // happens only on machines that have fewer processors than that.
        let threads = decode_threads();
        let logical = std::thread::available_parallelism().map(|count| count.get()).unwrap_or(4);
        assert!(threads <= 8 && threads >= 1, "{threads}");
        assert!(threads >= 4.min(logical), "{threads}");
    }

    #[test]
    fn a_model_directory_missing_a_file_is_not_installed() {
        let temp = tempfile::tempdir().unwrap();
        let engine = Engine::new(temp.path());
        let directory = engine.model_dir("parakeet");
        std::fs::create_dir_all(&directory).unwrap();
        for name in ["encoder-model.onnx", ENCODER_DATA_FILE, "decoder_joint-model.onnx"] {
            std::fs::write(directory.join(name), b"x").unwrap();
        }

        assert!(!engine.is_installed("parakeet"), "vocab.txt is missing");

        std::fs::write(directory.join("vocab.txt"), b"x").unwrap();
        assert!(engine.is_installed("parakeet"));
    }

    #[test]
    fn a_split_encoder_without_its_weights_is_not_installed() {
        // A download interrupted between the graph and its data file.
        let temp = tempfile::tempdir().unwrap();
        let engine = Engine::new(temp.path());
        let directory = engine.model_dir("parakeet");
        std::fs::create_dir_all(&directory).unwrap();
        for name in MODEL_FILES {
            std::fs::write(directory.join(name), b"x").unwrap();
        }

        assert!(!engine.is_installed("parakeet"));
    }

    #[test]
    fn the_first_releases_self_contained_encoder_still_counts_as_installed() {
        // Installations from before the split keep their 945 MB encoder and
        // load it as they always did, without downloading anything again.
        let temp = tempfile::tempdir().unwrap();
        let engine = Engine::new(temp.path());
        let directory = engine.model_dir("parakeet");
        std::fs::create_dir_all(&directory).unwrap();
        for name in MODEL_FILES {
            std::fs::write(directory.join(name), b"x").unwrap();
        }
        std::fs::File::options()
            .write(true)
            .open(directory.join(MODEL_FILES[0]))
            .unwrap()
            .set_len(SELF_CONTAINED_ENCODER_MIN_BYTES)
            .unwrap();

        assert!(engine.is_installed("parakeet"));
    }

    #[test]
    fn the_old_sherpa_model_does_not_count_as_installed() {
        // Users upgrading from the sherpa-onnx engine still have its int8
        // files; the new engine cannot read them, so the app must offer the
        // download instead of failing on the first dictation.
        let temp = tempfile::tempdir().unwrap();
        let engine = Engine::new(temp.path());
        let directory = engine.model_dir("parakeet");
        std::fs::create_dir_all(&directory).unwrap();
        for name in ["encoder.int8.onnx", "decoder.int8.onnx", "joiner.int8.onnx", "tokens.txt"] {
            std::fs::write(directory.join(name), b"x").unwrap();
        }

        assert!(!engine.is_installed("parakeet"));
    }

    /// Low-level noise shaped like the user's empty takes: a floor around
    /// -60 dBFS with a few clicks up to -41 dBFS.
    fn silent_take(seconds: usize) -> Vec<f32> {
        let rate = TARGET_SAMPLE_RATE as usize;
        let mut samples: Vec<f32> = (0..seconds * rate)
            .map(|index| if index % 2 == 0 { 0.001 } else { -0.001 })
            .collect();
        for click in [rate / 2, rate, rate * 3 / 2] {
            samples[click..click + CUT_FRAME_SAMPLES].iter_mut().enumerate().for_each(|(index, sample)| {
                *sample = if index % 2 == 0 { 0.009 } else { -0.009 };
            });
        }
        samples
    }

    #[test]
    fn a_take_without_speech_is_not_decoded() {
        assert!(!has_speech(&silent_take(4)));
        assert!(!has_speech(&[]));
        assert!(!has_speech(&vec![0.0; TARGET_SAMPLE_RATE as usize * 3]));
    }

    #[test]
    fn quiet_speech_is_still_decoded() {
        // A quarter of a second of speech at -32 dBFS (RMS), well below the
        // quietest real take (-23 dBFS peak), inside an otherwise silent take.
        let rate = TARGET_SAMPLE_RATE as usize;
        let mut samples = silent_take(3);
        for (index, sample) in samples[rate..rate + rate / 4].iter_mut().enumerate() {
            *sample = 0.035 * ((index as f32) * 0.05).sin();
        }
        assert!(has_speech(&samples));
    }

    #[test]
    fn a_single_loud_click_is_not_speech() {
        let mut samples = silent_take(2);
        samples[1000..1000 + CUT_FRAME_SAMPLES].fill(0.5);
        assert!(!has_speech(&samples));
    }

    /// End-to-end check against a real model and a real recording.
    ///
    /// Ignored by default because it needs a downloaded model, which is far
    /// too large to keep in the repository. Run it with the two paths set:
    ///
    /// ```text
    /// LOQUARA_TEST_MODELS=<dir containing parakeet/>     /// LOQUARA_TEST_WAV=<a .wav file>     ///   cargo test --lib engine::tests::transcribes -- --ignored --nocapture
    /// ```
    #[test]
    #[ignore = "needs a downloaded model"]
    fn transcribes_a_real_recording() {
        let models = std::env::var("LOQUARA_TEST_MODELS").expect("LOQUARA_TEST_MODELS");
        let wav = std::env::var("LOQUARA_TEST_WAV").expect("LOQUARA_TEST_WAV");
        let engine = Engine::new(models);

        let started = std::time::Instant::now();
        let text = engine.transcribe("parakeet", Path::new(&wav), None).unwrap();
        let first = started.elapsed();

        // Loading is the expensive part, so the second pass proves the model
        // is still resident rather than rebuilt.
        let started = std::time::Instant::now();
        let again = engine.transcribe("parakeet", Path::new(&wav), None).unwrap();
        let second = started.elapsed();

        println!("first: {first:?}  second: {second:?}");
        println!("accelerator: {:?}", engine.accelerator());
        println!("text: {text}");
        assert!(!text.is_empty());
        assert_eq!(text, again);
        assert!(second < first, "the model should not be reloaded");
    }

    /// ONNX Runtime permits one environment per process: a reload after an
    /// unload (keep-alive) or from a second engine must reuse it, not fail.
    #[test]
    #[ignore = "needs a downloaded model"]
    fn the_model_reloads_after_unload_in_the_same_process() {
        let models = std::env::var("LOQUARA_TEST_MODELS").expect("LOQUARA_TEST_MODELS");
        let engine = Engine::new(models.clone());
        engine.ensure_loaded("parakeet").unwrap();
        engine.unload();
        engine.ensure_loaded("parakeet").unwrap();
        Engine::new(models).ensure_loaded("parakeet").unwrap();
    }

    /// A real take with no speech in it must come back empty rather than as
    /// "Okay." Set `LOQUARA_TEST_SILENT_WAV` alongside `LOQUARA_TEST_MODELS`.
    #[test]
    #[ignore = "needs a downloaded model"]
    fn a_real_silent_take_transcribes_to_nothing() {
        let models = std::env::var("LOQUARA_TEST_MODELS").expect("LOQUARA_TEST_MODELS");
        let wav = std::env::var("LOQUARA_TEST_SILENT_WAV").expect("LOQUARA_TEST_SILENT_WAV");
        let engine = Engine::new(models);

        assert_eq!(engine.transcribe("parakeet", Path::new(&wav), None).unwrap(), "");
    }

    #[test]
    fn loading_a_model_that_is_not_there_says_so() {
        let temp = tempfile::tempdir().unwrap();
        let engine = Engine::new(temp.path());

        let error = engine.ensure_loaded("parakeet").unwrap_err();

        assert!(matches!(error, EngineError::ModelIncomplete { .. }), "{error:?}");
        assert_eq!(engine.loaded_model(), None);
    }
}
