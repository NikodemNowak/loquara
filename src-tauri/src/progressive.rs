//! The final local transcription, started while the user is still speaking.
//!
//! The final pass cuts a take into 18-30 s windows at quiet points and decodes
//! each one. A window's boundaries depend only on the audio up to 30 s past
//! its start, so as soon as that much has been recorded the window is final:
//! it is cut here exactly as the final pass will cut it, decoded once in the
//! background, and its text kept. At stop, [`Engine::transcribe_prepared`]
//! runs the ordinary final pass over the whole take and decodes only the
//! windows it does not find in that cache, which is usually just the last.
//!
//! The capture is converted to 16 kHz mono with the same arithmetic and the
//! same resampler, fed the same chunks, as reading the finished WAV, so the
//! samples are bit-identical; every reused window is checked against a hash
//! of the samples the final pass sees. Anything that does not match is simply
//! decoded at stop, and anything unexpected falls back to the batch pass.

use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

use rubato::{Resampler, SincFixedIn};

use crate::engine::{
    self, Engine, EngineError, WindowCache, RESAMPLE_INPUT_CHUNK, TARGET_SAMPLE_RATE,
    TRANSCRIBE_MIN_WINDOW_SAMPLES, TRANSCRIBE_WINDOW_SAMPLES,
};

/// The capture as it arrives, converted exactly as
/// [`engine::read_as_target_rate`] converts the finished 16-bit WAV.
pub struct TargetRateStream {
    channels: u16,
    sample_rate: u32,
    /// Interleaved samples of a frame not yet complete.
    leftover: Vec<i16>,
    /// Mono frames waiting for a full resampler chunk.
    pending: Vec<f32>,
    resampler: Option<SincFixedIn<f32>>,
    frames: u64,
    output: Vec<f32>,
}

impl TargetRateStream {
    pub fn new(channels: u16, sample_rate: u32) -> Result<Self, EngineError> {
        if channels == 0 || sample_rate == 0 {
            return Err(EngineError::Audio("capture format has no channels or rate".into()));
        }
        let resampler = if sample_rate == TARGET_SAMPLE_RATE {
            None
        } else {
            Some(engine::target_rate_resampler(sample_rate)?)
        };
        Ok(Self {
            channels,
            sample_rate,
            leftover: Vec::new(),
            pending: Vec::new(),
            resampler,
            frames: 0,
            output: Vec::new(),
        })
    }

    pub fn push(&mut self, pcm: &[i16]) -> Result<(), EngineError> {
        self.leftover.extend_from_slice(pcm);
        let channels = usize::from(self.channels);
        let whole = self.leftover.len() / channels * channels;
        if whole == 0 {
            return Ok(());
        }
        // `read_as_target_rate` reads 16-bit samples as i32 and divides by 2^15.
        let interleaved: Vec<f32> = self.leftover[..whole]
            .iter()
            .map(|sample| i32::from(*sample) as f32 / i32::pow(2, 15) as f32)
            .collect();
        self.leftover.drain(..whole);
        let mono = engine::downmix(&interleaved, self.channels);
        self.frames += mono.len() as u64;
        match &mut self.resampler {
            None => self.output.extend_from_slice(&mono),
            Some(resampler) => {
                self.pending.extend_from_slice(&mono);
                let mut position = 0;
                while position + RESAMPLE_INPUT_CHUNK <= self.pending.len() {
                    let chunk = &self.pending[position..position + RESAMPLE_INPUT_CHUNK];
                    let waves = resampler
                        .process(&[chunk], None)
                        .map_err(|error| EngineError::Audio(error.to_string()))?;
                    if let Some(channel) = waves.first() {
                        self.output.extend_from_slice(channel);
                    }
                    position += RESAMPLE_INPUT_CHUNK;
                }
                self.pending.drain(..position);
            }
        }
        Ok(())
    }

    /// 16 kHz samples so far. All but the last few are final.
    pub fn output(&self) -> &[f32] {
        &self.output
    }

    /// Whole input frames consumed, comparable with the WAV's duration.
    pub fn frames(&self) -> u64 {
        self.frames
    }

