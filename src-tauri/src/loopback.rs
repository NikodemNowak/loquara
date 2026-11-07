//! Capturing what the computer plays: the other side of a meeting.
//!
//! Each platform reaches the output differently. Windows opens the default
//! render endpoint in WASAPI loopback mode, macOS (14.6 and later) attaches a
//! Core Audio process tap through an aggregate device, and Linux records the
//! default sink's monitor through the PulseAudio protocol, which PipeWire
//! serves as well. All three end in the same `PreparedInput`, so the recorder
//! that writes dictations to disk writes this track the same way.

use crate::audio::{
    ActiveInput, AudioError, AudioFormat, AudioSamples, CaptureFaultSink, InputBackend,
    InputDeviceInfo, PreparedInput,
};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError, SyncSender};
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

/// How long the output may stay quiet before the gap is written as silence.
///
/// WASAPI delivers no packets at all while nothing is playing. Left alone the
/// track would come out shorter than the meeting and every later segment
/// would be stamped too early; filling the gap keeps both tracks on one clock.
const GAP_TOLERANCE: Duration = Duration::from_millis(150);

/// Silence is written up to this far behind the wall clock, leaving room for
/// a packet that is already on its way.
const GAP_MARGIN: Duration = Duration::from_millis(50);

pub struct LoopbackBackend;

impl InputBackend for LoopbackBackend {
    fn list_devices(&self) -> Result<Vec<InputDeviceInfo>, AudioError> {
        Ok(Vec::new())
    }

    fn prepare(&self, _device_id: Option<&str>) -> Result<Box<dyn PreparedInput>, AudioError> {
        let inner = platform::prepare()?;
        Ok(Box::new(GapFilledInput { inner }))
    }
}

/// Feeds the recorder from `inner`, writing silence wherever the source
/// stopped delivering for longer than `GAP_TOLERANCE`.
struct GapFilledInput {
    inner: Box<dyn PreparedInput>,
}

impl PreparedInput for GapFilledInput {
    fn device_id(&self) -> &str {
        self.inner.device_id()
    }

    fn format(&self) -> AudioFormat {
        self.inner.format()
    }

    fn start(
        self: Box<Self>,
        samples: SyncSender<AudioSamples>,
        overflowed: Arc<AtomicBool>,
        fault: CaptureFaultSink,
    ) -> Result<Box<dyn ActiveInput>, AudioError> {
        let format = self.inner.format();
        let (source_sender, source_receiver) = mpsc::sync_channel(256);
        let input = self.inner.start(source_sender, overflowed.clone(), fault)?;
        let stop = Arc::new(AtomicBool::new(false));
        let thread_stop = stop.clone();
        let filler = thread::spawn(move || {
            let mut clock = GapClock::new(format, Instant::now());
            loop {
                if thread_stop.load(Ordering::Acquire) {
                    break;
                }
                match source_receiver.recv_timeout(GAP_TOLERANCE / 3) {
                    Ok(packet) => {
                        clock.delivered(packet.len());
                        crate::audio::try_send_samples(&samples, &overflowed, packet);
                    }
                    Err(RecvTimeoutError::Timeout) => {
                        if let Some(silence) = clock.silence_due(Instant::now()) {
                            crate::audio::try_send_samples(&samples, &overflowed, AudioSamples::I16(silence));
                        }
                    }
                    Err(RecvTimeoutError::Disconnected) => break,
                }
            }
        });
        Ok(Box::new(ActiveGapFilled { input: Some(input), stop, filler: Some(filler) }))
    }
}

struct ActiveGapFilled {
    input: Option<Box<dyn ActiveInput>>,
    stop: Arc<AtomicBool>,
    filler: Option<JoinHandle<()>>,
}

impl Drop for ActiveGapFilled {
    fn drop(&mut self) {
        // Close the source first so nothing arrives after the filler is gone.
        drop(self.input.take());
        self.stop.store(true, Ordering::Release);
        if let Some(filler) = self.filler.take() {
            let _ = filler.join();
        }
    }
}

/// Bookkeeping for how much audio a track holds against how long it has run.
struct GapClock {
    format: AudioFormat,
    started: Instant,
    last_packet: Instant,
    frames: u64,
}

impl GapClock {
    fn new(format: AudioFormat, started: Instant) -> Self {
        Self { format, started, last_packet: started, frames: 0 }
    }

    fn delivered(&mut self, samples: usize) {
        self.frames += (samples / usize::from(self.format.channels.max(1))) as u64;
        self.last_packet = Instant::now();
    }

