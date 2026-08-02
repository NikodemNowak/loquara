//! Krótkie dźwięki sygnalizacyjne (piknięcia) dla przepływu dyktowania.

fn sine_wav(frequency_hz: f32, duration_ms: u32, volume: f32, sample_rate: u32) -> Vec<u8> {
    let count = (sample_rate as u64 * u64::from(duration_ms)) / 1000;
    let mut samples = Vec::with_capacity(count as usize);
    for index in 0..count {
        let t = index as f32 / sample_rate as f32;
        let envelope = (index as f32 / count as f32).min(1.0);
        let attack = (index as f32 / (sample_rate as f32 * 0.01)).min(1.0);
        let value = (t * std::f32::consts::TAU * frequency_hz).sin() * volume * attack * (1.0 - envelope * 0.35);
        samples.push((value.clamp(-1.0, 1.0) * 32767.0) as i16);
    }

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

#[cfg(windows)]
fn play_wav(wav: &[u8]) {
    use std::os::raw::c_void;
    use windows_sys::Win32::Media::Audio::{PlaySoundW, SND_ASYNC, SND_MEMORY};
    let pointer = wav.as_ptr() as *const u16 as *const c_void;
    unsafe {
        PlaySoundW(
            pointer as windows_sys::core::PCWSTR,
            std::ptr::null_mut(),
            SND_ASYNC | SND_MEMORY,
        );
    }
}

#[cfg(not(windows))]
fn play_wav(_wav: &[u8]) {}

pub fn play_file(path: &std::path::Path) -> Result<(), String> {
    if !path.is_file() {
        return Err(format!("Nie znaleziono pliku audio: {}", path.display()));
    }
    hound::WavReader::open(path)
        .map(|_| ())
        .map_err(|error| format!("Nieprawidłowy plik WAV: {error}"))?;
    #[cfg(windows)]
    {
        use windows_sys::Win32::Media::Audio::{PlaySoundW, SND_FILENAME, SND_NODEFAULT, SND_SYNC};
        let path = std::fs::canonicalize(path).map_err(|error| error.to_string())?;
        let wide: Vec<u16> = path.to_string_lossy().encode_utf16().chain(std::iter::once(0)).collect();
        unsafe {
            PlaySoundW(
                wide.as_ptr(),
                std::ptr::null_mut(),
                SND_SYNC | SND_FILENAME | SND_NODEFAULT,
            );
        }
    }
    #[cfg(not(windows))]
    {
        let mut command = if cfg!(target_os = "macos") {
            let mut command = std::process::Command::new("afplay");
            command.arg(path);
            command
        } else {
            let mut command = std::process::Command::new("paplay");
            command.arg(path);
            command
        };
        command
            .spawn()
            .map_err(|error| format!("Nie można uruchomić odtwarzacza: {error}"))?;
    }
    Ok(())
}

pub fn play_recording_started() {
    play_wav(&sine_wav(880.0, 90, 0.35, 44_100));
}

pub fn play_recording_stopped() {
    play_wav(&sine_wav(660.0, 120, 0.35, 44_100));
}

pub fn play_transcription_ready() {
    let first = sine_wav(1046.0, 80, 0.35, 44_100);
    let second = sine_wav(1318.0, 90, 0.35, 44_100);
    let mut combined = first;
    combined.extend_from_slice(&[0u8; 2000]);
    combined.extend_from_slice(&second);
    play_wav(&combined);
}