    /// The whole take, identical to reading the finished WAV.
    pub fn finish(mut self) -> Result<Vec<f32>, EngineError> {
        let Some(mut resampler) = self.resampler.take() else {
            return Ok(self.output);
        };
        if self.frames == 0 {
            return Ok(Vec::new());
        }
        let waves = if self.pending.is_empty() {
            resampler.process_partial::<&[f32]>(None, None)
        } else {
            resampler.process_partial(Some(&[self.pending.as_slice()]), None)
        }
        .map_err(|error| EngineError::Audio(error.to_string()))?;
        if let Some(channel) = waves.first() {
            self.output.extend_from_slice(channel);
        }
        let expected = self.frames.saturating_mul(u64::from(TARGET_SAMPLE_RATE)) / u64::from(self.sample_rate);
        self.output.truncate(expected as usize);
        Ok(self.output)
    }
}

/// A window whose boundaries can no longer change.
struct Committed {
    start: usize,
    end: usize,
    hash: u64,
    /// Where the window ends in the 16 kHz capture, before silence shortening.
    source_end: usize,
    text: Option<String>,
}

/// A committed window waiting to be decoded.
pub struct ReadyWindow {
    pub index: usize,
    pub samples: Vec<f32>,
}

/// Cuts and caches final windows of a take still being recorded. No threads,
/// no model: [`ProgressiveSession`] drives it.
pub struct ProgressiveFinal {
    stream: TargetRateStream,
    next_start: usize,
    windows: Vec<Committed>,
}

impl ProgressiveFinal {
    pub fn new(channels: u16, sample_rate: u32) -> Result<Self, EngineError> {
        Ok(Self { stream: TargetRateStream::new(channels, sample_rate)?, next_start: 0, windows: Vec::new() })
    }

    pub fn push(&mut self, pcm: &[i16]) -> Result<(), EngineError> {
        self.stream.push(pcm)
    }

    /// Cuts every window that is now final, exactly as
    /// [`engine::transcription_windows`] will cut the finished take, and
    /// returns them for decoding.
    ///
    /// The batch loop cuts `(start, quietest_cut(start+18 s, start+30 s))`
    /// while more than 30 s remain. Over the stable prefix of the shortened
    /// audio that condition and that cut read only samples that will never
    /// change, so the same windows come out.
    pub fn commit(&mut self) -> Vec<ReadyWindow> {
        let shortened = engine::shorten_digital_silence_tracked(self.stream.output());
        let stable = &shortened.samples[..shortened.stable];
        let mut ready = Vec::new();
        while stable.len().saturating_sub(self.next_start) > TRANSCRIBE_WINDOW_SAMPLES {
            let start = self.next_start;
            let end = engine::quietest_cut(
                stable,
                start + TRANSCRIBE_MIN_WINDOW_SAMPLES,
                start + TRANSCRIBE_WINDOW_SAMPLES,
            );
            let window = &stable[start..end];
            ready.push(ReadyWindow { index: self.windows.len(), samples: window.to_vec() });
            self.windows.push(Committed {
                start,
                end,
                hash: engine::window_hash(window),
                source_end: shortened.source_index(end),
                text: None,
            });
            self.next_start = end;
        }
        ready
    }

    pub fn record(&mut self, index: usize, text: String) {
        if let Some(window) = self.windows.get_mut(index) {
            window.text = Some(text);
        }
    }

    /// How many leading windows are decoded, their joined text, and where the
    /// last of them ends in the 16 kHz capture.
    pub fn decoded_prefix(&self) -> Option<(usize, String, u64)> {
        let decoded: Vec<&Committed> = self.windows.iter().take_while(|window| window.text.is_some()).collect();
        let last = decoded.last()?;
        let parts: Vec<String> = decoded.iter().filter_map(|window| window.text.clone()).collect();
        Some((decoded.len(), parts.iter().map(|part| part.trim()).filter(|part| !part.is_empty()).collect::<Vec<_>>().join(" "), last.source_end as u64))
    }

    pub fn committed_windows(&self) -> usize {
        self.windows.len()
    }

    pub fn input_frames(&self) -> u64 {
        self.stream.frames()
    }

    /// The whole take at 16 kHz and the text of every window decoded so far.
    pub fn finish(self) -> Result<(Vec<f32>, WindowCache), EngineError> {
        let cache = self
            .windows
            .into_iter()
            .filter_map(|window| window.text.map(|text| ((window.start, window.end, window.hash), text)))
            .collect();
        Ok((self.stream.finish()?, cache))
    }
}

#[derive(Default)]
struct Incoming {
    format: Option<(u16, u32)>,
    pcm: Vec<i16>,
}

