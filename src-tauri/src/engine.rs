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

use rubato::{Resampler, SincFixedIn, SincInterpolationParameters, SincInterpolationType, WindowFunction};
use sherpa_rs::transducer::{TransducerConfig, TransducerRecognizer};

/// Every model Loquara ships is trained on 16 kHz mono audio.
pub const TARGET_SAMPLE_RATE: u32 = 16_000;

/// Rubato's sinc interpolator allocates filters per input chunk. Passing the
/// whole recording as one chunk (seven minutes of 48 kHz) exhausted memory
/// and crashed retry. A few thousand frames is enough for quality.
const RESAMPLE_INPUT_CHUNK: usize = 4_096;

/// Parakeet/NeMo offline decode aborts the whole process (Windows `0xc0000409`)
/// when given several minutes in one shot. Takes around 40s already succeed, so
/// windows stay under that and hop with a little overlap so words on the cut
/// are not dropped.
const TRANSCRIBE_WINDOW_SAMPLES: usize = TARGET_SAMPLE_RATE as usize * 30;
const TRANSCRIBE_HOP_SAMPLES: usize = TARGET_SAMPLE_RATE as usize * 29;

/// Which processor actually ran the last model load.
///
/// Reported to the interface so the user can see whether their graphics card
/// is being used, without the app having to ask them what hardware they own.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Accelerator {
    /// A DirectX 12 graphics card: NVIDIA, AMD or Intel alike.
    Gpu,
    Cpu,
}

/// Providers to try, best first.
///
/// Loquara does not inspect the hardware. Detecting graphics cards means
/// keeping a list of what counts, which is wrong on the day a new card ships;
/// asking the provider to start and seeing whether it does is the same answer
/// with none of the guesswork. DirectML covers every DirectX 12 card, so one
/// attempt serves NVIDIA, AMD and Intel, and the CPU below it always works.
const PROVIDERS: &[(&str, Accelerator)] = &[("directml", Accelerator::Gpu), ("cpu", Accelerator::Cpu)];

