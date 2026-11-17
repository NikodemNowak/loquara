//! macOS: no public per-app volume or system-wide media pause exists, so
//! ducking lowers the system output volume (only when nobody else is using
//! audio input) and pausing drives Spotify and Music through AppleScript.

use super::command::run;
use super::{JournalEntry, Mixer, Policy, Target, is_meeting_app, normalize_app, recovered_target};
use std::time::Duration;

const SCRIPT_TIMEOUT: Duration = Duration::from_secs(4);
const SYSTEM_OUTPUT: &str = "system-output";
/// Pausable players: (process name, AppleScript application name).
const PLAYERS: &[(&str, &str)] = &[("Spotify", "Spotify"), ("Music", "Music")];
/// System daemons that keep audio input open without anyone talking.
const BACKGROUND_INPUT: &[&str] = &["corespeechd", "coreaudiod", "assistantd", "siriactionsd", "heard", "systemsoundserverd"];

pub(crate) struct ThreadGuard;

pub(crate) fn init_thread() -> Option<ThreadGuard> {
    Some(ThreadGuard)
}

/// CoreAudio's per-process view (macOS 14+).
mod coreaudio {
    use std::ffi::c_void;

    #[repr(C)]
    struct Address {
        selector: u32,
        scope: u32,
        element: u32,
    }

    #[link(name = "CoreAudio", kind = "framework")]
    unsafe extern "C" {
        fn AudioObjectGetPropertyDataSize(
            object: u32,
            address: *const Address,
            qualifier_size: u32,
            qualifier: *const c_void,
            size: *mut u32,
        ) -> i32;
        fn AudioObjectGetPropertyData(
            object: u32,
            address: *const Address,
            qualifier_size: u32,
            qualifier: *const c_void,
            size: *mut u32,
            data: *mut c_void,
        ) -> i32;
    }

    const fn code(value: &[u8; 4]) -> u32 {
        u32::from_be_bytes(*value)
    }
    const SYSTEM_OBJECT: u32 = 1;
    const SCOPE_GLOBAL: u32 = code(b"glob");
    const ELEMENT_MAIN: u32 = 0;
    const PROCESS_OBJECT_LIST: u32 = code(b"prs#");
    const PROCESS_PID: u32 = code(b"ppid");
    const PROCESS_IS_RUNNING_INPUT: u32 = code(b"piri");

    fn address(selector: u32) -> Address {
        Address { selector, scope: SCOPE_GLOBAL, element: ELEMENT_MAIN }
    }

    fn scalar<T: Copy + Default>(object: u32, selector: u32) -> Option<T> {
        let address = address(selector);
        let mut value = T::default();
        let mut size = std::mem::size_of::<T>() as u32;
        let status = unsafe {
            AudioObjectGetPropertyData(object, &address, 0, std::ptr::null(), &mut size, (&mut value as *mut T).cast())
        };
        (status == 0 && size as usize == std::mem::size_of::<T>()).then_some(value)
    }

    /// PIDs of processes currently running audio input; `None` when the
    /// system does not expose the process list (before macOS 14).
    pub(super) fn input_pids() -> Option<Vec<i32>> {
        let address = address(PROCESS_OBJECT_LIST);
        let mut size = 0_u32;
        if unsafe { AudioObjectGetPropertyDataSize(SYSTEM_OBJECT, &address, 0, std::ptr::null(), &mut size) } != 0 {
            return None;
        }
        let mut objects = vec![0_u32; size as usize / std::mem::size_of::<u32>()];
        if !objects.is_empty() {
            let status = unsafe {
                AudioObjectGetPropertyData(SYSTEM_OBJECT, &address, 0, std::ptr::null(), &mut size, objects.as_mut_ptr().cast())
            };
            if status != 0 {
                return None;
            }
            objects.truncate(size as usize / std::mem::size_of::<u32>());
        }
        Some(
            objects
                .into_iter()
                .filter(|object| scalar::<u32>(*object, PROCESS_IS_RUNNING_INPUT).unwrap_or(0) != 0)
                .filter_map(|object| scalar::<i32>(object, PROCESS_PID))
                .collect(),
        )
    }
}

pub(super) fn process_names(pids: &[i32]) -> Option<Vec<String>> {
    if pids.is_empty() {
        return Some(Vec::new());
    }
    let list = pids.iter().map(i32::to_string).collect::<Vec<_>>().join(",");
    let text = run("ps", &["-o", "comm=", "-p", list.as_str()], SCRIPT_TIMEOUT)?;
    Some(text.lines().map(str::trim).filter(|line| !line.is_empty()).map(str::to_owned).collect())
}