/// One take's progressive final transcription, shared between the capture
/// (which feeds it), a background worker (which decodes committed windows)
/// and the stop path (which finishes it).
#[derive(Default)]
pub struct ProgressiveSession {
    /// Filled by the audio writer thread; never held for more than a copy.
    incoming: Mutex<Incoming>,
    core: Mutex<Option<ProgressiveFinal>>,
    failed: AtomicBool,
    worker: Mutex<Option<JoinHandle<()>>>,
}

/// How often the worker looks for newly final windows.
const WORKER_TICK: Duration = Duration::from_millis(500);

impl ProgressiveSession {
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    /// Exactly the 16-bit interleaved samples written to the WAV.
    pub fn feed(&self, pcm: &[i16], channels: u16, sample_rate: u32) {
        if self.failed.load(Ordering::SeqCst) {
            return;
        }
        let Ok(mut incoming) = self.incoming.lock() else {
            self.failed.store(true, Ordering::SeqCst);
            return;
        };
        match incoming.format {
            None => incoming.format = Some((channels, sample_rate)),
            Some(format) if format != (channels, sample_rate) => {
                self.failed.store(true, Ordering::SeqCst);
                incoming.pcm.clear();
                return;
            }
            Some(_) => {}
        }
        incoming.pcm.extend_from_slice(pcm);
    }

    /// Converts everything fed so far; with `commit`, also cuts final windows.
    fn absorb(&self, commit: bool) -> Vec<ReadyWindow> {
        if self.failed.load(Ordering::SeqCst) {
            return Vec::new();
        }
        let (format, pcm) = match self.incoming.lock() {
            Ok(mut incoming) => (incoming.format, std::mem::take(&mut incoming.pcm)),
            Err(_) => {
                self.fail();
                return Vec::new();
            }
        };
        let Some((channels, sample_rate)) = format else { return Vec::new() };
        let Ok(mut core) = self.core.lock() else {
            self.fail();
            return Vec::new();
        };
        if core.is_none() {
            match ProgressiveFinal::new(channels, sample_rate) {
                Ok(created) => *core = Some(created),
                Err(_) => {
                    drop(core);
                    self.fail();
                    return Vec::new();
                }
            }
        }
        let progressive = core.as_mut().expect("created above");
        if progressive.push(&pcm).is_err() {
            drop(core);
            self.fail();
            return Vec::new();
        }
        if commit { progressive.commit() } else { Vec::new() }
    }

    /// Windows that have become final since the last call.
    pub fn advance(&self) -> Vec<ReadyWindow> {
        self.absorb(true)
    }

    pub fn record(&self, index: usize, text: String) {
        if let Ok(mut core) = self.core.lock() && let Some(core) = core.as_mut() {
            core.record(index, text);
        }
    }

    pub fn decoded_prefix(&self) -> Option<(usize, String, u64)> {
        self.core.lock().ok()?.as_ref()?.decoded_prefix()
    }

    /// Gives up on this take: the stop path will use the batch pass.
    pub fn fail(&self) {
        self.failed.store(true, Ordering::SeqCst);
        if let Ok(mut core) = self.core.lock() {
            *core = None;
        }
        if let Ok(mut incoming) = self.incoming.lock() {
            incoming.pcm = Vec::new();
        }
    }

    /// The take was cancelled or replaced: drop the cache and the audio.
    pub fn discard(&self) {
        self.fail();
    }

    /// Starts the background decoder. `stop` ends it when the take stops;
    /// `cancel` also discards everything. `on_tick` runs after each round of
    /// window decoding, on the same thread, so a live preview sharing this
    /// worker never competes with it for the model.
    pub fn spawn_worker(
        self: &Arc<Self>,
        engine: Arc<Engine>,
        model: String,
        stop: Arc<AtomicBool>,
        cancel: Arc<AtomicBool>,
        mut on_tick: impl FnMut(&ProgressiveSession) + Send + 'static,
    ) {
        let session = self.clone();
        let handle = std::thread::spawn(move || {
            loop {
                std::thread::park_timeout(WORKER_TICK);
                if cancel.load(Ordering::SeqCst) {
                    session.discard();
                    break;
                }
                if stop.load(Ordering::SeqCst) {
                    break;
                }
                for window in session.advance() {
                    // Checked before every window: a stop never waits for
                    // more than the one window already being decoded.
                    if stop.load(Ordering::SeqCst) || cancel.load(Ordering::SeqCst) {
                        break;
                    }
                    match engine.transcribe_window(&model, &window.samples, &stop) {
                        Ok(Some(text)) => session.record(window.index, text),
                        Ok(None) => break,
                        Err(_) => {
                            session.fail();
                            break;
                        }
                    }
                }
                if stop.load(Ordering::SeqCst) || cancel.load(Ordering::SeqCst) {
                    continue;
                }
                on_tick(&session);
            }
        });
        if let Ok(mut worker) = self.worker.lock() {
            *worker = Some(handle);
        }
    }

