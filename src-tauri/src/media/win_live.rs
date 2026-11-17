//! Live checks against the real Windows audio stack. They play quiet tones
//! from a helper process for a few seconds each, so they are ignored by
//! default:
//! `cargo test --lib media::win_live -- --ignored --nocapture --test-threads=1`

use super::win::{endpoints, init_thread, is_active, muffle_begin_only, sessions};
use super::win_muffle::{ATTENUATION, Engine, record};
use super::{Policy, Stop};
use std::f32::consts::PI;
use std::sync::mpsc;
use std::time::{Duration, Instant};
use windows::Win32::Media::Audio::*;
use windows_core::Interface;

/// A PowerShell process looping a quiet tone made of `partials` (Hz, amplitude).
/// Started through `cmd /c start` so it is not in our process tree: our
/// own loopback recordings must hear only what the engine plays.
struct Tone {
    pid: u32,
    _dir: tempfile::TempDir,
}

impl Tone {
    fn start(partials: &[(f32, f32)], seconds: u32) -> Tone {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("tone.wav");
        let spec = hound::WavSpec { channels: 1, sample_rate: 48_000, bits_per_sample: 16, sample_format: hound::SampleFormat::Int };
        let mut writer = hound::WavWriter::create(&path, spec).unwrap();
        for n in 0..48_000 {
            let t = n as f32 / 48_000.0;
            let value: f32 = partials.iter().map(|(hz, amplitude)| (2.0 * PI * hz * t).sin() * amplitude).sum();
            writer.write_sample((value * i16::MAX as f32) as i16).unwrap();
        }
        writer.finalize().unwrap();
        let pid_file = dir.path().join("pid.txt");
        let script = format!(
            "$PID | Out-File -Encoding ascii '{}'; $p = New-Object System.Media.SoundPlayer '{}'; $p.PlayLooping(); Start-Sleep -Seconds {seconds}",
            pid_file.display(),
            path.display()
        );
        std::process::Command::new("cmd.exe")
            .args(["/c", "start", "", "/min", "powershell.exe", "-NoProfile", "-NonInteractive", "-WindowStyle", "Hidden", "-Command", &script])
            .status()
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(10);
        let pid = loop {
            if let Some(pid) = std::fs::read_to_string(&pid_file).ok().and_then(|text| text.trim().parse().ok()) {
                break pid;
            }
            assert!(Instant::now() < deadline, "tone process did not start");
            std::thread::sleep(Duration::from_millis(50));
        };
        Tone { pid, _dir: dir }
    }

    fn pid(&self) -> u32 {
        self.pid
    }
}

impl Drop for Tone {
    fn drop(&mut self) {
        let _ = std::process::Command::new("taskkill").args(["/PID", &self.pid.to_string(), "/F"]).output();
    }
}

fn session_volume(pid: u32) -> Option<ISimpleAudioVolume> {
    for device in endpoints(eRender).ok()? {
        for session in sessions(&device) {
            if is_active(&session) && unsafe { session.GetProcessId() }.ok() == Some(pid) {
                return session.cast().ok();
            }
        }
    }
    None
}

fn wait_for_session(pid: u32) -> ISimpleAudioVolume {
    let deadline = Instant::now() + Duration::from_secs(8);
    loop {
        if let Some(volume) = session_volume(pid) {
            // Let the stream reach a steady state.
            std::thread::sleep(Duration::from_millis(300));
            return volume;
        }
        assert!(Instant::now() < deadline, "tone session never appeared");
        std::thread::sleep(Duration::from_millis(100));
    }
}

fn rms(samples: &[f32]) -> f32 {
    if samples.is_empty() {
        return 0.0;
    }
    (samples.iter().map(|value| value * value).sum::<f32>() / samples.len() as f32).sqrt()
}

/// Amplitude of one frequency in interleaved stereo (left channel), Goertzel.
fn tone_level(samples: &[f32], hz: f32) -> f32 {
    let left: Vec<f32> = samples.chunks(2).map(|frame| frame[0]).collect();
    let coefficient = 2.0 * (2.0 * PI * hz / 48_000.0).cos();
    let (mut s1, mut s2) = (0.0f32, 0.0f32);
    for sample in &left {
        let s0 = sample + coefficient * s1 - s2;
        s2 = s1;
        s1 = s0;
    }
    let power = s1 * s1 + s2 * s2 - coefficient * s1 * s2;
    2.0 * power.max(0.0).sqrt() / left.len().max(1) as f32
}

fn db(ratio: f32) -> f32 {
    20.0 * ratio.max(1e-9).log10()
}

fn average_level(engine: &Engine, window: Duration) -> f32 {
    let deadline = Instant::now() + window;
    let (mut sum, mut count) = (0.0, 0.0);
    while Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(20));
        sum += engine.level(0);
        count += 1.0;
    }
    sum / count
}

