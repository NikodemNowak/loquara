//! "Music in the next room": a steep low-pass, a few dB of attenuation and a
//! hint of early reflections, all driven by one `amount` (0 = transparent,
//! 1 = fully muffled) that glides per sample so nothing clicks.
//!
//! Pure and allocation-free after construction, so it can run inside any
//! platform's real-time audio callback.
#![cfg_attr(not(any(windows, target_os = "linux", target_os = "macos")), allow(dead_code))]

use std::f32::consts::PI;
use std::time::Duration;

/// Cutoff when the effect is fully open: above anything most people hear.
const OPEN_HZ: f32 = 18_000.0;
/// Cutoff when fully muffled: voices and bass survive, detail is gone.
pub(crate) const CLOSED_HZ: f32 = 600.0;
/// Overall level change when fully muffled.
pub(crate) const CLOSED_GAIN_DB: f32 = -8.0;
/// 4th-order Butterworth split into two biquads (24 dB/octave).
const STAGE_Q: [f32; 2] = [0.541_196_1, 1.306_563];
/// Coefficients are refreshed this often (frames) while gliding.
const COEFF_BLOCK: u32 = 16;
/// Early reflections: (delay ms, gain). Left and right use different
/// delays so the "room" is wide, mixed in proportion to `amount`.
const REFLECTIONS: [[(f32, f32); 3]; 2] = [
    [(7.0, 0.16), (13.0, 0.10), (23.0, 0.06)],
    [(9.0, 0.15), (17.0, 0.09), (27.0, 0.05)],
];
const MAX_DELAY_MS: f32 = 30.0;

#[derive(Clone, Copy, Default)]
struct Coeffs {
    b0: f32,
    b1: f32,
    b2: f32,
    a1: f32,
    a2: f32,
}

impl Coeffs {
    /// RBJ cookbook low-pass.
    fn low_pass(sample_rate: f32, cutoff: f32, q: f32) -> Self {
        let w0 = 2.0 * PI * cutoff / sample_rate;
        let (sin, cos) = w0.sin_cos();
        let alpha = sin / (2.0 * q);
        let a0 = 1.0 + alpha;
        let b1 = (1.0 - cos) / a0;
        Self { b0: b1 / 2.0, b1, b2: b1 / 2.0, a1: -2.0 * cos / a0, a2: (1.0 - alpha) / a0 }
    }
}

/// Transposed direct form II state of one biquad on one channel.
#[derive(Clone, Copy, Default)]
struct Stage {
    z1: f32,
    z2: f32,
}

impl Stage {
    #[inline]
    fn run(&mut self, c: &Coeffs, x: f32) -> f32 {
        let y = c.b0 * x + self.z1;
        self.z1 = c.b1 * x - c.a1 * y + self.z2;
        self.z2 = c.b2 * x - c.a2 * y;
        // Flush denormals and recover from any non-finite input.
        if !self.z1.is_finite() || self.z1.abs() < 1e-20 {
            self.z1 = 0.0;
        }
        if !self.z2.is_finite() || self.z2.abs() < 1e-20 {
            self.z2 = 0.0;
        }
        if y.is_finite() { y } else { 0.0 }
    }
}

pub(crate) fn cutoff_for(amount: f32, sample_rate: f32) -> f32 {
    let amount = amount.clamp(0.0, 1.0);
    // Exponential glide sounds even: every step is the same musical interval.
    let cutoff = OPEN_HZ * (CLOSED_HZ / OPEN_HZ).powf(amount);
    cutoff.min(sample_rate * 0.45)
}

pub(crate) fn gain_for(amount: f32) -> f32 {
    10_f32.powf(CLOSED_GAIN_DB * amount.clamp(0.0, 1.0) / 20.0)
}

pub(crate) struct Muffler {
    sample_rate: f32,
    channels: usize,
    amount: f32,
    target: f32,
    /// Change of `amount` per frame while gliding.
    step: f32,
    /// Frames left in the current glide.
    remaining: u32,
    coeffs: [Coeffs; 2],
    countdown: u32,
    /// [channel][stage]
    stages: Vec<[Stage; 2]>,
    /// Ring buffer of filtered output per channel, for reflections.
    history: Vec<Vec<f32>>,
    taps: Vec<[(usize, f32); 3]>,
    position: usize,
}

