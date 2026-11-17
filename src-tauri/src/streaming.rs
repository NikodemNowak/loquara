//! Live preview plumbing: the captured audio, resampled to the 16 kHz mono
//! the recogniser wants, and the sink that receives replaceable drafts.
//! Drafts never inject keyboard input.

use rubato::{
    Resampler, SincFixedIn, SincInterpolationParameters, SincInterpolationType, WindowFunction,
};

/// The recogniser's sample rate.
pub const TARGET_SAMPLE_RATE: u32 = 16_000;

/// Turns captured audio into the 16 kHz mono stream the recogniser wants.
pub struct AudioPump {
    channels: usize,
    resampler: Option<SincFixedIn<f32>>,
    pending: Vec<f32>,
    interleaved: Vec<i16>,
    input_frames: usize,
    output_frames: usize,
    sample_rate: u32,
    flushed: bool,
}

/// Samples per resampler call. Small enough to keep latency under a tenth of
/// a second, large enough that the sinc filter is worth its name.
const PUMP_CHUNK: usize = 1_024;

impl AudioPump {
    pub fn new(channels: u16, sample_rate: u32) -> Result<Self, String> {
        let channels = usize::from(channels.max(1));
        let resampler = if sample_rate == TARGET_SAMPLE_RATE || sample_rate == 0 {
            None
        } else {
            let parameters = SincInterpolationParameters {
                sinc_len: 64,
                f_cutoff: 0.95,
                interpolation: SincInterpolationType::Linear,
                oversampling_factor: 128,
                window: WindowFunction::BlackmanHarris2,
            };
            Some(
                SincFixedIn::<f32>::new(
                    f64::from(TARGET_SAMPLE_RATE) / f64::from(sample_rate),
                    1.0,
                    parameters,
                    PUMP_CHUNK,
                    1,
                )
                .map_err(|error| error.to_string())?,
            )
        };
        Ok(Self {
            channels,
            resampler,
            pending: Vec::with_capacity(PUMP_CHUNK * 2),
            interleaved: Vec::new(),
            input_frames: 0,
            output_frames: 0,
            sample_rate: sample_rate.max(1),
            flushed: false,
        })
    }

    /// Adds one captured packet and returns whatever is ready to send.
    pub fn push(&mut self, pcm: &[i16]) -> Result<Vec<i16>, String> {
        if self.flushed {
            return Err("audio pump already finalized".into());
        }
        self.interleaved.extend_from_slice(pcm);
        let complete = self.interleaved.len() / self.channels * self.channels;
        for frame in self.interleaved[..complete].chunks_exact(self.channels) {
            self.input_frames += 1;
            let sum: f32 = frame
                .iter()
                .map(|sample| f32::from(*sample) / 32768.0)
                .sum();
            self.pending.push(sum / self.channels as f32);
        }
        self.interleaved.drain(..complete);
        let mut out = Vec::new();
        match self.resampler.as_mut() {
            Some(resampler) => {
                while self.pending.len() >= PUMP_CHUNK {
                    let chunk: Vec<f32> = self.pending.drain(..PUMP_CHUNK).collect();
                    let waves = resampler
                        .process(&[chunk], None)
                        .map_err(|error| error.to_string())?;
                    if let Some(wave) = waves.first() {
                        out.extend(wave.iter().map(|sample| f32_to_i16(*sample)));
                    }
                }
            }
            None => out.extend(self.pending.drain(..).map(f32_to_i16)),
        }
        self.output_frames += out.len();
        Ok(out)
    }

    /// Drain fractional input and filter tail before audio.done. Output length
    /// tracks input frames exactly, including packets split inside a frame.
    pub fn flush(&mut self) -> Result<Vec<i16>, String> {
        if self.flushed {
            return Ok(Vec::new());
        }
        self.flushed = true;
        let expected = self.input_frames * TARGET_SAMPLE_RATE as usize / self.sample_rate as usize;
        let needed = expected.saturating_sub(self.output_frames);
        let mut out = Vec::new();
        if let Some(resampler) = self.resampler.as_mut() {
            let pending = std::mem::take(&mut self.pending);
            let mut first = true;
            while out.len() < needed {
                let waves = if first && !pending.is_empty() {
                    resampler.process_partial(Some(&[pending.as_slice()]), None)
                } else {
                    resampler.process_partial::<&[f32]>(None, None)
                }
                .map_err(|error| error.to_string())?;
                first = false;
                if let Some(wave) = waves.first() {
                    out.extend(wave.iter().map(|sample| f32_to_i16(*sample)));
                } else {
                    return Err("resampler produced no tail".into());
                }
            }
        }
        out.truncate(needed);
        self.output_frames += out.len();
        Ok(out)
    }
}

fn f32_to_i16(sample: f32) -> i16 {
    (sample.clamp(-1.0, 1.0) * 32767.0).round() as i16
}

/// Whole replaceable draft; no keyboard/focus capability is exposed.
pub trait TranscriptSink: Send + Sync {
    fn publish(&self, text: &str) -> Result<(), String>;
    fn failed(&self, message: &str);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn audio_already_at_the_target_rate_is_left_alone() {
        let mut pump = AudioPump::new(1, 16_000).unwrap();

        let out = pump.push(&[100, -100, 200]).unwrap();

        assert_eq!(out, vec![100, -100, 200]);
    }
    #[test]
    fn a_stereo_packet_is_averaged_into_one_channel() {
        let mut pump = AudioPump::new(2, 16_000).unwrap();

        let out = pump.push(&[100, 300, -100, -300]).unwrap();

        assert_eq!(out, vec![200, -200]);
    }

    #[test]
    fn pump_flush_preserves_every_frame_for_common_rates_and_channels() {
        for rate in [16_000, 44_100, 48_000] {
            for channels in [1, 2, 3] {
                for frames in [1, 200, 1024, 1999, rate as usize + 73] {
                    let mut pump = AudioPump::new(channels, rate).unwrap();
                    let input = vec![1000; frames * channels as usize];
                    let mut out = Vec::new();
                    for packet in input.chunks(137) {
                        out.extend(pump.push(packet).unwrap());
                    }
                    out.extend(pump.flush().unwrap());
                    assert_eq!(
                        out.len(),
                        frames * 16_000 / rate as usize,
                        "rate {rate} channels {channels} frames {frames}"
                    );
                    assert!(pump.flush().unwrap().is_empty());
                }
            }
        }
    }
}
