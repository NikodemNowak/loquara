//! Windows: per-app session volume through WASAPI and pause/resume through
//! the system media transport controls (GSMTC).

use super::{JournalEntry, Mixer, Policy, Target, normalize_app, recovered_target};
use windows::Media::Control::{
    GlobalSystemMediaTransportControlsSession as MediaSession,
    GlobalSystemMediaTransportControlsSessionManager as MediaSessionManager,
    GlobalSystemMediaTransportControlsSessionPlaybackStatus as PlaybackStatus,
};
use windows::Win32::Foundation::S_OK;
use windows::Win32::Media::Audio::*;
use windows::Win32::System::Com::*;
use windows_core::Interface;

/// Keeps COM (and WinRT) initialised on the worker thread.
pub(crate) struct ThreadGuard;

impl Drop for ThreadGuard {
    fn drop(&mut self) {
        unsafe { CoUninitialize() };
    }
}

pub(crate) fn init_thread() -> Option<ThreadGuard> {
    match unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) }.ok() {
        Ok(()) => Some(ThreadGuard),
        Err(error) => {
            eprintln!("Loquara media: COM init failed: {error}");
            None
        }
    }
}

pub(super) fn endpoints(flow: EDataFlow) -> windows_core::Result<Vec<IMMDevice>> {
    unsafe {
        let enumerator: IMMDeviceEnumerator = CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)?;
        let collection = enumerator.EnumAudioEndpoints(flow, DEVICE_STATE_ACTIVE)?;
        Ok((0..collection.GetCount()?).filter_map(|index| collection.Item(index).ok()).collect())
    }
}

pub(super) fn sessions(device: &IMMDevice) -> Vec<IAudioSessionControl2> {
    let result = (|| -> windows_core::Result<Vec<IAudioSessionControl2>> {
        unsafe {
            let manager: IAudioSessionManager2 = device.Activate(CLSCTX_ALL, None)?;
            let list = manager.GetSessionEnumerator()?;
            Ok((0..list.GetCount()?)
                .filter_map(|index| list.GetSession(index).ok())
                .filter_map(|session| session.cast::<IAudioSessionControl2>().ok())
                .collect())
        }
    })();
    result.unwrap_or_default()
}

pub(super) fn is_active(session: &IAudioSessionControl2) -> bool {
    matches!(unsafe { session.GetState() }, Ok(state) if state == AudioSessionStateActive)
}

/// Full image path, `None` when the process is gone or protected.
pub(super) fn process_path(pid: u32) -> Option<String> {
    use windows_sys::Win32::Foundation::CloseHandle;
    use windows_sys::Win32::System::Threading::{
        OpenProcess, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION, QueryFullProcessImageNameW,
    };
    unsafe {
        let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if handle.is_null() {
            return None;
        }
        let mut buffer = [0_u16; 1024];
        let mut size = buffer.len() as u32;
        let ok = QueryFullProcessImageNameW(handle, PROCESS_NAME_WIN32, buffer.as_mut_ptr(), &mut size);
        CloseHandle(handle);
        (ok != 0).then(|| String::from_utf16_lossy(&buffer[..size as usize]))
    }
}

/// Stable per-app session identity (no PID), so a journal entry still
/// matches after the app restarts and Windows re-applies its saved volume.
pub(super) fn session_identifier(session: &IAudioSessionControl2) -> Option<String> {
    unsafe {
        let text = session.GetSessionIdentifier().ok()?;
        let value = text.to_string().ok();
        CoTaskMemFree(Some(text.0 as *const _));
        value
    }
}

/// Policy id of everything a process plays (see [Policy::skips]).
pub(super) fn pid_id(pid: u32) -> String {
    format!("pid:{pid}")
}