    /// Interleaved zeros that bring the track up to the wall clock, when the
    /// source has been quiet for long enough to count as a gap.
    fn silence_due(&mut self, now: Instant) -> Option<Vec<i16>> {
        if now.duration_since(self.last_packet) < GAP_TOLERANCE {
            return None;
        }
        let target = now.duration_since(self.started).saturating_sub(GAP_MARGIN);
        let expected = (target.as_secs_f64() * f64::from(self.format.sample_rate)) as u64;
        let missing = expected.saturating_sub(self.frames);
        if missing == 0 {
            return None;
        }
        self.frames += missing;
        Some(vec![0; missing as usize * usize::from(self.format.channels.max(1))])
    }
}

#[cfg(any(windows, target_os = "macos"))]
mod platform {
    use super::*;
    use cpal::traits::{DeviceTrait, HostTrait};

    pub(super) fn prepare() -> Result<Box<dyn PreparedInput>, AudioError> {
        let host = cpal::default_host();
        let device = host.default_output_device().ok_or_else(|| {
            AudioError::Io("no audio output device is available to record from".into())
        })?;
        let id = device
            .id()
            .map(|id| id.to_string())
            .unwrap_or_else(|_| "system-output".into());
        let supported = device
            .default_output_config()
            .map_err(|error| AudioError::Io(error.to_string()))?;
        Ok(Box::new(crate::audio::PreparedCpalInput::new(device, id, supported)))
    }
}

#[cfg(not(any(windows, target_os = "macos")))]
mod platform {
    use super::*;
    use std::io::Read;
    use std::process::{Child, Command, Stdio};

    const RATE: u32 = 16_000;

    pub(super) fn prepare() -> Result<Box<dyn PreparedInput>, AudioError> {
        Ok(Box::new(PreparedMonitor))
    }

    /// The default sink's monitor, read through `parec`.
    ///
    /// cpal's ALSA host cannot see monitor sources, and its PipeWire host
    /// would replace the microphone host for dictation too. `parec` speaks to
    /// PulseAudio and to PipeWire's Pulse server alike and resamples on the
    /// server, so the track arrives as 16 kHz mono.
    struct PreparedMonitor;

    impl PreparedInput for PreparedMonitor {
        fn device_id(&self) -> &str {
            "@DEFAULT_MONITOR@"
        }

        fn format(&self) -> AudioFormat {
            AudioFormat { channels: 1, sample_rate: RATE }
        }

        fn start(
            self: Box<Self>,
            samples: SyncSender<AudioSamples>,
            overflowed: Arc<AtomicBool>,
            fault: CaptureFaultSink,
        ) -> Result<Box<dyn ActiveInput>, AudioError> {
            let mut child = Command::new("parec")
                .args([
                    "--device=@DEFAULT_MONITOR@",
                    "--format=s16le",
                    &format!("--rate={RATE}"),
                    "--channels=1",
                    "--raw",
                    "--latency-msec=100",
                ])
                .stdout(Stdio::piped())
                .stderr(Stdio::null())
                .spawn()
                .map_err(|error| {
                    AudioError::Io(format!(
                        "system audio capture needs `parec` (pulseaudio-utils or pipewire-pulse): {error}"
                    ))
                })?;
            let mut stdout = child
                .stdout
                .take()
                .ok_or_else(|| AudioError::Io("parec has no output".into()))?;
            let reader = thread::spawn(move || {
                let mut buffer = vec![0_u8; 3_200];
                let mut carry: Option<u8> = None;
                loop {
                    match stdout.read(&mut buffer) {
                        Ok(0) => {
                            fault.report("system audio capture stopped".into());
                            break;
                        }
                        Ok(read) => {
                            let mut bytes: Vec<u8> = carry.take().into_iter().collect();
                            bytes.extend_from_slice(&buffer[..read]);
                            if bytes.len() % 2 == 1 {
                                carry = bytes.pop();
                            }
                            let pcm: Vec<i16> = bytes
                                .chunks_exact(2)
                                .map(|pair| i16::from_le_bytes([pair[0], pair[1]]))
                                .collect();
                            crate::audio::try_send_samples(&samples, &overflowed, AudioSamples::I16(pcm));
                        }
                        Err(error) => {
                            fault.report(error.to_string());
                            break;
                        }
                    }
                }
            });
            Ok(Box::new(RunningMonitor { child, reader: Some(reader) }))
        }
    }

    struct RunningMonitor {
        child: Child,
        reader: Option<JoinHandle<()>>,
    }