    /// At stop, once the capture has been finalised: waits for the worker
    /// (at most the window it is decoding, which is then reused), checks the
    /// fed audio is exactly the WAV on disk, and returns the whole take at
    /// 16 kHz with every window decoded so far. `None` means use the batch
    /// pass. Leaves nothing behind either way.
    pub fn finish(&self, wav: &Path) -> Option<(Vec<f32>, WindowCache)> {
        let worker = self.worker.lock().ok().and_then(|mut worker| worker.take());
        if let Some(worker) = worker {
            worker.thread().unpark();
            let _ = worker.join();
        }
        self.absorb(false);
        let core = self.core.lock().ok()?.take();
        if self.failed.load(Ordering::SeqCst) {
            return None;
        }
        let core = core?;
        let format = self.incoming.lock().ok()?.format?;
        let reader = hound::WavReader::open(wav).ok()?;
        let spec = reader.spec();
        if (spec.channels, spec.sample_rate) != format
            || spec.bits_per_sample != 16
            || spec.sample_format != hound::SampleFormat::Int
            || u64::from(reader.duration()) != core.input_frames()
        {
            return None;
        }
        core.finish().ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::read_as_target_rate;

    /// Deterministic noise, so tests are reproducible without a crate.
    struct Noise(u64);
    impl Noise {
        fn next(&mut self) -> f32 {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 7;
            self.0 ^= self.0 << 17;
            (self.0 >> 40) as f32 / (1u64 << 24) as f32 * 2.0 - 1.0
        }
    }

    /// A synthetic take at the capture rate: bursts of "speech" (noise at
    /// varying level), room-tone pauses, and gated digital silence of every
    /// length the shortener treats differently, including a long one at the end.
    fn synthetic_take(channels: u16, rate: u32, seconds: f32) -> Vec<i16> {
        let mut noise = Noise(0x9e37_79b9_7f4a_7c15);
        let mut mono = Vec::new();
        let total = (seconds * rate as f32) as usize;
        let mut segment = 0usize;
        while mono.len() < total {
            let (length, level) = match segment % 7 {
                0 => (2.7, 0.2),
                1 => (0.35, 0.002),
                2 => (4.1, 0.12),
                3 => (3.3, 0.0),
                4 => (1.9, 0.25),
                5 => (1.2, 0.0),
                _ => (0.6, 0.004),
            };
            let length = (length + (segment % 5) as f32 * 0.37) * rate as f32;
            for _ in 0..length as usize {
                mono.push(noise.next() * level);
            }
            segment += 1;
        }
        mono.truncate(total);
        // Long gated silence at the end, as when the user pauses before stopping.
        mono.extend(std::iter::repeat_n(0.0, (2.6 * rate as f32) as usize));
        mono.iter()
            .flat_map(|sample| {
                let value = (sample * 32767.0).round() as i16;
                (0..channels).map(move |channel| value.saturating_add(channel as i16))
            })
            .collect()
    }

    fn write_wav(path: &Path, pcm: &[i16], channels: u16, rate: u32) {
        let spec = hound::WavSpec { channels, sample_rate: rate, bits_per_sample: 16, sample_format: hound::SampleFormat::Int };
        let mut writer = hound::WavWriter::create(path, spec).unwrap();
        for sample in pcm {
            writer.write_sample(*sample).unwrap();
        }
        writer.finalize().unwrap();
    }

    /// Irregular packet sizes, some splitting a frame between packets.
    fn packets(pcm: &[i16]) -> impl Iterator<Item = &[i16]> {
        let sizes = [961usize, 1920, 7, 4801, 480, 2048, 3];
        let mut position = 0;
        let mut index = 0;
        std::iter::from_fn(move || {
            if position >= pcm.len() {
                return None;
            }
            let end = (position + sizes[index % sizes.len()]).min(pcm.len());
            index += 1;
            let packet = &pcm[position..end];
            position = end;
            Some(packet)
        })
    }

    fn bits(samples: &[f32]) -> Vec<u32> {
        samples.iter().map(|sample| sample.to_bits()).collect()
    }

    #[test]
    fn streaming_conversion_is_bit_identical_to_reading_the_wav() {
        let temp = tempfile::tempdir().unwrap();
        for (channels, rate, seconds) in [(2u16, 48_000u32, 7.3f32), (1, 44_100, 5.1), (1, 16_000, 3.0), (2, 48_000, 0.01)] {
            let pcm = synthetic_take(channels, rate, seconds);
            let path = temp.path().join(format!("{channels}-{rate}.wav"));
            write_wav(&path, &pcm, channels, rate);
            let mut stream = TargetRateStream::new(channels, rate).unwrap();
            for packet in packets(&pcm) {
                stream.push(packet).unwrap();
            }
            let batch = read_as_target_rate(&path).unwrap();
            assert_eq!(stream.frames(), u64::from(hound::WavReader::open(&path).unwrap().duration()));
            assert_eq!(bits(&stream.finish().unwrap()), bits(&batch), "{channels} ch at {rate} Hz");
        }
    }

    #[test]
    fn the_stable_part_of_shortened_audio_never_changes_as_the_take_grows() {
        let pcm = synthetic_take(1, 16_000, 40.0);
        let full: Vec<f32> = pcm.iter().map(|sample| f32::from(*sample) / 32768.0).collect();
        let complete = engine::shorten_digital_silence_tracked(&full);
        for length in (0..full.len()).step_by(1_337) {
            let prefix = engine::shorten_digital_silence_tracked(&full[..length]);
            assert!(prefix.stable <= prefix.samples.len());
            assert_eq!(bits(&prefix.samples[..prefix.stable]), bits(&complete.samples[..prefix.stable]), "prefix of {length}");
            for index in (0..prefix.stable).step_by(997) {
                assert_eq!(prefix.source_index(index), complete.source_index(index));
            }
        }
    }

    /// The design requirement: the final transcript built from windows decoded
    /// during recording is the batch final pass over the WAV, and at stop only
    /// the windows not yet committed are decoded.
    #[test]
    fn progressive_final_text_equals_the_batch_pass() {
        let temp = tempfile::tempdir().unwrap();
        let (channels, rate) = (2u16, 48_000u32);
        let pcm = synthetic_take(channels, rate, 97.0);
        let path = temp.path().join("take.wav");
        write_wav(&path, &pcm, channels, rate);
        // A stand-in model whose text depends on every sample it is given.
        let fake = |window: &[f32]| Ok::<_, EngineError>(format!(" w{:016x} ", engine::window_hash(window)));

        let batch_samples = read_as_target_rate(&path).unwrap();
        let mut batch_decodes = 0;
        let batch = engine::final_text(&batch_samples, &WindowCache::new(), None, |window| {
            batch_decodes += 1;
            fake(window)
        })
        .unwrap();

        // Recording: packets arrive, the worker looks every half second.
        let mut progressive = ProgressiveFinal::new(channels, rate).unwrap();
        let tick = rate as usize * usize::from(channels) / 2;
        let mut since_tick = 0;
        for packet in packets(&pcm) {
            progressive.push(packet).unwrap();
            since_tick += packet.len();
            if since_tick >= tick {
                since_tick = 0;
                for window in progressive.commit() {
                    let text = engine::window_text(&window.samples, fake).unwrap();
                    progressive.record(window.index, text);
                }
            }
        }
        let committed = progressive.committed_windows();
        let (samples, cache) = progressive.finish().unwrap();
        assert_eq!(bits(&samples), bits(&batch_samples));

        let mut stop_decodes = 0;
        let at_stop = engine::final_text(&samples, &cache, None, |window| {
            stop_decodes += 1;
            fake(window)
        })
        .unwrap();

        let windows = engine::transcription_windows(&engine::shorten_digital_silence_tracked(&batch_samples).samples);
        assert!(committed >= 2, "a 97 s take commits windows while recording ({committed})");
        assert_eq!(cache.len(), committed, "every committed window is reused");
        assert_eq!(stop_decodes, windows.len() - committed, "only the tail is decoded at stop");
        assert!(stop_decodes <= 1);
        assert_eq!(batch_decodes, windows.len());
        assert_eq!(at_stop, batch);
        assert!(!batch.is_empty());
    }

    #[test]
    fn the_session_falls_back_when_the_audio_does_not_match_the_wav() {
        let temp = tempfile::tempdir().unwrap();
        let pcm = synthetic_take(1, 16_000, 2.0);
        let path = temp.path().join("take.wav");
        write_wav(&path, &pcm, 1, 16_000);

        let session = ProgressiveSession::new();
        session.feed(&pcm[..pcm.len() - 160], 1, 16_000);
        assert!(session.finish(&path).is_none(), "a missing packet must not be accepted");

        let session = ProgressiveSession::new();
        session.feed(&pcm, 1, 16_000);
        let (samples, cache) = session.finish(&path).expect("matching audio is used");
        assert_eq!(bits(&samples), bits(&read_as_target_rate(&path).unwrap()));
        assert!(cache.is_empty());

        let session = ProgressiveSession::new();
        session.feed(&pcm, 1, 16_000);
        session.discard();
        assert!(session.finish(&path).is_none(), "a discarded take keeps nothing");
    }

    /// Feeds real recordings through the progressive path at a multiple of
    /// real time, with the real model decoding in the background, then
    /// measures stop-to-text against the batch pass and checks both agree.
    ///
    /// ```text
    /// LOQUARA_TEST_MODELS=<dir containing parakeet/>
    /// LOQUARA_TEST_WAVS=<a.wav>;<b.wav>   (default: three takes from %APPDATA%)
    /// LOQUARA_TEST_SPEED=<x real time>     (default 1)
    ///   cargo test --release --lib progressive::tests::real -- --ignored --nocapture
    /// ```
    #[test]
    #[ignore = "needs a downloaded model and recordings"]
    fn real_recordings_progressive_matches_batch_and_stops_faster() {
        let models = std::env::var("LOQUARA_TEST_MODELS").expect("LOQUARA_TEST_MODELS");
        let wavs = std::env::var("LOQUARA_TEST_WAVS").expect("LOQUARA_TEST_WAVS");
        let speed: f64 = std::env::var("LOQUARA_TEST_SPEED").ok().and_then(|speed| speed.parse().ok()).unwrap_or(1.0);
        let engine = Arc::new(Engine::new(models));
        engine.ensure_loaded("parakeet").unwrap();
        for wav in wavs.split(';').filter(|wav| !wav.is_empty()) {
            let path = Path::new(wav);
            let mut reader = hound::WavReader::open(path).unwrap();
            let spec = reader.spec();
            let pcm: Vec<i16> = reader.samples::<i16>().map(Result::unwrap).collect();
            let seconds = pcm.len() as f64 / f64::from(spec.channels) / f64::from(spec.sample_rate);

            let session = ProgressiveSession::new();
            let stop = Arc::new(AtomicBool::new(false));
            let cancel = Arc::new(AtomicBool::new(false));
            session.spawn_worker(engine.clone(), "parakeet".into(), stop.clone(), cancel.clone(), |_| {});
            // 10 ms packets, paced like a microphone.
            let packet = spec.sample_rate as usize / 100 * usize::from(spec.channels);
            let started = std::time::Instant::now();
            for (index, chunk) in pcm.chunks(packet).enumerate() {
                session.feed(chunk, spec.channels, spec.sample_rate);
                let due = Duration::from_secs_f64((index + 1) as f64 * 0.01 / speed);
                if let Some(wait) = due.checked_sub(started.elapsed()) {
                    std::thread::sleep(wait);
                }
            }
            stop.store(true, Ordering::SeqCst);
            let stopped = std::time::Instant::now();
            let (samples, cache) = session.finish(path).expect("progressive state matches the WAV");
            let reused = cache.len();
            let progressive = engine.transcribe_prepared("parakeet", &samples, &cache, None).unwrap();
            let progressive_latency = stopped.elapsed();

            let batch_started = std::time::Instant::now();
            let batch = engine.transcribe("parakeet", path, None).unwrap();
            let batch_latency = batch_started.elapsed();

            println!(
                "{seconds:6.1} s take: batch {batch_latency:?}, progressive {progressive_latency:?} ({reused} windows reused), identical: {}",
                progressive == batch
            );
            assert_eq!(progressive, batch, "{wav}");
        }
    }
}