pub(crate) fn foreign_capturers() -> Option<Vec<String>> {
    let own = std::process::id();
    let mut names = Vec::new();
    for device in endpoints(eCapture).ok()? {
        for session in sessions(&device) {
            if !is_active(&session) {
                continue;
            }
            let pid = unsafe { session.GetProcessId() }.unwrap_or(0);
            if pid == own || pid == 0 {
                continue;
            }
            // An empty name (protected process) is treated as a possible call.
            names.push(process_path(pid).unwrap_or_default());
        }
    }
    Some(names)
}

pub(super) struct SessionMixer {
    pub(super) volumes: Vec<ISimpleAudioVolume>,
}

impl Mixer for SessionMixer {
    fn read(&mut self, _targets: &[Target]) -> Vec<Option<Vec<f32>>> {
        self.volumes.iter().map(|volume| unsafe { volume.GetMasterVolume() }.ok().map(|level| vec![level])).collect()
    }

    fn write(&mut self, index: usize, volume: &[f32]) -> Option<Vec<f32>> {
        let (session, level) = (self.volumes.get(index)?, volume.first()?.clamp(0.0, 1.0));
        unsafe { session.SetMasterVolume(level, std::ptr::null()) }.ok()?;
        Some(vec![level])
    }
}

pub(crate) fn duck_targets(policy: &Policy) -> Option<(Box<dyn Mixer>, Vec<Target>)> {
    let own = std::process::id();
    let mut volumes = Vec::new();
    let mut targets = Vec::new();
    for device in endpoints(eRender).ok()? {
        for session in sessions(&device) {
            if !is_active(&session) || unsafe { session.IsSystemSoundsSession() } == S_OK {
                continue;
            }
            let pid = unsafe { session.GetProcessId() }.unwrap_or(0);
            if pid == own || pid == 0 || policy.skips(&pid_id(pid)) {
                continue;
            }
            let Some(path) = process_path(pid) else { continue };
            if !policy.allows(&path) {
                continue;
            }
            let Ok(volume) = session.cast::<ISimpleAudioVolume>() else { continue };
            let muted = unsafe { volume.GetMute() }.map(|muted| muted.as_bool()).unwrap_or(true);
            let Ok(level) = (unsafe { volume.GetMasterVolume() }) else { continue };
            if muted || level <= 0.01 {
                continue;
            }
            let key = session_identifier(&session).unwrap_or_else(|| normalize_app(&path));
            targets.push(Target::new(key, vec![level]));
            volumes.push(volume);
        }
    }
    Some((Box::new(SessionMixer { volumes }), targets))
}

pub(crate) fn recovery_targets(entries: &[JournalEntry]) -> Option<(Box<dyn Mixer>, Vec<Target>)> {
    let mut volumes = Vec::new();
    let mut targets = Vec::new();
    for device in endpoints(eRender).ok()? {
        for session in sessions(&device) {
            let Some(id) = session_identifier(&session) else { continue };
            let Some(entry) = entries.iter().find(|entry| entry.key == id) else { continue };
            let Ok(volume) = session.cast::<ISimpleAudioVolume>() else { continue };
            targets.push(recovered_target(entry));
            volumes.push(volume);
        }
    }
    Some((Box::new(SessionMixer { volumes }), targets))
}

fn media_sessions() -> Vec<MediaSession> {
    let result = (|| -> windows_core::Result<Vec<MediaSession>> {
        let manager = MediaSessionManager::RequestAsync()?.join()?;
        let list = manager.GetSessions()?;
        Ok((0..list.Size()?).filter_map(|index| list.GetAt(index).ok()).collect())
    })();
    result.unwrap_or_default()
}

fn status(session: &MediaSession) -> Option<PlaybackStatus> {
    session.GetPlaybackInfo().ok()?.PlaybackStatus().ok()
}

fn app_id(session: &MediaSession) -> Option<String> {
    session.SourceAppUserModelId().ok().map(|id| id.to_string())
}

