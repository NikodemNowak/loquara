use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use serde::{Deserialize, Serialize};
use std::fs::{self, OpenOptions};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, SyncSender, TrySendError};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use thiserror::Error;

static NEXT_RECORDING: AtomicU64 = AtomicU64::new(0);

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InputDeviceInfo {
    pub id: String,
    pub name: String,
    pub is_default: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AudioFormat {
    pub channels: u16,
    pub sample_rate: u32,
}

#[derive(Clone, Debug)]
pub enum AudioSamples {
    I8(Vec<i8>),
    F32(Vec<f32>),
    I16(Vec<i16>),
    I24(Vec<cpal::I24>),
    I32(Vec<i32>),
    I64(Vec<i64>),
    U8(Vec<u8>),
    U16(Vec<u16>),
    U32(Vec<u32>),
    U64(Vec<u64>),
    F64(Vec<f64>),
    Error(String),
}

impl AudioSamples {
    fn as_f32(&self) -> Vec<f32> {
        self.pcm16()
            .unwrap_or_default()
            .into_iter()
            .map(|sample| f32::from(sample) / 32768.0)
            .collect()
    }

    fn pcm16(&self) -> Result<Vec<i16>, AudioError> {
        match self {
            Self::I8(samples) => Ok(samples
                .iter()
                .map(|sample| i16::from(*sample) << 8)
                .collect()),
            Self::I16(samples) => Ok(samples.clone()),
            Self::I24(samples) => Ok(samples
                .iter()
                .map(|sample| (sample.inner() >> 8) as i16)
                .collect()),
            Self::I32(samples) => Ok(samples_i32_to_i16(samples)),
            Self::I64(samples) => Ok(samples
                .iter()
                .map(|sample| (*sample >> 48) as i16)
                .collect()),
            Self::U8(samples) => Ok(samples
                .iter()
                .map(|sample| (i16::from(*sample) - 128) << 8)
                .collect()),
            Self::U16(samples) => Ok(samples_u16_to_i16(samples)),
            Self::U32(samples) => Ok(samples_u32_to_i16(samples)),
            Self::U64(samples) => Ok(samples
                .iter()
                .map(|sample| {
                    let centered = i128::from(*sample) - (1_i128 << 63);
                    (centered >> 48).clamp(i128::from(i16::MIN), i128::from(i16::MAX)) as i16
                })
                .collect()),
            Self::F32(samples) => Ok(samples_f32_to_i16(samples)),
            Self::F64(samples) => Ok(samples_f64_to_i16(samples)),
            Self::Error(message) => Err(AudioError::Io(message.clone())),
        }
    }

    fn write_to(
        self,
        writer: &mut hound::WavWriter<std::io::BufWriter<std::fs::File>>,
    ) -> Result<(), AudioError> {
        for sample in self.pcm16()? {
            writer.write_sample(sample).map_err(AudioError::from)?;
        }
        Ok(())
    }
}

#[derive(Debug, Error, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "code", content = "message", rename_all = "snake_case")]
pub enum AudioError {
    #[error("recording is already active")]
    AlreadyRecording,
    #[error("recording is not active")]
    NotRecording,
    #[error("no input device is available")]
    NoInputDevice,
    #[error("audio input buffer overflowed")]
    BufferOverflow,
    #[error("audio I/O failed: {0}")]
    Io(String),
}

impl From<std::io::Error> for AudioError {
    fn from(error: std::io::Error) -> Self {
        Self::Io(error.to_string())
    }
}

impl From<hound::Error> for AudioError {
    fn from(error: hound::Error) -> Self {
        Self::Io(error.to_string())
    }
}

pub trait ActiveInput: Send {}
impl<T: Send> ActiveInput for T {}

pub trait InputBackend: Send + Sync {
    fn list_devices(&self) -> Result<Vec<InputDeviceInfo>, AudioError>;
    fn format(&self, device_id: Option<&str>) -> Result<AudioFormat, AudioError>;
    fn start(
        &self,
        device_id: Option<&str>,
        samples: SyncSender<AudioSamples>,
        overflowed: Arc<AtomicBool>,
    ) -> Result<Box<dyn ActiveInput>, AudioError>;
}

fn try_send_samples(
    sender: &SyncSender<AudioSamples>,
    overflowed: &AtomicBool,
    samples: AudioSamples,
) {
    if matches!(sender.try_send(samples), Err(TrySendError::Full(_))) {
        overflowed.store(true, Ordering::Release);
    }
}

pub struct CpalInputBackend;

impl CpalInputBackend {
    fn device(&self, requested: Option<&str>) -> Result<cpal::Device, AudioError> {
        let host = cpal::default_host();
        if let Some(requested) = requested {
            let devices = host
                .input_devices()
                .map_err(|error| AudioError::Io(error.to_string()))?;
            for device in devices {
                if device.name().ok().as_deref() == Some(requested) {
                    return Ok(device);
                }
            }
            return Err(AudioError::NoInputDevice);
        }
        host.default_input_device().ok_or(AudioError::NoInputDevice)
    }
}

impl InputBackend for CpalInputBackend {
    fn list_devices(&self) -> Result<Vec<InputDeviceInfo>, AudioError> {
        let host = cpal::default_host();
        let default_name = host
            .default_input_device()
            .and_then(|device| device.name().ok());
        host.input_devices()
            .map_err(|error| AudioError::Io(error.to_string()))?
            .map(|device| {
                let name = device
                    .name()
                    .map_err(|error| AudioError::Io(error.to_string()))?;
                Ok(InputDeviceInfo {
                    id: name.clone(),
                    is_default: default_name.as_deref() == Some(name.as_str()),
                    name,
                })
            })
            .collect()
    }

    fn format(&self, device_id: Option<&str>) -> Result<AudioFormat, AudioError> {
        let config = self
            .device(device_id)?
            .default_input_config()
            .map_err(|error| AudioError::Io(error.to_string()))?;
        Ok(AudioFormat {
            channels: config.channels(),
            sample_rate: config.sample_rate().0,
        })
    }

    fn start(
        &self,
        device_id: Option<&str>,
        samples: SyncSender<AudioSamples>,
        overflowed: Arc<AtomicBool>,
    ) -> Result<Box<dyn ActiveInput>, AudioError> {
        let device = self.device(device_id)?;
        let supported = device
            .default_input_config()
            .map_err(|error| AudioError::Io(error.to_string()))?;
        let config = supported.config();
        let errors = samples.clone();
        let error_overflowed = overflowed.clone();
        let error_callback = move |error: cpal::StreamError| {
            try_send_samples(
                &errors,
                &error_overflowed,
                AudioSamples::Error(error.to_string()),
            );
        };
        let stream = match supported.sample_format() {
            cpal::SampleFormat::I8 => device.build_input_stream(
                &config,
                move |data: &[i8], _| {
                    try_send_samples(&samples, &overflowed, AudioSamples::I8(data.to_vec()));
                },
                error_callback,
                None,
            ),
            cpal::SampleFormat::F32 => device.build_input_stream(
                &config,
                move |data: &[f32], _| {
                    try_send_samples(&samples, &overflowed, AudioSamples::F32(data.to_vec()));
                },
                error_callback,
                None,
            ),
            cpal::SampleFormat::I16 => device.build_input_stream(
                &config,
                move |data: &[i16], _| {
                    try_send_samples(&samples, &overflowed, AudioSamples::I16(data.to_vec()));
                },
                error_callback,
                None,
            ),
            cpal::SampleFormat::I24 => device.build_input_stream(
                &config,
                move |data: &[cpal::I24], _| {
                    try_send_samples(&samples, &overflowed, AudioSamples::I24(data.to_vec()));
                },
                error_callback,
                None,
            ),
            cpal::SampleFormat::I32 => device.build_input_stream(
                &config,
                move |data: &[i32], _| {
                    try_send_samples(&samples, &overflowed, AudioSamples::I32(data.to_vec()));
                },
                error_callback,
                None,
            ),
            cpal::SampleFormat::I64 => device.build_input_stream(
                &config,
                move |data: &[i64], _| {
                    try_send_samples(&samples, &overflowed, AudioSamples::I64(data.to_vec()));
                },
                error_callback,
                None,
            ),
            cpal::SampleFormat::U8 => device.build_input_stream(
                &config,
                move |data: &[u8], _| {
                    try_send_samples(&samples, &overflowed, AudioSamples::U8(data.to_vec()));
                },
                error_callback,
                None,
            ),
            cpal::SampleFormat::U16 => device.build_input_stream(
                &config,
                move |data: &[u16], _| {
                    try_send_samples(&samples, &overflowed, AudioSamples::U16(data.to_vec()));
                },
                error_callback,
                None,
            ),
            cpal::SampleFormat::U32 => device.build_input_stream(
                &config,
                move |data: &[u32], _| {
                    try_send_samples(&samples, &overflowed, AudioSamples::U32(data.to_vec()));
                },
                error_callback,
                None,
            ),
            cpal::SampleFormat::U64 => device.build_input_stream(
                &config,
                move |data: &[u64], _| {
                    try_send_samples(&samples, &overflowed, AudioSamples::U64(data.to_vec()));
                },
                error_callback,
                None,
            ),
            cpal::SampleFormat::F64 => device.build_input_stream(
                &config,
                move |data: &[f64], _| {
                    try_send_samples(&samples, &overflowed, AudioSamples::F64(data.to_vec()));
                },
                error_callback,
                None,
            ),
            other => {
                return Err(AudioError::Io(format!(
                    "unsupported input sample format: {other}"
                )));
            }
        }
        .map_err(|error| AudioError::Io(error.to_string()))?;
        stream
            .play()
            .map_err(|error| AudioError::Io(error.to_string()))?;
        Ok(Box::new(stream))
    }
}

enum WriterMessage {
    Samples(AudioSamples),
    Finish,
}

struct ActiveRecording {
    id: String,
    path: PathBuf,
    part_path: PathBuf,
    started_at: Instant,
    sender: SyncSender<WriterMessage>,
    bridge: JoinHandle<()>,
    writer: JoinHandle<Result<(), AudioError>>,
    overflowed: Arc<AtomicBool>,
    _input: Box<dyn ActiveInput>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StartedRecording {
    pub id: String,
    pub path: PathBuf,
    pub part_path: PathBuf,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CompletedRecording {
    pub id: String,
    pub path: PathBuf,
    pub duration_ms: u64,
}

pub struct AudioRecorder {
    recordings_dir: PathBuf,
    backend: Arc<dyn InputBackend>,
    active: Mutex<Option<ActiveRecording>>,
    level_sender: Mutex<Option<SyncSender<f32>>>,
}

impl AudioRecorder {
    pub fn new(recordings_dir: impl Into<PathBuf>) -> Self {
        Self::with_backend(recordings_dir, Arc::new(CpalInputBackend))
    }

    pub fn with_backend(
        recordings_dir: impl Into<PathBuf>,
        backend: Arc<dyn InputBackend>,
    ) -> Self {
        Self {
            recordings_dir: recordings_dir.into(),
            backend,
            active: Mutex::new(None),
            level_sender: Mutex::new(None),
        }
    }

    pub fn set_level_sender(&self, sender: SyncSender<f32>) {
        if let Ok(mut level_sender) = self.level_sender.lock() {
            *level_sender = Some(sender);
        }
    }

    pub fn list_devices(&self) -> Result<Vec<InputDeviceInfo>, AudioError> {
        self.backend.list_devices()
    }

    pub fn start(&self, device_id: Option<&str>) -> Result<StartedRecording, AudioError> {
        let mut active = self
            .active
            .lock()
            .map_err(|_| AudioError::Io("recorder lock poisoned".into()))?;
        if active.is_some() {
            return Err(AudioError::AlreadyRecording);
        }
        fs::create_dir_all(&self.recordings_dir)?;
        let epoch_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis()
            .try_into()
            .unwrap_or(u64::MAX);
        let nonce = NEXT_RECORDING.fetch_add(1, Ordering::Relaxed);
        let id = recording_id(epoch_ms, nonce);
        let path = self.recordings_dir.join(format!("{id}.wav"));
        let part_path = part_path_for(&path);
        let format = self.backend.format(device_id)?;
        let spec = hound::WavSpec {
            channels: format.channels,
            sample_rate: format.sample_rate,
            bits_per_sample: 16,
            sample_format: hound::SampleFormat::Int,
        };
        let writer = hound::WavWriter::create(&part_path, spec)?;
        let (packet_sender, packet_receiver) = mpsc::sync_channel(16);
        let (writer_sender, writer_receiver) = mpsc::sync_channel(16);
        let overflowed = Arc::new(AtomicBool::new(false));
        let level_sender = self
            .level_sender
            .lock()
            .ok()
            .and_then(|sender| sender.clone());
        let bridge_sender = writer_sender.clone();
        let input = match self
            .backend
            .start(device_id, packet_sender, overflowed.clone())
        {
            Ok(input) => input,
            Err(error) => {
                drop(writer);
                let _ = fs::remove_file(&part_path);
                return Err(error);
            }
        };
        let bridge = thread::spawn(move || {
            while let Ok(packet) = packet_receiver.recv() {
                if bridge_sender.send(WriterMessage::Samples(packet)).is_err() {
                    break;
                }
            }
        });
        let writer_handle = thread::spawn(move || {
            let mut writer = writer;
            let mut last_level = Instant::now() - Duration::from_millis(40);
            while let Ok(message) = writer_receiver.recv() {
                match message {
                    WriterMessage::Samples(samples) => {
                        if last_level.elapsed() >= Duration::from_millis(40) {
                            if let Some(sender) = &level_sender {
                                let _ = sender.try_send(normalized_rms(&samples.as_f32()));
                            }
                            last_level = Instant::now();
                        }
                        samples.write_to(&mut writer)?;
                    }
                    WriterMessage::Finish => break,
                }
            }
            writer.finalize().map_err(AudioError::from)?;
            Ok(())
        });
        let started = StartedRecording {
            id: id.clone(),
            path: path.clone(),
            part_path: part_path.clone(),
        };
        *active = Some(ActiveRecording {
            id,
            path,
            part_path,
            started_at: Instant::now(),
            sender: writer_sender,
            bridge,
            writer: writer_handle,
            overflowed,
            _input: input,
        });
        Ok(started)
    }

    pub fn stop(&self) -> Result<CompletedRecording, AudioError> {
        self.finish(false)
    }

    pub fn cancel(&self) -> Result<CompletedRecording, AudioError> {
        self.finish(true)
    }

    fn finish(&self, cancel: bool) -> Result<CompletedRecording, AudioError> {
        let recording = self
            .active
            .lock()
            .map_err(|_| AudioError::Io("recorder lock poisoned".into()))?
            .take()
            .ok_or(AudioError::NotRecording)?;
        let ActiveRecording {
            id,
            path,
            part_path,
            started_at,
            sender,
            bridge,
            writer,
            overflowed,
            _input,
        } = recording;
        let duration_ms = started_at
            .elapsed()
            .as_millis()
            .try_into()
            .unwrap_or(u64::MAX);
        drop(_input);
        let result = (|| {
            bridge
                .join()
                .map_err(|_| AudioError::Io("audio bridge thread panicked".into()))?;
            let finish_result = sender
                .send(WriterMessage::Finish)
                .map_err(|error| AudioError::Io(error.to_string()));
            let writer_result = writer
                .join()
                .map_err(|_| AudioError::Io("audio writer thread panicked".into()))?;
            writer_result?;
            finish_result?;
            if overflowed.load(Ordering::Acquire) {
                return Err(AudioError::BufferOverflow);
            }
            if cancel {
                fs::remove_file(&part_path)?;
            } else {
                finalize_atomic(&part_path, &path)?;
            }
            Ok::<(), AudioError>(())
        })();
        if let Err(error) = result {
            let _ = fs::remove_file(&part_path);
            return Err(error);
        }
        Ok(CompletedRecording {
            id,
            path,
            duration_ms,
        })
    }
}

pub fn cleanup_partial(final_path: &Path) -> Result<(), AudioError> {
    let part_path = part_path_for(final_path);
    match fs::remove_file(part_path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(AudioError::Io(error.to_string())),
    }
}

pub fn samples_f32_to_i16(samples: &[f32]) -> Vec<i16> {
    samples
        .iter()
        .map(|sample| {
            let sample = if sample.is_finite() { *sample } else { 0.0 };
            if sample <= -1.0 {
                i16::MIN
            } else if sample >= 1.0 {
                i16::MAX
            } else {
                (sample * 32768.0).round() as i16
            }
        })
        .collect()
}

pub fn samples_u16_to_i16(samples: &[u16]) -> Vec<i16> {
    samples
        .iter()
        .map(|sample| {
            let shifted = i32::from(*sample) - 32768;
            shifted.clamp(i32::from(i16::MIN), i32::from(i16::MAX)) as i16
        })
        .collect()
}

pub fn samples_i32_to_i16(samples: &[i32]) -> Vec<i16> {
    samples
        .iter()
        .map(|sample| (*sample >> 16).clamp(i32::from(i16::MIN), i32::from(i16::MAX)) as i16)
        .collect()
}

pub fn samples_u32_to_i16(samples: &[u32]) -> Vec<i16> {
    samples
        .iter()
        .map(|sample| {
            let centered = i64::from(*sample) - (1_i64 << 31);
            (centered >> 16).clamp(i64::from(i16::MIN), i64::from(i16::MAX)) as i16
        })
        .collect()
}

pub fn samples_f64_to_i16(samples: &[f64]) -> Vec<i16> {
    samples
        .iter()
        .map(|sample| {
            let sample = if sample.is_finite() { *sample } else { 0.0 };
            if sample <= -1.0 {
                i16::MIN
            } else if sample >= 1.0 {
                i16::MAX
            } else {
                (sample * 32768.0).round() as i16
            }
        })
        .collect()
}

pub fn normalized_rms(samples: &[f32]) -> f32 {
    if samples.is_empty() {
        return 0.0;
    }
    let mut valid = 0usize;
    let sum = samples
        .iter()
        .filter_map(|sample| {
            if !sample.is_finite() {
                return None;
            }
            valid += 1;
            let value = sample.abs().min(1.0);
            Some(value * value)
        })
        .sum::<f32>();
    if valid == 0 {
        0.0
    } else {
        (sum / valid as f32).sqrt().clamp(0.0, 1.0)
    }
}

fn recording_id(epoch_ms: u64, nonce: u64) -> String {
    format!("recording-{epoch_ms:013}-{nonce:010}")
}

pub fn recording_filename(epoch_ms: u64, nonce: u64) -> String {
    format!("{}.wav", recording_id(epoch_ms, nonce))
}

pub fn part_path_for(final_path: &Path) -> PathBuf {
    let mut path = final_path.as_os_str().to_owned();
    path.push(".part");
    PathBuf::from(path)
}

pub fn finalize_atomic(part_path: &Path, final_path: &Path) -> Result<(), AudioError> {
    if final_path.exists() {
        return Err(AudioError::Io(format!(
            "refusing to overwrite {}",
            final_path.display()
        )));
    }
    let file = OpenOptions::new().read(true).write(true).open(part_path)?;
    file.sync_all()?;
    drop(file);
    fs::rename(part_path, final_path)?;
    if let Some(parent) = final_path.parent()
        && let Ok(directory) = OpenOptions::new().read(true).open(parent)
    {
        let _ = directory.sync_all();
    }
    Ok(())
}

#[cfg(test)]
#[derive(Default)]
struct FakeInputBackend {
    samples: Vec<AudioSamples>,
}

#[cfg(test)]
impl FakeInputBackend {
    fn with_samples(samples: Vec<AudioSamples>) -> Self {
        Self { samples }
    }
}

#[cfg(test)]
impl InputBackend for FakeInputBackend {
    fn list_devices(&self) -> Result<Vec<InputDeviceInfo>, AudioError> {
        Ok(vec![InputDeviceInfo {
            id: "fake".into(),
            name: "Fake microphone".into(),
            is_default: true,
        }])
    }

    fn format(&self, _device_id: Option<&str>) -> Result<AudioFormat, AudioError> {
        Ok(AudioFormat {
            channels: 1,
            sample_rate: 48_000,
        })
    }

    fn start(
        &self,
        _device_id: Option<&str>,
        sender: SyncSender<AudioSamples>,
        overflowed: Arc<AtomicBool>,
    ) -> Result<Box<dyn ActiveInput>, AudioError> {
        for samples in &self.samples {
            try_send_samples(&sender, &overflowed, samples.clone());
        }
        Ok(Box::new(()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::sync::Arc;

    #[test]
    fn converts_supported_samples_to_pcm16_without_overflow() {
        assert_eq!(
            samples_f32_to_i16(&[-1.0, -0.5, 0.0, 0.5, 1.0]),
            vec![i16::MIN, -16384, 0, 16384, i16::MAX]
        );
        assert_eq!(
            samples_u16_to_i16(&[u16::MIN, 32768, u16::MAX]),
            vec![i16::MIN, 0, i16::MAX]
        );
        assert_eq!(
            samples_i32_to_i16(&[i32::MIN, 0, i32::MAX]),
            vec![i16::MIN, 0, i16::MAX]
        );
        assert_eq!(
            samples_u32_to_i16(&[u32::MIN, 1 << 31, u32::MAX]),
            vec![i16::MIN, 0, i16::MAX]
        );
        assert_eq!(
            samples_f64_to_i16(&[-2.0, -1.0, 0.0, 1.0, 2.0]),
            vec![i16::MIN, i16::MIN, 0, i16::MAX, i16::MAX]
        );
    }

    #[test]
    fn rms_is_normalized_and_finite() {
        assert_eq!(normalized_rms(&[]), 0.0);
        assert!((normalized_rms(&[1.0, -1.0]) - 1.0).abs() < f32::EPSILON);
        assert_eq!(normalized_rms(&[f32::NAN, 2.0]), 1.0);
    }

    #[test]
    fn recording_names_are_sortable_unique_and_path_safe() {
        let first = recording_filename(1_722_268_800_000, 7);
        let second = recording_filename(1_722_268_800_001, 0);

        assert!(first < second);
        assert!(first.ends_with(".wav"));
        assert!(!first.contains(['/', '\\', ':']));
        assert_ne!(first, recording_filename(1_722_268_800_000, 8));
        assert_eq!(
            part_path_for(std::path::Path::new(&first))
                .extension()
                .unwrap(),
            "part"
        );
    }

    #[test]
    fn atomic_finalize_renames_part_and_refuses_overwrite() {
        let temp = tempfile::tempdir().unwrap();
        let final_path = temp.path().join("recording.wav");
        let part_path = part_path_for(&final_path);
        fs::write(&part_path, b"wav").unwrap();

        finalize_atomic(&part_path, &final_path).unwrap();
        assert_eq!(fs::read(&final_path).unwrap(), b"wav");
        assert!(!part_path.exists());

        fs::write(&part_path, b"new").unwrap();
        let error = finalize_atomic(&part_path, &final_path).unwrap_err();
        assert!(matches!(error, AudioError::Io(_)));
        assert_eq!(fs::read(&final_path).unwrap(), b"wav");
    }

    #[test]
    fn recorder_enforces_single_session_and_cancel_does_not_complete_audio() {
        let temp = tempfile::tempdir().unwrap();
        let backend = Arc::new(FakeInputBackend::default());
        let recorder = AudioRecorder::with_backend(temp.path(), backend);

        let active = recorder.start(None).unwrap();
        assert!(active.part_path.exists());
        assert_eq!(
            recorder.start(None).unwrap_err(),
            AudioError::AlreadyRecording
        );

        let cancelled = recorder.cancel().unwrap();
        assert_eq!(cancelled.id, active.id);
        assert!(!cancelled.path.exists());
        assert!(!active.part_path.exists());
        assert_eq!(recorder.cancel().unwrap_err(), AudioError::NotRecording);
    }

    #[test]
    fn recorder_finalizes_a_fake_input_session() {
        let temp = tempfile::tempdir().unwrap();
        let backend = Arc::new(FakeInputBackend::with_samples(vec![
            AudioSamples::I16(vec![0, 100, -100]),
            AudioSamples::F32(vec![0.5, -0.5]),
        ]));
        let recorder = AudioRecorder::with_backend(temp.path(), backend);

        let started = recorder.start(None).unwrap();
        let completed = recorder.stop().unwrap();

        assert_eq!(completed.id, started.id);
        assert!(completed.path.is_file());
        assert!(!started.part_path.exists());
        let reader = hound::WavReader::open(completed.path).unwrap();
        assert_eq!(reader.spec().sample_rate, 48_000);
        assert_eq!(reader.spec().channels, 1);
    }

    #[test]
    fn recorder_surfaces_asynchronous_input_errors_as_io() {
        let temp = tempfile::tempdir().unwrap();
        let backend = Arc::new(FakeInputBackend::with_samples(vec![AudioSamples::Error(
            "device disconnected".into(),
        )]));
        let recorder = AudioRecorder::with_backend(temp.path(), backend);
        let started = recorder.start(None).unwrap();

        let error = recorder.stop().unwrap_err();

        assert_eq!(error, AudioError::Io("device disconnected".into()));
        assert!(!started.part_path.exists());
        assert!(recorder.start(None).is_ok());
    }

    #[test]
    fn shutdown_preserves_device_error_after_writer_has_already_exited() {
        let temp = tempfile::tempdir().unwrap();
        let backend = Arc::new(FakeInputBackend::with_samples(vec![AudioSamples::Error(
            "device disconnected".into(),
        )]));
        let recorder = AudioRecorder::with_backend(temp.path(), backend);
        recorder.start(None).unwrap();
        while !recorder
            .active
            .lock()
            .unwrap()
            .as_ref()
            .unwrap()
            .writer
            .is_finished()
        {
            std::thread::yield_now();
        }

        assert_eq!(
            recorder.stop().unwrap_err(),
            AudioError::Io("device disconnected".into())
        );
    }

    #[test]
    fn callback_burst_overflow_is_bounded_cleans_partial_and_allows_restart() {
        let temp = tempfile::tempdir().unwrap();
        let backend = Arc::new(FakeInputBackend::with_samples(
            (0..10_000)
                .map(|_| AudioSamples::I16(vec![1; 64]))
                .collect(),
        ));
        let recorder = AudioRecorder::with_backend(temp.path(), backend);
        let started = recorder.start(None).unwrap();

        assert_eq!(recorder.stop().unwrap_err(), AudioError::BufferOverflow);
        assert!(!started.part_path.exists());
        assert!(!started.path.exists());
        assert!(recorder.start(None).is_ok());
        assert_eq!(recorder.cancel().unwrap_err(), AudioError::BufferOverflow);
    }
}