/// Threads to give ONNX Runtime.
///
/// Measured on this workload, four threads reach the same speed as eight and
/// sixteen are markedly *slower* — the graph is small enough that scheduling
/// overhead outweighs the extra parallelism. Machines with fewer cores use
/// what they have.
fn decode_threads() -> i32 {
    let cores = std::thread::available_parallelism()
        .map(|count| count.get())
        .unwrap_or(4);
    cores.clamp(1, 4) as i32
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
    recogniser: TransducerRecognizer,
    accelerator: Accelerator,
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
        required_files(&self.model_dir(key))
            .iter()
            .all(|path| path.is_file())
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
        let files = required_files(&directory);
        if !files.iter().all(|path| path.is_file()) {
            self.set_status(Status::default());
            return Err(EngineError::ModelIncomplete {
                model: key.to_owned(),
            });
        }
        self.set_status(Status { key: None, accelerator: None, loading: true });
        // Drop the previous model before building the next one, so two never
        // sit in memory at the same time.
        *guard = None;

        let mut last_error = String::from("no provider was tried");
        for (provider, accelerator) in PROVIDERS {
            let config = TransducerConfig {
                encoder: files[0].to_string_lossy().into_owned(),
                decoder: files[1].to_string_lossy().into_owned(),
                joiner: files[2].to_string_lossy().into_owned(),
                tokens: files[3].to_string_lossy().into_owned(),
                model_type: "nemo_transducer".into(),
                num_threads: decode_threads(),
                decoding_method: "greedy_search".into(),
                provider: Some((*provider).to_owned()),
                debug: false,
                ..Default::default()
            };
            match TransducerRecognizer::new(config) {
                Ok(recogniser) => {
                    *guard = Some(Loaded {
                        key: key.to_owned(),
                        recogniser,
                        accelerator: *accelerator,
                    });
                    self.set_status(Status {
                        key: Some(key.to_owned()),
                        accelerator: Some(*accelerator),
                        loading: false,
                    });
                    return Ok(());
                }
                Err(error) => last_error = error.to_string(),
            }
        }
        self.set_status(Status::default());
        Err(EngineError::Load(last_error))
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

/// The files a transducer model is made of, in the order the config wants.
fn required_files(directory: &Path) -> [PathBuf; 4] {
    [
        directory.join("encoder.int8.onnx"),
        directory.join("decoder.int8.onnx"),
        directory.join("joiner.int8.onnx"),
        directory.join("tokens.txt"),
    ]
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
fn downmix(interleaved: &[f32], channels: u16) -> Vec<f32> {
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
    let parameters = SincInterpolationParameters {
        sinc_len: 128,
        f_cutoff: 0.95,
        interpolation: SincInterpolationType::Linear,
        oversampling_factor: 128,
        window: WindowFunction::BlackmanHarris2,
    };
    let ratio = TARGET_SAMPLE_RATE as f64 / from_rate as f64;
    let mut resampler = SincFixedIn::<f32>::new(ratio, 1.0, parameters, RESAMPLE_INPUT_CHUNK, 1)
        .map_err(|error| EngineError::Audio(error.to_string()))?;
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

fn transcription_windows(sample_count: usize) -> Vec<(usize, usize)> {
    if sample_count == 0 {
        return Vec::new();
    }
    if sample_count <= TRANSCRIBE_WINDOW_SAMPLES {
        return vec![(0, sample_count)];
    }
    let mut windows = Vec::new();
    let mut start = 0;
    loop {
        let end = (start + TRANSCRIBE_WINDOW_SAMPLES).min(sample_count);
        windows.push((start, end));
        if end == sample_count {
            break;
        }
        start += TRANSCRIBE_HOP_SAMPLES;
        if start >= sample_count {
            break;
        }
    }
    windows
}

fn join_window_transcripts(parts: &[String]) -> String {
    parts
        .iter()
        .map(|part| part.trim())
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

fn transcribe_samples(
    recogniser: &mut TransducerRecognizer,
    samples: &[f32],
    cancel: Option<&AtomicBool>,
) -> Result<String, EngineError> {
    let mut parts = Vec::new();
    for (start, end) in transcription_windows(samples.len()) {
        if cancel.is_some_and(|flag| flag.load(Ordering::SeqCst)) {
            return Err(EngineError::Decode("transcription cancelled".into()));
        }
        let text = recogniser.transcribe(TARGET_SAMPLE_RATE, &samples[start..end]);
        let trimmed = text.trim();
        if !trimmed.is_empty() {
            parts.push(trimmed.to_owned());
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
    fn an_empty_recording_resamples_to_nothing() {
        assert!(resample(Vec::new(), 48_000).unwrap().is_empty());
    }

    #[test]
    fn a_short_take_is_a_single_transcription_window() {
        assert_eq!(transcription_windows(16_000), vec![(0, 16_000)]);
        assert!(transcription_windows(0).is_empty());
        assert_eq!(
            transcription_windows(TRANSCRIBE_WINDOW_SAMPLES),
            vec![(0, TRANSCRIBE_WINDOW_SAMPLES)]
        );
    }

    #[test]
    fn a_seven_minute_take_is_split_into_encoder_sized_windows() {
        // This is the 6:49 recording: one-shot decode aborted the process
        // (Windows 0xc0000409). Windows must cover every sample and never
        // exceed the length that already transcribes successfully.
        let samples = TARGET_SAMPLE_RATE as usize * 409;
        let windows = transcription_windows(samples);

        assert!(windows.len() > 1);
        assert_eq!(windows[0], (0, TRANSCRIBE_WINDOW_SAMPLES));
        assert_eq!(windows.last().map(|(_, end)| *end), Some(samples));
        assert!(
            windows
                .iter()
                .all(|(start, end)| end.saturating_sub(*start) <= TRANSCRIBE_WINDOW_SAMPLES)
        );
        let mut covered = vec![false; samples];
        for (start, end) in &windows {
            covered[*start..*end].fill(true);
        }
        assert!(covered.iter().all(|sample| *sample));
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
    fn the_graphics_card_is_tried_before_the_processor() {
        assert_eq!(PROVIDERS.first().map(|(_, kind)| *kind), Some(Accelerator::Gpu));
    }

    #[test]
    fn the_processor_is_always_the_last_resort() {
        // Whatever the machine has, one entry in this list cannot fail to
        // start, or a user without a supported card would be left with an app
        // that refuses to load anything.
        assert_eq!(PROVIDERS.last().map(|(name, kind)| (*name, *kind)), Some(("cpu", Accelerator::Cpu)));
    }

    #[test]
    fn decode_threads_never_exceed_the_measured_sweet_spot() {
        // Sixteen threads measured slower than four on this workload.
        assert!((1..=4).contains(&decode_threads()));
    }

    #[test]
    fn a_model_directory_missing_a_file_is_not_installed() {
        let temp = tempfile::tempdir().unwrap();
        let engine = Engine::new(temp.path());
        let directory = engine.model_dir("parakeet");
        std::fs::create_dir_all(&directory).unwrap();
        for name in ["encoder.int8.onnx", "decoder.int8.onnx", "joiner.int8.onnx"] {
            std::fs::write(directory.join(name), b"x").unwrap();
        }

        assert!(!engine.is_installed("parakeet"), "tokens.txt is missing");

        std::fs::write(directory.join("tokens.txt"), b"x").unwrap();
        assert!(engine.is_installed("parakeet"));
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

    #[test]
    fn loading_a_model_that_is_not_there_says_so() {
        let temp = tempfile::tempdir().unwrap();
        let engine = Engine::new(temp.path());

        let error = engine.ensure_loaded("parakeet").unwrap_err();

        assert!(matches!(error, EngineError::ModelIncomplete { .. }), "{error:?}");
        assert_eq!(engine.loaded_model(), None);
    }
}
