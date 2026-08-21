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
use std::sync::Mutex;

use rubato::{Resampler, SincFixedIn, SincInterpolationParameters, SincInterpolationType, WindowFunction};
use sherpa_rs::transducer::{TransducerConfig, TransducerRecognizer};

/// Every model Loquara ships is trained on 16 kHz mono audio.
pub const TARGET_SAMPLE_RATE: u32 = 16_000;

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
}

/// Holds at most one loaded model.
///
/// Guarded by a mutex because dictation runs on a background thread while the
/// interface may ask to change models from another.
pub struct Engine {
    models_dir: PathBuf,
    loaded: Mutex<Option<Loaded>>,
}

impl Engine {
    pub fn new(models_dir: impl Into<PathBuf>) -> Self {
        Self {
            models_dir: models_dir.into(),
            loaded: Mutex::new(None),
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
        self.loaded
            .lock()
            .ok()
            .and_then(|guard| guard.as_ref().map(|loaded| loaded.key.clone()))
    }

    /// Releases the model, returning its memory to the system.
    pub fn unload(&self) {
        if let Ok(mut guard) = self.loaded.lock() {
            *guard = None;
        }
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
            return Err(EngineError::ModelIncomplete {
                model: key.to_owned(),
            });
        }
        // Drop the previous model before building the next one, so two never
        // sit in memory at the same time.
        *guard = None;

        let config = TransducerConfig {
            encoder: files[0].to_string_lossy().into_owned(),
            decoder: files[1].to_string_lossy().into_owned(),
            joiner: files[2].to_string_lossy().into_owned(),
            tokens: files[3].to_string_lossy().into_owned(),
            model_type: "nemo_transducer".into(),
            num_threads: decode_threads(),
            decoding_method: "greedy_search".into(),
            debug: false,
            ..Default::default()
        };
        let recogniser =
            TransducerRecognizer::new(config).map_err(|error| EngineError::Load(error.to_string()))?;
        *guard = Some(Loaded {
            key: key.to_owned(),
            recogniser,
        });
        Ok(())
    }

    /// Transcribes a recording, loading the model first if necessary.
    pub fn transcribe(&self, key: &str, audio: &Path) -> Result<String, EngineError> {
        let samples = read_as_target_rate(audio)?;
        self.ensure_loaded(key)?;
        let mut guard = self
            .loaded
            .lock()
            .map_err(|_| EngineError::Decode("engine lock poisoned".into()))?;
        let loaded = guard
            .as_mut()
            .ok_or_else(|| EngineError::Decode("model was unloaded mid-transcription".into()))?;
        Ok(loaded
            .recogniser
            .transcribe(TARGET_SAMPLE_RATE, &samples)
            .trim()
            .to_owned())
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
    let mut resampler = SincFixedIn::<f32>::new(ratio, 1.0, parameters, samples.len(), 1)
        .map_err(|error| EngineError::Audio(error.to_string()))?;
    let output = resampler
        .process(&[samples], None)
        .map_err(|error| EngineError::Audio(error.to_string()))?;
    Ok(output.into_iter().next().unwrap_or_default())
}

#[cfg(test)]
mod tests {
    use super::*;

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
    fn an_empty_recording_resamples_to_nothing() {
        assert!(resample(Vec::new(), 48_000).unwrap().is_empty());
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
        let text = engine.transcribe("parakeet", Path::new(&wav)).unwrap();
        let first = started.elapsed();

        // Loading is the expensive part, so the second pass proves the model
        // is still resident rather than rebuilt.
        let started = std::time::Instant::now();
        let again = engine.transcribe("parakeet", Path::new(&wav)).unwrap();
        let second = started.elapsed();

        println!("first: {first:?}  second: {second:?}");
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