    impl Drop for RunningMonitor {
        fn drop(&mut self) {
            let _ = self.child.kill();
            let _ = self.child.wait();
            if let Some(reader) = self.reader.take() {
                let _ = reader.join();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stereo_48k() -> AudioFormat {
        AudioFormat { channels: 2, sample_rate: 48_000 }
    }

    #[test]
    fn a_steady_source_needs_no_padding() {
        let started = Instant::now();
        let mut clock = GapClock::new(stereo_48k(), started);
        clock.delivered(48_000 * 2);
        clock.last_packet = started + Duration::from_secs(1);

        assert_eq!(clock.silence_due(started + Duration::from_millis(1_100)), None);
    }

    #[test]
    fn a_quiet_output_is_filled_up_to_the_wall_clock() {
        let started = Instant::now();
        let mut clock = GapClock::new(stereo_48k(), started);

        let silence = clock
            .silence_due(started + Duration::from_secs(2))
            .expect("two silent seconds are a gap");

        // Two seconds minus the margin, in interleaved stereo samples.
        let frames = (Duration::from_secs(2) - GAP_MARGIN).as_secs_f64() * 48_000.0;
        assert_eq!(silence.len(), frames as usize * 2);
        assert!(silence.iter().all(|sample| *sample == 0));
        // Asking again right away has nothing more to fill.
        assert_eq!(clock.silence_due(started + Duration::from_secs(2)), None);
    }

    /// Records the default output while a quiet tone plays on it, after a
    /// second of nothing: the quiet second has to come out as silence of the
    /// right length, not be skipped.
    ///
    /// ```text
    /// cargo test --lib loopback::tests::records -- --ignored --nocapture
    /// ```
    #[test]
    #[ignore = "plays a quiet tone on the default output device"]
    fn records_what_the_computer_plays() {
        use crate::audio::AudioRecorder;
        use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};

        let temp = tempfile::tempdir().unwrap();
        let recorder = AudioRecorder::with_backend(temp.path(), Arc::new(LoopbackBackend));
        let path = temp.path().join("others.wav");
        recorder.start_at(None, "loopback".into(), path.clone()).unwrap();
        std::thread::sleep(Duration::from_secs(1));

        let device = cpal::default_host().default_output_device().unwrap();
        let supported = device.default_output_config().unwrap();
        assert_eq!(supported.sample_format(), cpal::SampleFormat::F32, "the test tone is written as f32");
        let config = supported.config();
        let channels = usize::from(config.channels);
        let rate = config.sample_rate as f32;
        let mut phase = 0.0_f32;
        let stream = device
            .build_output_stream(
                config,
                move |data: &mut [f32], _| {
                    for frame in data.chunks_mut(channels) {
                        let value = (phase * std::f32::consts::TAU).sin() * 0.05;
                        phase = (phase + 440.0 / rate) % 1.0;
                        frame.fill(value);
                    }
                },
                |error| eprintln!("tone failed: {error}"),
                None,
            )
            .unwrap();
        stream.play().unwrap();
        std::thread::sleep(Duration::from_millis(1_500));
        drop(stream);
        std::thread::sleep(Duration::from_millis(500));
        let completed = recorder.stop().unwrap();

        let mut reader = hound::WavReader::open(&completed.path).unwrap();
        let spec = reader.spec();
        let samples: Vec<i16> = reader.samples::<i16>().collect::<Result<_, _>>().unwrap();
        let frames = samples.len() / usize::from(spec.channels);
        let seconds = frames as f32 / spec.sample_rate as f32;
        let peak_at = |from: f32, to: f32| {
            let range = (from * spec.sample_rate as f32) as usize * usize::from(spec.channels)
                ..((to * spec.sample_rate as f32) as usize * usize::from(spec.channels)).min(samples.len());
            samples[range].iter().map(|sample| sample.unsigned_abs()).max().unwrap_or(0)
        };
        println!("{seconds:.2}s at {} Hz x{}, peak before {}, during {}", spec.sample_rate, spec.channels, peak_at(0.0, 0.8), peak_at(1.3, 2.3));
        assert!((2.6..3.4).contains(&seconds), "the track has to keep wall-clock time, got {seconds:.2}s");
        assert!(peak_at(1.3, 2.3) > 500, "the tone has to be in the recording");
    }

    #[test]
    fn a_source_ahead_of_the_clock_is_never_padded() {
        let started = Instant::now();
        let mut clock = GapClock::new(stereo_48k(), started);
        clock.delivered(48_000 * 2 * 5);
        clock.last_packet = started;

        assert_eq!(clock.silence_due(started + Duration::from_secs(3)), None);
    }
}