pub(crate) fn foreign_capturers() -> Option<Vec<String>> {
    let own = std::process::id() as i32;
    if let Some(pids) = coreaudio::input_pids() {
        let pids: Vec<i32> = pids.into_iter().filter(|pid| *pid != own && *pid > 0).collect();
        // Processes that exited in between simply drop out; a failed `ps`
        // is an unidentified capturer, which stands everything down.
        let names = process_names(&pids).unwrap_or_else(|| vec![String::new()]);
        return Some(names.into_iter().filter(|name| !BACKGROUND_INPUT.contains(&normalize_app(name).as_str())).collect());
    }
    // Older macOS: no per-process view. Any running meeting app counts as a
    // possible call, which is conservative but never wrong.
    let text = run("ps", &["-axo", "comm="], SCRIPT_TIMEOUT)?;
    Some(text.lines().map(str::trim).filter(|name| is_meeting_app(name)).map(str::to_owned).collect())
}

fn osascript(lines: &[&str]) -> Option<String> {
    let mut args = Vec::with_capacity(lines.len() * 2);
    for line in lines {
        args.push("-e");
        args.push(*line);
    }
    run("osascript", &args, SCRIPT_TIMEOUT).map(|text| text.trim().to_owned())
}

/// Output volume as 0..=1, `None` when muted or not adjustable (some HDMI
/// and USB outputs report `missing value`).
fn output_volume() -> Option<f32> {
    let text = osascript(&[
        "set s to get volume settings",
        "return ((output volume of s) as text) & \",\" & ((output muted of s) as text)",
    ])?;
    let (volume, muted) = text.split_once(',')?;
    if muted.trim() != "false" {
        return None;
    }
    volume.trim().parse::<f32>().ok().map(|volume| volume / 100.0)
}

struct SystemMixer;

impl Mixer for SystemMixer {
    fn read(&mut self, targets: &[Target]) -> Vec<Option<Vec<f32>>> {
        let current = output_volume().map(|volume| vec![volume]);
        targets.iter().map(|_| current.clone()).collect()
    }

    fn write(&mut self, _index: usize, volume: &[f32]) -> Option<Vec<f32>> {
        let percent = (volume.first()? * 100.0).round().clamp(0.0, 100.0) as u32;
        osascript(&[format!("set volume output volume {percent}").as_str()])?;
        Some(vec![percent as f32 / 100.0])
    }

    fn steps(&self) -> u32 {
        3
    }
}

pub(crate) fn duck_targets(policy: &Policy) -> Option<(Box<dyn Mixer>, Vec<Target>)> {
    // The system volume carries everyone's audio, a call's included, and
    // while muffling also our own filtered copy.
    if policy.foreign_capture || policy.has_skips() {
        return None;
    }
    let volume = output_volume().filter(|volume| *volume > 0.01)?;
    Some((Box::new(SystemMixer), vec![Target::new(SYSTEM_OUTPUT.to_owned(), vec![volume])]))
}

pub(crate) fn recovery_targets(entries: &[JournalEntry]) -> Option<(Box<dyn Mixer>, Vec<Target>)> {
    let entry = entries.iter().find(|entry| entry.key == SYSTEM_OUTPUT)?;
    Some((Box::new(SystemMixer), vec![recovered_target(entry)]))
}

/// `tell application` would launch a closed app (or ask where an
/// uninstalled one is), so only running players are addressed.
fn is_running(process: &str) -> bool {
    run("pgrep", &["-x", process], SCRIPT_TIMEOUT).is_some()
}

fn player_script(app: &str, from_state: &str, action: &str) -> bool {
    let tell = format!("tell application \"{app}\"");
    let check = format!("if player state is {from_state} then");
    osascript(&[tell.as_str(), check.as_str(), action, "return \"done\"", "end if", "end tell", "return \"skip\""]).as_deref() == Some("done")
}

pub(crate) fn pause(policy: &Policy) -> Vec<String> {
    PLAYERS
        .iter()
        .filter(|(process, _)| policy.allows(process) && is_running(process))
        .filter(|(_, app)| player_script(app, "playing", "pause"))
        .map(|(_, app)| (*app).to_owned())
        .collect()
}

pub(crate) fn resume(paused: Vec<String>) {
    for app in paused {
        let process = PLAYERS.iter().find(|(_, name)| *name == app).map_or(app.as_str(), |(process, _)| *process);
        if is_running(process) {
            player_script(&app, "paused", "play");
        }
    }
}

pub(crate) fn muffle_begin(policy: &Policy, stop: &mut super::Stop) -> Option<Box<dyn super::Muffle>> {
    super::mac_muffle::muffle_begin(policy, stop, process_names)
}

/// The system destroys a crashed process's taps, which unmutes the apps.
pub(crate) fn recover_muffle() {}