impl Muffler {
    pub fn new(sample_rate: u32, channels: usize) -> Self {
        let sample_rate = sample_rate.max(8_000) as f32;
        let channels = channels.max(1);
        let length = (sample_rate * MAX_DELAY_MS / 1000.0) as usize + 1;
        let taps = (0..channels)
            .map(|channel| REFLECTIONS[channel % 2].map(|(ms, gain)| (((sample_rate * ms / 1000.0) as usize).min(length - 1), gain)))
            .collect();
        let mut muffler = Self {
            sample_rate,
            channels,
            amount: 0.0,
            target: 0.0,
            step: 0.0,
            remaining: 0,
            coeffs: [Coeffs::default(); 2],
            countdown: 0,
            stages: vec![[Stage::default(); 2]; channels],
            history: vec![vec![0.0; length]; channels],
            taps,
            position: 0,
        };
        muffler.update_coeffs();
        muffler
    }

    #[cfg_attr(not(test), allow(dead_code))]
    pub fn amount(&self) -> f32 {
        self.amount
    }

    /// Glides to `amount` over `ramp` (immediately for a zero ramp).
    pub fn set_target(&mut self, amount: f32, ramp: Duration) {
        let amount = if amount.is_finite() { amount.clamp(0.0, 1.0) } else { 0.0 };
        self.target = amount;
        let frames = (ramp.as_secs_f32() * self.sample_rate).max(0.0);
        if frames < 1.0 {
            self.amount = amount;
            self.step = 0.0;
            self.remaining = 0;
            self.update_coeffs();
        } else {
            self.step = (amount - self.amount) / frames;
            self.remaining = frames.round() as u32;
        }
    }

    fn update_coeffs(&mut self) {
        let cutoff = cutoff_for(self.amount, self.sample_rate);
        for (coeffs, q) in self.coeffs.iter_mut().zip(STAGE_Q) {
            *coeffs = Coeffs::low_pass(self.sample_rate, cutoff, q);
        }
    }