pub(crate) fn pause(policy: &Policy) -> Vec<String> {
    let mut paused = Vec::new();
    for session in media_sessions() {
        let Some(app) = app_id(&session) else { continue };
        if !policy.allows(&app) || status(&session) != Some(PlaybackStatus::Playing) {
            continue;
        }
        if session.TryPauseAsync().and_then(|operation| operation.join()).unwrap_or(false) {
            paused.push(app);
        }
    }
    paused
}

pub(crate) fn resume(mut paused: Vec<String>) {
    for session in media_sessions() {
        if paused.is_empty() {
            break;
        }
        let Some(app) = app_id(&session) else { continue };
        let Some(position) = paused.iter().position(|id| *id == app) else { continue };
        // Still paused means nobody touched it; playing or stopped is the
        // user's choice.
        if status(&session) == Some(PlaybackStatus::Paused) {
            paused.swap_remove(position);
            let _ = session.TryPlayAsync().and_then(|operation| operation.join());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Read-only probe of the live system: `cargo test media::win -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn lists_capturers_and_candidates_without_touching_them() {
        let _thread = init_thread().expect("COM");
        let capturers = foreign_capturers();
        println!("capturers: {capturers:?}");
        let policy = Policy::from_capturers(capturers.as_deref());
        println!("policy: {policy:?}");
        let (mut mixer, targets) = duck_targets(&Policy::from_capturers(Some(&[]))).expect("render sessions");
        println!("duck candidates: {:?}", targets.iter().map(|target| (&target.key, &target.original)).collect::<Vec<_>>());
        println!("current: {:?}", mixer.read(&targets));
        let players: Vec<_> = media_sessions().iter().map(|session| (app_id(session), status(session))).collect();
        println!("media sessions: {players:?}");
    }
}

// ---------------------------------------------------------------------------
// Muffle ("music in the next room")
// ---------------------------------------------------------------------------

use super::win_muffle::{ATTENUATION, Engine};
use super::{Muffle, Stop, fade, restore};
use std::time::Duration;

const MUFFLE_RAMP: Duration = Duration::from_millis(300);
/// Session volume changes glide inside Windows and our copy runs ~40 ms
/// behind; the copy is faded in once the original has settled.
const HANDOVER: Duration = Duration::from_millis(50);
const GAIN_RAMP: Duration = Duration::from_millis(40);

fn instance_identifier(session: &IAudioSessionControl2) -> Option<String> {
    unsafe {
        let text = session.GetSessionInstanceIdentifier().ok()?;
        let value = text.to_string().ok();
        CoTaskMemFree(Some(text.0 as *const _));
        value
    }
}

/// An audible session of someone else's app the policy allows touching.
struct Candidate {
    pid: u32,
    volume: ISimpleAudioVolume,
    level: f32,
    key: String,
    instance: String,
}

fn candidates(policy: &Policy, only: Option<u32>) -> Vec<Candidate> {
    let own = std::process::id();
    let mut found = Vec::new();
    for device in endpoints(eRender).unwrap_or_default() {
        for session in sessions(&device) {
            if !is_active(&session) || unsafe { session.IsSystemSoundsSession() } == S_OK {
                continue;
            }
            let pid = unsafe { session.GetProcessId() }.unwrap_or(0);
            if pid == own || pid == 0 || only.is_some_and(|only| only != pid) {
                continue;
            }
            let Some(path) = process_path(pid) else { continue };
            if !policy.allows(&path) {
                continue;
            }
            let Ok(volume) = session.cast::<ISimpleAudioVolume>() else { continue };
            let muted = unsafe { volume.GetMute() }.map(|muted| muted.as_bool()).unwrap_or(true);
            let Ok(level) = (unsafe { volume.GetMasterVolume() }) else { continue };
            if muted || level <= 0.01 {
                continue;
            }
            let key = session_identifier(&session).unwrap_or_else(|| normalize_app(&path));
            let instance = instance_identifier(&session).unwrap_or_else(|| key.clone());
            found.push(Candidate { pid, volume, level, key, instance });
        }
    }
    found
}

struct Muffled {
    engine: Engine,
    mixer: SessionMixer,
    targets: Vec<Target>,
    pids: Vec<u32>,
    /// Session instances already handled, adopted or deliberately left.
    known: Vec<String>,
    policy: Policy,
}

pub(crate) fn muffle_begin(policy: &Policy, stop: &mut Stop) -> Option<Box<dyn Muffle>> {
    muffle_begin_only(policy, stop, None)
}

/// `only` limits the session to one process (live tests).
pub(super) fn muffle_begin_only(policy: &Policy, stop: &mut Stop, only: Option<u32>) -> Option<Box<dyn Muffle>> {
    let found = candidates(policy, only);
    let mut pids: Vec<u32> = found.iter().map(|candidate| candidate.pid).collect();
    pids.sort_unstable();
    pids.dedup();
    if pids.is_empty() {
        return None;
    }
    let engine = Engine::start(pids.clone(), Some(ATTENUATION))?;
    let pids: Vec<u32> = pids.into_iter().zip(&engine.started).filter(|(_, ok)| **ok).map(|(pid, _)| pid).collect();
    // A silent app still delivers silent packets, so this only times out
    // when capture is broken; the copy then stays muted by the guard anyway.
    engine.wait_playing(Duration::from_millis(300));
    if stop.fired() {
        return None;
    }
    let mut mixer = SessionMixer { volumes: Vec::new() };
    let mut targets = Vec::new();
    let mut known = Vec::new();
    for candidate in found {
        if pids.contains(&candidate.pid) {
            known.push(candidate.instance);
            targets.push(Target::new(candidate.key, vec![candidate.level]));
            mixer.volumes.push(candidate.volume);
        }
    }
    // Lower the originals; Windows glides the change itself.
    fade(&mut mixer, &mut targets, 1.0, ATTENUATION, Duration::ZERO, 1, None);
    let _ = stop.wait_for(HANDOVER);
    engine.set_gain(1.0 / ATTENUATION, GAIN_RAMP);
    engine.set_amount(1.0, MUFFLE_RAMP);
    Some(Box::new(Muffled { engine, mixer, targets, pids, known, policy: policy.clone() }))
}

impl Muffle for Muffled {
    fn claimed(&self) -> Vec<String> {
        self.pids.iter().map(|pid| pid_id(*pid)).collect()
    }

    fn targets(&self) -> &[Target] {
        &self.targets
    }

    /// A muffled app that opens a new stream (another tab, a new track)
    /// gets that stream lowered too; until then the guard keeps its
    /// full-volume audio out of our amplified copy.
    fn tick(&mut self) {
        for candidate in candidates(&self.policy, None) {
            if !self.pids.contains(&candidate.pid) || self.known.contains(&candidate.instance) {
                continue;
            }
            self.known.push(candidate.instance);
            if candidate.level <= ATTENUATION * 1.5 {
                continue;
            }
            self.mixer.volumes.push(candidate.volume);
            let mut target = Target::new(candidate.key, vec![candidate.level]);
            let index = self.mixer.volumes.len() - 1;
            target.applied = self.mixer.write(index, &super::scaled(&target.original, ATTENUATION));
            target.owned = target.applied.is_some();
            self.targets.push(target);
        }
    }

    fn end(self: Box<Self>) {
        let mut this = *self;
        // Open the filter while still playing our copy, then hand back.
        this.engine.set_amount(0.0, MUFFLE_RAMP);
        std::thread::sleep(MUFFLE_RAMP + Duration::from_millis(20));
        this.engine.set_gain(0.0, Duration::from_millis(30));
        std::thread::sleep(Duration::from_millis(35));
        restore(&mut this.mixer, &mut this.targets, ATTENUATION, 1);
        this.engine.stop();
    }
}

pub(crate) fn recover_muffle() {}