/// The finding the engine is built around: process loopback hears an app
/// after its session volume, linearly, and hears nothing when muted.
#[test]
#[ignore]
fn process_loopback_follows_session_volume_linearly() {
    let _com = init_thread().unwrap();
    let tone = Tone::start(&[(440.0, 0.08)], 12);
    let volume = wait_for_session(tone.pid());
    let engine = Engine::start(vec![tone.pid()], None).expect("engine");
    assert_eq!(engine.started, vec![true]);
    assert!(engine.wait_playing(Duration::from_secs(2)));
    let full = average_level(&engine, Duration::from_millis(500));
    let mut ratios = Vec::new();
    for level in [0.1f32, 0.01] {
        unsafe { volume.SetMasterVolume(level, std::ptr::null()).unwrap() };
        std::thread::sleep(Duration::from_millis(250));
        ratios.push((level, average_level(&engine, Duration::from_millis(400)) / full));
    }
    unsafe { volume.SetMasterVolume(1.0, std::ptr::null()).unwrap() };
    unsafe { volume.SetMute(true, std::ptr::null()).unwrap() };
    std::thread::sleep(Duration::from_millis(250));
    let muted = average_level(&engine, Duration::from_millis(400)) / full;
    unsafe { volume.SetMute(false, std::ptr::null()).unwrap() };
    engine.stop();
    println!("full rms {full:.4}; session→capture ratios {ratios:?}; muted {muted:.4}");
    assert!(full > 0.03, "tone not captured");
    for (level, ratio) in ratios {
        assert!((ratio / level - 1.0).abs() < 0.25, "session {level} gave capture ratio {ratio}");
    }
    assert!(muted < 0.01);
}

/// End to end on the real stack: the original is lowered to 1 %, our copy
/// keeps the bass of the tone and loses its treble, and everything is
/// restored on end.
#[test]
#[ignore]
fn muffle_end_to_end() {
    let _com = init_thread().unwrap();
    let tone = Tone::start(&[(440.0, 0.04), (5_000.0, 0.04)], 20);
    let volume = wait_for_session(tone.pid());
    let dry = record(tone.pid(), Duration::from_millis(600));
    let (dry_low, dry_high) = (tone_level(&dry, 440.0), tone_level(&dry, 5_000.0));

    let (_end, signal) = mpsc::channel();
    let mut stop = Stop { signal, fired: false };
    let policy = Policy::from_capturers(Some(&[]));
    let begun = Instant::now();
    let muffled = muffle_begin_only(&policy, &mut stop, Some(tone.pid())).expect("muffle started");
    let returned = begun.elapsed();
    // How long the hand-over leaves a gap: first 5 ms window where our copy is audible.
    let onset = record(std::process::id(), Duration::from_millis(300));
    let gap = onset.chunks(480).position(|window| rms(window) > 0.005).map(|windows| returned + Duration::from_millis(windows as u64 * 5));
    println!("begin returned after {returned:?}; copy audible ~{gap:?} after begin (includes the dry tone fading at the start)");
    std::thread::sleep(Duration::from_millis(200));
    let session_level = unsafe { volume.GetMasterVolume().unwrap() };
    let residue = rms(&record(tone.pid(), Duration::from_millis(400)));
    let ours = record(std::process::id(), Duration::from_millis(800));
    let (wet_low, wet_high) = (tone_level(&ours, 440.0), tone_level(&ours, 5_000.0));
    let started = Instant::now();
    muffled.end();
    let handback = started.elapsed();
    std::thread::sleep(Duration::from_millis(200));
    let restored = unsafe { volume.GetMasterVolume().unwrap() };
    let after = rms(&record(std::process::id(), Duration::from_millis(300)));

    println!(
        "dry 440 {dry_low:.4} 5k {dry_high:.4} | session {session_level:.3} residue rms {residue:.5} | \
         ours 440 {wet_low:.4} ({:.1} dB) 5k {wet_high:.5} ({:.1} dB) | end took {handback:?}, restored {restored:.3}, ours after {after:.5}",
        db(wet_low / dry_low),
        db(wet_high / dry_high),
    );
    assert!((session_level - ATTENUATION).abs() < 1e-4, "original not lowered");
    assert!(residue < rms(&dry) * 0.03, "original still audible");
    assert!(db(wet_low / dry_low) > -13.0 && db(wet_low / dry_low) < -3.0, "bass level off");
    assert!(db(wet_high / dry_high) < -30.0, "treble not muffled");
    assert!((restored - 1.0).abs() < 1e-4, "original not restored");
    assert!(after < 1e-3, "our copy kept playing");
}

/// A stream that is not attenuated must never be amplified a hundredfold.
#[test]
#[ignore]
fn guard_blocks_unattenuated_audio() {
    let _com = init_thread().unwrap();
    let tone = Tone::start(&[(440.0, 0.08)], 10);
    wait_for_session(tone.pid());
    let engine = Engine::start(vec![tone.pid()], Some(ATTENUATION)).expect("engine");
    engine.set_gain(1.0 / ATTENUATION, Duration::ZERO);
    std::thread::sleep(Duration::from_millis(300));
    let ours = rms(&record(std::process::id(), Duration::from_millis(400)));
    println!("overloads {} ours rms {ours:.5}", engine.overloads());
    engine.stop();
    assert!(ours < 1e-3, "full-volume audio was amplified");
}