    /// Filters interleaved samples in place.
    pub fn process(&mut self, interleaved: &mut [f32]) {
        let length = self.history[0].len();
        for frame in interleaved.chunks_mut(self.channels) {
            if self.remaining > 0 {
                self.remaining -= 1;
                if self.remaining == 0 {
                    self.amount = self.target;
                    self.countdown = 0;
                } else {
                    self.amount = (self.amount + self.step).clamp(0.0, 1.0);
                }
                if self.countdown == 0 {
                    self.update_coeffs();
                    self.countdown = COEFF_BLOCK;
                }
                self.countdown -= 1;
            }
            let gain = gain_for(self.amount);
            let room = self.amount;
            for (channel, sample) in frame.iter_mut().enumerate() {
                // Real audio stays within a few units of full scale; anything else
                // is garbage that must not poison the filter state.
                let input = if sample.is_finite() { sample.clamp(-4.0, 4.0) } else { 0.0 };
                let [first, second] = &mut self.stages[channel];
                let filtered = second.run(&self.coeffs[1], first.run(&self.coeffs[0], input));
                let history = &mut self.history[channel];
                history[self.position] = filtered;
                let mut reflected = 0.0;
                for (delay, tap_gain) in self.taps[channel] {
                    reflected += history[(self.position + length - delay) % length] * tap_gain;
                }
                *sample = (filtered + reflected * room) * gain;
            }
            self.position = (self.position + 1) % length;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const RATE: u32 = 48_000;

    fn sine(frequency: f32, seconds: f32, channels: usize) -> Vec<f32> {
        let frames = (RATE as f32 * seconds) as usize;
        (0..frames)
            .flat_map(|n| {
                let value = (2.0 * PI * frequency * n as f32 / RATE as f32).sin() * 0.5;
                std::iter::repeat_n(value, channels)
            })
            .collect()
    }

    fn rms(samples: &[f32]) -> f32 {
        (samples.iter().map(|value| value * value).sum::<f32>() / samples.len() as f32).sqrt()
    }

    /// Level change in dB after the filter settles (second half only).
    fn response_db(amount: f32, frequency: f32) -> f32 {
        let mut muffler = Muffler::new(RATE, 2);
        muffler.set_target(amount, Duration::ZERO);
        let input = sine(frequency, 0.5, 2);
        let mut output = input.clone();
        muffler.process(&mut output);
        let half = input.len() / 2;
        20.0 * (rms(&output[half..]) / rms(&input[half..])).log10()
    }

    #[test]
    fn open_is_transparent_in_the_audible_band() {
        for frequency in [100.0, 1_000.0, 5_000.0] {
            let db = response_db(0.0, frequency);
            assert!(db.abs() < 0.5, "{frequency} Hz changed by {db} dB when open");
        }
    }

    #[test]
    fn muffled_kills_treble_and_keeps_bass() {
        let treble = response_db(1.0, 5_000.0);
        let bass = response_db(1.0, 200.0);
        assert!(treble < -20.0 - CLOSED_GAIN_DB.abs(), "5 kHz only {treble} dB");
        assert!(bass > CLOSED_GAIN_DB - 3.5 && bass < CLOSED_GAIN_DB + 3.5, "200 Hz at {bass} dB");
    }

    #[test]
    fn handles_mono_and_44k() {
        let mut muffler = Muffler::new(44_100, 1);
        muffler.set_target(1.0, Duration::ZERO);
        let mut samples = vec![0.25; 4_410];
        muffler.process(&mut samples);
        assert!(samples.iter().all(|value| value.is_finite()));
    }

    #[test]
    fn never_emits_nan_or_infinity() {
        let mut muffler = Muffler::new(RATE, 2);
        muffler.set_target(1.0, Duration::from_millis(10));
        let mut samples = vec![f32::NAN, f32::INFINITY, -f32::INFINITY, 1e30, -1e30, 1e-40, 0.0, 0.0];
        samples.extend(sine(300.0, 0.05, 2));
        muffler.process(&mut samples);
        assert!(samples.iter().all(|value| value.is_finite()));
        let mut silence = vec![0.0; 9_600];
        muffler.process(&mut silence);
        assert!(silence.iter().all(|value| value.is_finite()));
        // Once the reflections have passed, the tail decays to silence.
        assert!(silence[silence.len() / 2..].iter().all(|value| value.abs() < 1e-3));
    }

    #[test]
    fn glides_without_clicks_and_lands_on_target() {
        let mut muffler = Muffler::new(RATE, 2);
        muffler.set_target(1.0, Duration::from_millis(300));
        let mut samples = sine(220.0, 0.4, 2);
        muffler.process(&mut samples);
        assert_eq!(muffler.amount(), 1.0);
        // A 0.5-amplitude 220 Hz sine moves at most ~0.0145 per sample;
        // a click would show up as a far larger jump.
        let largest = samples.chunks(2).zip(samples.chunks(2).skip(1)).map(|(a, b)| (b[0] - a[0]).abs()).fold(0.0, f32::max);
        assert!(largest < 0.03, "largest step {largest}");
        muffler.set_target(0.0, Duration::from_millis(300));
        let mut back = sine(220.0, 0.4, 2);
        muffler.process(&mut back);
        assert_eq!(muffler.amount(), 0.0);
    }

    #[test]
    fn ramp_is_monotonic_and_timed() {
        let mut muffler = Muffler::new(RATE, 1);
        muffler.set_target(1.0, Duration::from_millis(100));
        let mut block = vec![0.0; 2_400];
        muffler.process(&mut block);
        let halfway = muffler.amount();
        assert!((halfway - 0.5).abs() < 0.01, "{halfway}");
        muffler.process(&mut block);
        assert_eq!(muffler.amount(), 1.0);
    }

    #[test]
    fn curves_span_open_to_closed() {
        assert!((cutoff_for(0.0, 48_000.0) - OPEN_HZ).abs() < 1.0);
        assert!((cutoff_for(1.0, 48_000.0) - CLOSED_HZ).abs() < 1.0);
        assert!(cutoff_for(0.0, 22_050.0) < 11_025.0);
        assert_eq!(gain_for(0.0), 1.0);
        assert!((20.0 * gain_for(1.0).log10() - CLOSED_GAIN_DB).abs() < 0.01);
    }
}
