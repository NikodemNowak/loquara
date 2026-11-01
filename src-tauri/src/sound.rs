//! Short cue sounds (beeps) for the dictation flow.

fn sine_pcm(frequency_hz: f32, duration_ms: u32, volume: f32, sample_rate: u32) -> Vec<i16> {
    let count = (sample_rate as u64 * u64::from(duration_ms)) / 1000;
    let mut samples = Vec::with_capacity(count as usize);
    for index in 0..count {
        let t = index as f32 / sample_rate as f32;
        let envelope = (index as f32 / count as f32).min(1.0);
        let attack = (index as f32 / (sample_rate as f32 * 0.01)).min(1.0);
        let value = (t * std::f32::consts::TAU * frequency_hz).sin() * volume * attack * (1.0 - envelope * 0.35);
        samples.push((value.clamp(-1.0, 1.0) * 32767.0) as i16);
    }
    samples
}

fn wrap_wav(samples: &[i16], sample_rate: u32) -> Vec<u8> {
    let header_size = 44;
    let data_size = samples.len() * 2;
    let mut wav = Vec::with_capacity(header_size + data_size);
    wav.extend_from_slice(b"RIFF");
    wav.extend_from_slice(&((36 + data_size) as u32).to_le_bytes());
    wav.extend_from_slice(b"WAVE");
    wav.extend_from_slice(b"fmt ");
    wav.extend_from_slice(&16u32.to_le_bytes());
    wav.extend_from_slice(&1u16.to_le_bytes());
    wav.extend_from_slice(&1u16.to_le_bytes());
    wav.extend_from_slice(&sample_rate.to_le_bytes());
    wav.extend_from_slice(&(sample_rate * 2).to_le_bytes());
    wav.extend_from_slice(&2u16.to_le_bytes());
    wav.extend_from_slice(&16u16.to_le_bytes());
    wav.extend_from_slice(b"data");
    wav.extend_from_slice(&(data_size as u32).to_le_bytes());
    for sample in samples {
        wav.extend_from_slice(&sample.to_le_bytes());
    }
    wav
}

/// Builds a short melody: notes back to back with a tiny gap, one WAV header.
fn sequence(notes: &[(f32, u32)], volume: f32, sample_rate: u32) -> Vec<i16> {
    let gap_samples = (sample_rate as usize * 5) / 1000;
    let mut combined = Vec::new();
    for (index, (frequency, duration)) in notes.iter().enumerate() {
        if index > 0 {
            combined.extend(std::iter::repeat(0i16).take(gap_samples));
        }
        combined.extend(sine_pcm(*frequency, *duration, volume, sample_rate));
    }
    combined
}

#[cfg(windows)]
fn play_wav(wav: &[u8]) {
    use std::os::raw::c_void;
    use windows_sys::Win32::Media::Audio::{PlaySoundW, SND_MEMORY, SND_SYNC};
    // PlaySoundW reads the buffer asynchronously, so keep it alive on a
    // dedicated thread that blocks until playback finishes (no use-after-free).
    let owned = wav.to_vec();
    std::thread::spawn(move || {
        let pointer = owned.as_ptr() as *const u16 as *const c_void;
        unsafe {
            PlaySoundW(
                pointer as windows_sys::core::PCWSTR,
                std::ptr::null_mut(),
                SND_SYNC | SND_MEMORY,
            );
        }
    });
}

/// Plays a cue through the desktop's own audio player.
///
/// The SDKs that expose a "play this buffer" call are platform-specific, and
/// spending a sound-library dependency on three short beeps is not worth it.
/// A temporary WAV and the player every desktop already has is.
#[cfg(not(windows))]
fn play_wav(wav: &[u8]) {
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT_CUE: AtomicU64 = AtomicU64::new(0);
    let path = std::env::temp_dir().join(format!(
        "loquara-cue-{}-{}.wav",
        std::process::id(),
        NEXT_CUE.fetch_add(1, Ordering::Relaxed)
    ));
    if std::fs::write(&path, wav).is_err() {
        return;
    }

    std::thread::spawn(move || {
        #[cfg(target_os = "macos")]
        let players: &[(&str, &[&str])] = &[("afplay", &[])];
        #[cfg(all(unix, not(target_os = "macos")))]
        let players: &[(&str, &[&str])] = &[
            ("pw-play", &[]),
            ("paplay", &[]),
            ("aplay", &["-q"]),
        ];

        for (program, args) in players {
            let Ok(status) = std::process::Command::new(program)
                .args(*args)
                .arg(&path)
                .status()
            else {
                continue;
            };
            if status.success() {
                break;
            }
        }
        let _ = std::fs::remove_file(&path);
    });
}
pub fn play_file(path: &std::path::Path) -> Result<(), String> {
    if !path.is_file() {
        return Err(format!("Nie znaleziono pliku audio: {}", path.display()));
    }
    hound::WavReader::open(path)
        .map(|_| ())
        .map_err(|error| format!("Invalid WAV file: {error}"))?;
    #[cfg(windows)]
    {
        use windows_sys::Win32::Media::Audio::{PlaySoundW, SND_ASYNC, SND_FILENAME, SND_NODEFAULT};
        let path = std::fs::canonicalize(path).map_err(|error| error.to_string())?;
        let wide: Vec<u16> = path.to_string_lossy().encode_utf16().chain(std::iter::once(0)).collect();
        unsafe {
            PlaySoundW(
                wide.as_ptr(),
                std::ptr::null_mut(),
                SND_ASYNC | SND_FILENAME | SND_NODEFAULT,
            );
        }
    }
    #[cfg(target_os = "macos")]
    {
        std::process::Command::new("afplay")
            .arg(path)
            .spawn()
            .map_err(|error| format!("Could not start the audio player: {error}"))?;
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        let mut last_error = None;
        for program in ["pw-play", "paplay", "aplay"] {
            match std::process::Command::new(program).arg(path).spawn() {
                Ok(_) => return Ok(()),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => last_error = Some(error),
                Err(error) => {
                    return Err(format!("Could not start the audio player: {error}"));
                }
            }
        }
        return Err(format!(
            "Could not start an audio player: {}",
            last_error
                .map(|error| error.to_string())
                .unwrap_or_else(|| "none is installed".into())
        ));
    }
    Ok(())
}

/// Dictation started: short, energetic rising cue.
pub fn play_recording_started() {
    play_wav(&wrap_wav(&sequence(&[(587.0, 55), (880.0, 75)], 0.35, 44_100), 44_100));
}

/// Dictation stopped: clearly contrasting falling cue.
pub fn play_recording_stopped() {
    play_wav(&wrap_wav(&sequence(&[(523.0, 55), (392.0, 95)], 0.35, 44_100), 44_100));
}

/// Transcript ready to paste: three rising notes.
pub fn play_transcription_ready() {
    play_wav(&wrap_wav(
        &sequence(&[(784.0, 55), (1046.0, 55), (1318.0, 90)], 0.35, 44_100),
        44_100,
    ));
}