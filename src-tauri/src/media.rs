//! Background listening audio while the user dictates: muffled as if it
//! played in the next room (default), ducked, paused, or left alone, and
//! put back exactly as it was afterwards.
//!
//! Everything runs on one short-lived worker thread per dictation, so
//! [`MediaController::begin`] only spawns a thread and returns immediately.
//! Every failure is soft: muffle falls back to ducking, and the worst
//! outcome is that nothing changes.
//!
//! Safety rules, applied on every platform and mode:
//! - Calls and meetings are never touched. Known meeting apps (Teams, Zoom,
//!   Slack, Discord, Webex, FaceTime, Skype, ...) are skipped per stream, and
//!   when a meeting app is capturing the microphone the whole feature stands
//!   down for that dictation. Any other app capturing the microphone (for
//!   example a browser running Google Meet) is itself left alone.
//! - Only what we changed is restored, and only if it still holds the value
//!   we wrote. If the user moved a volume slider, moved a stream or
//!   resumed/paused a player meanwhile, their choice wins.
//! - Changed volumes are journaled to a temp file so a crash mid-dictation
//!   can be undone on the next start ([`MediaController::recover_after_crash`]).
//!
//! Muffle ([`dsp`]): the apps' audio is taken over, run through a 24 dB/oct
//! low-pass gliding from 18 kHz to 600 Hz over 300 ms, −8 dB and a hint of
//! early reflections, and played by us; the originals are silenced.
//! - Windows ([`win_muffle`]): per-process loopback capture. It hears an app
//!   after its session volume, so the original is lowered to 1 % (−40 dB)
//!   and the capture amplified back, with a guard against amplifying a
//!   stream that is not lowered. Verified on the real stack by `win_live`.
//!   Adds ~40 ms latency and a ~0.1 s gap at the hand-over.
//! - macOS 14.2+ ([`mac_muffle`]): Core Audio process tap that mutes the
//!   tapped apps while we read it; needs the system audio recording
//!   permission (Info.plist `NSAudioCaptureUsageDescription`).
//! - Linux: streams are moved to a private null sink, `parec` records its
//!   monitor and `pacat` plays the filtered audio on the default sink.
//!
//! Duck: per-app volume glides to ~22 % (Windows WASAPI sessions, Linux
//! `pactl` streams; macOS lowers the system output volume, only when nobody
//! else uses audio input). Pause: system media controls on Windows, MPRIS
//! on Linux, Spotify and Music via AppleScript on macOS.
mod dsp;
mod pactl;

#[cfg(windows)]
mod win;
#[cfg(windows)]
use win as platform;
#[cfg(windows)]
mod win_muffle;
#[cfg(all(test, windows))]
mod win_live;

#[cfg(target_os = "macos")]
mod mac;
#[cfg(target_os = "macos")]
use mac as platform;
#[cfg(target_os = "macos")]
mod mac_muffle;

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "linux")]
use linux as platform;

#[cfg(any(target_os = "macos", target_os = "linux"))]
mod command;

#[cfg(not(any(windows, target_os = "macos", target_os = "linux")))]
mod platform {
    use super::{JournalEntry, Mixer, Policy, Target};
    pub(crate) struct ThreadGuard;
    pub(crate) fn init_thread() -> Option<ThreadGuard> { Some(ThreadGuard) }
    pub(crate) fn foreign_capturers() -> Option<Vec<String>> { None }
    pub(crate) fn duck_targets(_: &Policy) -> Option<(Box<dyn Mixer>, Vec<Target>)> { None }
    pub(crate) fn recovery_targets(_: &[JournalEntry]) -> Option<(Box<dyn Mixer>, Vec<Target>)> { None }
    pub(crate) fn pause(_: &Policy) -> Vec<String> { Vec::new() }
    pub(crate) fn muffle_begin(_: &Policy, _: &mut super::Stop) -> Option<Box<dyn super::Muffle>> { None }
    pub(crate) fn recover_muffle() {}
    pub(crate) fn resume(_: Vec<String>) {}
}

use serde::{Deserialize, Deserializer, Serialize};
use std::path::PathBuf;
use std::sync::Mutex;
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender, TryRecvError};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

/// Share of the original volume kept while ducked.
pub const DEFAULT_DUCK_LEVEL: f32 = 0.22;
const FADE_DOWN: Duration = Duration::from_millis(200);
const FADE_UP: Duration = Duration::from_millis(250);
/// Safety net: a guard that is never ended still restores after this long.
const MAX_HOLD: Duration = Duration::from_secs(60 * 60);
/// Volumes are normalised to 1.0 = 100 %. Comparisons allow 5 % of the
/// value (at most 0.011), so a near-silent level we set (muffle) is not
/// confused with the user dragging the slider to zero.
fn tolerance(applied: f32) -> f32 {
    (applied.abs() * 0.05).clamp(0.0015, 0.011)
}

/// One dictation at a time touches the mixer, so a new dictation never
/// captures the still-ducked volume of the previous one as "original".
static SERIAL: Mutex<()> = Mutex::new(());

/// The `media_during_dictation` setting.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum MediaMode {
    /// Background audio sounds like it plays in the next room.
    #[default]
    Muffle,
    Duck,
    Pause,
    Off,
}

impl MediaMode {
    pub fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "muffle" => Some(Self::Muffle),
            "duck" => Some(Self::Duck),
            "pause" => Some(Self::Pause),
            "off" => Some(Self::Off),
            _ => None,
        }
    }

    /// Lenient form for stored settings: anything unknown is the default.
    pub fn from_setting(value: &str) -> Self {
        Self::parse(value).unwrap_or_default()
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Muffle => "muffle",
            Self::Duck => "duck",
            Self::Pause => "pause",
            Self::Off => "off",
        }
    }
}

/// Unknown stored values fall back to the default instead of failing the
/// whole settings file.
impl<'de> Deserialize<'de> for MediaMode {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Ok(Self::from_setting(&String::deserialize(deserializer)?))
    }
}

/// Cheap to copy; keep one in app state or build one per dictation.
#[derive(Clone, Copy, Debug)]
pub struct MediaController {
    duck_level: f32,
}

impl Default for MediaController {
    fn default() -> Self {
        Self::new(DEFAULT_DUCK_LEVEL)
    }
}

impl MediaController {
    /// `duck_level` is the share of the original volume kept (0.05..=1.0).
    pub fn new(duck_level: f32) -> Self {
        let duck_level = if duck_level.is_finite() { duck_level.clamp(0.05, 1.0) } else { DEFAULT_DUCK_LEVEL };
        Self { duck_level }
    }

    /// Starts ducking or pausing in the background and returns at once.
    /// `None` for [`MediaMode::Off`] or when no thread could be started.
    /// Ending (or dropping) the guard restores everything that was changed.
    pub fn begin(&self, mode: MediaMode) -> Option<MediaGuard> {
        if mode == MediaMode::Off {
            return None;
        }
        let (stop, signal) = mpsc::channel();
        let level = self.duck_level;
        let worker = std::thread::Builder::new()
            .name("loquara-media".into())
            .spawn(move || run(mode, level, signal));
        match worker {
            Ok(worker) => Some(MediaGuard { stop: Some(stop), worker: Some(worker) }),
            Err(error) => {
                eprintln!("Loquara media: cannot start worker: {error}");
                None
            }
        }
    }

    /// Restores volumes left ducked by a previous run that crashed or was
    /// killed mid-dictation. Runs in the background; call once at startup.
    pub fn recover_after_crash(&self) {
        let _ = std::thread::Builder::new().name("loquara-media-recover".into()).spawn(journal::recover);
    }
}

/// Holds ducked/paused media until ended or dropped.
pub struct MediaGuard {
    stop: Option<Sender<()>>,
    worker: Option<JoinHandle<()>>,
}

impl MediaGuard {
    /// Asks the worker to restore and returns immediately. Idempotent with
    /// drop, so calling it on stop, cancel and failure paths alike is safe.
    pub fn end(mut self) {
        self.signal();
    }

    /// Like [`end`](Self::end) but waits up to `timeout` for the restore to
    /// finish. Meant for app shutdown, where the process would otherwise
    /// exit before the volume comes back. Returns whether it finished.
    pub fn end_and_wait(mut self, timeout: Duration) -> bool {
        self.signal();
        let Some(worker) = self.worker.take() else { return true };
        let deadline = Instant::now() + timeout;
        while !worker.is_finished() {
            if Instant::now() >= deadline {
                return false;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        let _ = worker.join();
        true
    }

    fn signal(&mut self) {
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
    }
}

#[cfg(test)]
impl MediaGuard {
    /// A guard with no worker, for tests that must not touch real audio.
    pub(crate) fn inert() -> Self {
        let (stop, _) = mpsc::channel();
        Self { stop: Some(stop), worker: None }
    }
}

impl Drop for MediaGuard {
    fn drop(&mut self) {
        self.signal();
    }
}

/// End-of-dictation signal as seen by the worker. A dropped sender counts
/// as "ended" too.
struct Stop {
    signal: Receiver<()>,
    fired: bool,
}

impl Stop {
    fn fired(&mut self) -> bool {
        if !self.fired && !matches!(self.signal.try_recv(), Err(TryRecvError::Empty)) {
            self.fired = true;
        }
        self.fired
    }

    /// Waits up to `timeout`; true once dictation has ended.
    fn wait_for(&mut self, timeout: Duration) -> bool {
        if !self.fired && !matches!(self.signal.recv_timeout(timeout), Err(RecvTimeoutError::Timeout)) {
            self.fired = true;
        }
        self.fired
    }
}

fn run(mode: MediaMode, level: f32, signal: Receiver<()>) {
    let _serial = SERIAL.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    let mut stop = Stop { signal, fired: false };
    // Dictation may already be over while a previous restore held the lock.
    if stop.fired() {
        return;
    }
    let Some(_thread) = platform::init_thread() else { return };
    let policy = Policy::from_capturers(platform::foreign_capturers().as_deref());
    if policy.skip_all {
        return;
    }
    match mode {
        MediaMode::Muffle => muffle(level, &policy, &mut stop),
        MediaMode::Duck => {
            let ducked = duck_begin(level, &policy, &mut stop);
            if let Some(ducked) = &ducked {
                journal::save(&[&ducked.targets[..]]);
            }
            stop.wait_for(MAX_HOLD);
            duck_end(ducked);
        }
        MediaMode::Pause => {
            let paused = platform::pause(&policy);
            if paused.is_empty() {
                return;
            }
            stop.wait_for(MAX_HOLD);
            platform::resume(paused);
        }
        MediaMode::Off => {}
    }
}

struct Ducked {
    mixer: Box<dyn Mixer>,
    targets: Vec<Target>,
    reached: f32,
}

fn duck_begin(level: f32, policy: &Policy, stop: &mut Stop) -> Option<Ducked> {
    let (mut mixer, mut targets) = platform::duck_targets(policy)?;
    if targets.is_empty() {
        return None;
    }
    let steps = mixer.steps();
    let reached = fade(mixer.as_mut(), &mut targets, 1.0, level, FADE_DOWN, steps, Some(stop));
    Some(Ducked { mixer, targets, reached })
}

fn duck_end(ducked: Option<Ducked>) {
    let Some(mut ducked) = ducked else { return };
    let steps = ducked.mixer.steps();
    restore(ducked.mixer.as_mut(), &mut ducked.targets, ducked.reached, steps);
    journal::clear();
}

/// A running "next room" session on one platform.
pub(crate) trait Muffle {
    /// Policy ids (see [`Policy::skips`]) of the audio this session took
    /// over, so ducking leaves them alone.
    fn claimed(&self) -> Vec<String>;
    /// Volumes changed to silence the originals; journaled for recovery.
    fn targets(&self) -> &[Target];
    /// Called about every 100 ms while dictating (adopt new streams etc.).
    fn tick(&mut self) {}
    /// Opens the filter back up, restores the originals, stops playback.
    fn end(self: Box<Self>);
}

/// Muffles what it can and ducks the rest (apps whose audio could not be
/// captured, other output devices). Without muffle support it is a duck.
fn muffle(level: f32, policy: &Policy, stop: &mut Stop) {
    let Some(mut muffled) = platform::muffle_begin(policy, stop) else {
        let ducked = duck_begin(level, policy, stop);
        if let Some(ducked) = &ducked {
            journal::save(&[&ducked.targets[..]]);
        }
        stop.wait_for(MAX_HOLD);
        duck_end(ducked);
        return;
    };
    let rest = policy.skipping(muffled.claimed());
    let ducked = duck_begin(level, &rest, stop);
    let empty = Vec::new();
    journal::save(&[muffled.targets(), ducked.as_ref().map_or(&empty[..], |ducked| &ducked.targets[..])]);
    let deadline = Instant::now() + MAX_HOLD;
    while !stop.wait_for(Duration::from_millis(100)) && Instant::now() < deadline {
        muffled.tick();
    }
    muffled.end();
    duck_end(ducked);
    journal::clear();
}
/// One volume we changed (a WASAPI session, a pactl stream, the macOS
/// output). Volumes are per channel, normalised so 1.0 = 100 %.
pub(crate) struct Target {
    /// Identity that survives a restart of our app (journal key).
    pub key: String,
    pub original: Vec<f32>,
    /// What we wrote last; `None` before the first write.
    pub applied: Option<Vec<f32>>,
    /// False once we lost track of it or the user took over.
    pub owned: bool,
}

impl Target {
    pub fn new(key: String, original: Vec<f32>) -> Self {
        Self { key, original, applied: None, owned: true }
    }
}

/// Platform volume access. `index` is the target's position in the list
/// the platform returned alongside the mixer.
pub(crate) trait Mixer {
    fn read(&mut self, targets: &[Target]) -> Vec<Option<Vec<f32>>>;
    /// Sets a volume; returns what the system actually holds now (after its
    /// own rounding), or None when the target is gone.
    fn write(&mut self, index: usize, volume: &[f32]) -> Option<Vec<f32>>;
    /// Fade granularity; lower where every write spawns a process.
    fn steps(&self) -> u32 {
        10
    }
}

fn ease(t: f32) -> f32 {
    let t = t.clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

pub(crate) fn scaled(original: &[f32], gain: f32) -> Vec<f32> {
    original.iter().map(|value| value * gain).collect()
}

/// The restore-only-if-unchanged rule: the volume must still be what we
/// wrote, otherwise the user (or the app) changed it and it is theirs now.
pub(crate) fn still_ours(current: Option<&[f32]>, applied: Option<&[f32]>) -> bool {
    match (current, applied) {
        (Some(current), Some(applied)) => {
            current.len() == applied.len()
                && current.iter().zip(applied).all(|(current, applied)| (current - applied).abs() <= tolerance(*applied))
        }
        _ => false,
    }
}

fn apply(mixer: &mut dyn Mixer, targets: &mut [Target], gain: f32) {
    for (index, target) in targets.iter_mut().enumerate() {
        if !target.owned {
            continue;
        }
        let volume = if gain >= 1.0 { target.original.clone() } else { scaled(&target.original, gain) };
        match mixer.write(index, &volume) {
            Some(applied) => target.applied = Some(applied),
            None => target.owned = false,
        }
    }
}

/// Glides every owned target from `from` to `to` (gains relative to the
/// original). Stops early when `stop` fires; returns the gain reached.
fn fade(
    mixer: &mut dyn Mixer,
    targets: &mut [Target],
    from: f32,
    to: f32,
    duration: Duration,
    steps: u32,
    mut stop: Option<&mut Stop>,
) -> f32 {
    let steps = steps.max(1);
    let pause = duration / steps;
    let mut gain = from;
    for step in 1..=steps {
        gain = if step == steps { to } else { from + (to - from) * ease(step as f32 / steps as f32) };
        apply(mixer, targets, gain);
        if step < steps {
            match stop.as_deref_mut() {
                Some(stop) => {
                    if stop.wait_for(pause) {
                        break;
                    }
                }
                None => std::thread::sleep(pause),
            }
        }
    }
    gain
}

fn restore(mixer: &mut dyn Mixer, targets: &mut [Target], from: f32, steps: u32) {
    let current = mixer.read(targets);
    for (target, current) in targets.iter_mut().zip(current.iter().map(Option::as_deref).chain(std::iter::repeat(None))) {
        target.owned = target.owned && still_ours(current, target.applied.as_deref());
    }
    if targets.iter().any(|target| target.owned) {
        fade(mixer, targets, from, 1.0, FADE_UP, steps, None);
    }
}

// ---------------------------------------------------------------------------
// Meeting safety
// ---------------------------------------------------------------------------

/// Matched anywhere in the normalised process name / app id.
const MEETING_SUBSTRINGS: &[&str] = &[
    "teams", "zoom", "cpthost", "aomhost", "slack", "discord", "webex", "ciscocollabhost", "skype", "lync",
    "facetime", "avconferenced", "gotomeeting", "g2mcomm", "g2mlauncher", "ringcentral", "bluejeans", "jitsi",
    "whatsapp", "telegram", "viber", "chime", "zoiper", "linphone", "microsip", "3cx", "dialpad", "mattermost",
    "lifesize", "vonage", "8x8", "phoneexperiencehost", "yourphone", "signal-desktop", "element-desktop",
    "rocketchat", "nextcloud-talk", "mumble", "ts3client", "wickr", "pexip", "bigbluebutton",
    // Embedded Edge web views host new Teams and Outlook calls; also our own UI.
    "msedgewebview2",
];
/// Too short or generic for substring matching.
const MEETING_EXACT: &[&str] = &["signal", "element", "wire", "atmgr", "around", "tuple", "pop", "jami", "ekiga"];

const BROWSER_PREFIXES: &[(&str, &str)] = &[
    ("chrome", "chrome"),
    ("google-chrome", "chrome"),
    ("chromium", "chrome"),
    ("msedge", "edge"),
    ("microsoft-edge", "edge"),
    ("microsoftedge", "edge"),
    ("firefox", "firefox"),
    // Firefox's media-session app id on Windows.
    ("308046b0af4a39cb", "firefox"),
    ("librewolf", "firefox"),
    ("waterfox", "firefox"),
    ("brave", "brave"),
    ("opera", "opera"),
    ("vivaldi", "vivaldi"),
    ("yandex", "yandex"),
    ("thorium", "thorium"),
    ("safari", "safari"),
];
const BROWSER_EXACT: &[(&str, &str)] = &[("arc", "arc"), ("zen", "zen"), ("com.apple.webkit.gpu", "safari")];

/// Lower-case base name without path, extension or packaged-app prefix:
/// `C:\...\Spotify.exe` → `spotify`, `SpotifyAB.SpotifyMusic_x!Spotify` →
/// `spotify`, `/Applications/zoom.us.app/.../zoom.us` → `zoom.us`.
pub(crate) fn normalize_app(name: &str) -> String {
    let name = name.trim().trim_matches('"');
    let name = name.rsplit(['/', '\\']).next().unwrap_or(name);
    let name = name.rsplit('!').next().unwrap_or(name);
    let lower = name.to_lowercase();
    lower.strip_suffix(".exe").or_else(|| lower.strip_suffix(".app")).unwrap_or(&lower).to_owned()
}

pub(crate) fn is_meeting_app(name: &str) -> bool {
    let name = normalize_app(name);
    MEETING_EXACT.contains(&name.as_str()) || MEETING_SUBSTRINGS.iter().any(|needle| name.contains(needle))
}

fn browser_family(name: &str) -> Option<&'static str> {
    BROWSER_EXACT
        .iter()
        .find(|(exact, _)| name == *exact)
        .or_else(|| BROWSER_PREFIXES.iter().find(|(prefix, _)| name.starts_with(prefix)))
        .map(|(_, family)| *family)
}

/// Groups the processes of one app: all Chrome processes are "chrome".
pub(crate) fn app_family(name: &str) -> String {
    let name = normalize_app(name);
    browser_family(&name).map(str::to_owned).unwrap_or(name)
}

/// What may be touched during this dictation.
#[derive(Clone, Debug)]
pub(crate) struct Policy {
    /// A call is likely: touch nothing at all.
    pub skip_all: bool,
    /// Some other process is capturing audio input (gates the macOS
    /// system-wide duck).
    #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
    pub foreign_capture: bool,
    /// App families capturing the microphone; never touched.
    excluded: Vec<String>,
    /// Platform stream ids already handled by another strategy (muffle).
    skip_ids: Vec<String>,
}

impl Policy {
    /// `capturers` are other processes currently capturing audio input;
    /// `None` means detection failed, which is treated as "maybe a call".
    /// An empty name means a process we could not identify.
    pub fn from_capturers(capturers: Option<&[String]>) -> Self {
        let Some(capturers) = capturers else {
            return Self { skip_all: true, foreign_capture: true, excluded: Vec::new(), skip_ids: Vec::new() };
        };
        let skip_all = capturers.iter().any(|name| name.trim().is_empty() || is_meeting_app(name));
        Self {
            skip_all,
            foreign_capture: !capturers.is_empty(),
            excluded: capturers.iter().map(|name| app_family(name)).collect(),
            skip_ids: Vec::new(),
        }
    }

    /// The same policy, also leaving ids alone.
    pub fn skipping(&self, ids: Vec<String>) -> Self {
        let mut policy = self.clone();
        policy.skip_ids.extend(ids);
        policy
    }

    #[cfg_attr(not(any(windows, target_os = "linux")), allow(dead_code))]
    pub fn skips(&self, id: &str) -> bool {
        self.skip_ids.iter().any(|skip| skip == id)
    }

    #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
    pub fn has_skips(&self) -> bool {
        !self.skip_ids.is_empty()
    }

    pub fn allows(&self, app: &str) -> bool {
        !self.skip_all && !app.trim().is_empty() && !is_meeting_app(app) && !self.excluded.contains(&app_family(app))
    }
}

// ---------------------------------------------------------------------------
// Crash journal
// ---------------------------------------------------------------------------

#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct JournalEntry {
    pub key: String,
    pub original: Vec<f32>,
    pub applied: Vec<f32>,
}

#[derive(Debug, Serialize, Deserialize)]
struct Journal {
    pid: u32,
    entries: Vec<JournalEntry>,
}

mod journal {
    use super::*;

    /// Per-user location: the runtime dir on Linux (`/tmp` is shared),
    /// the per-user temp dir elsewhere.
    fn path() -> PathBuf {
        let runtime = if cfg!(target_os = "linux") { std::env::var_os("XDG_RUNTIME_DIR").map(PathBuf::from) } else { None };
        runtime.unwrap_or_else(std::env::temp_dir).join("loquara-media-ducking.json")
    }

    pub(super) fn save(groups: &[&[Target]]) {
        let entries: Vec<JournalEntry> = groups
            .iter()
            .flat_map(|targets| targets.iter())
            .filter(|target| target.owned && !target.key.is_empty())
            .filter_map(|target| {
                Some(JournalEntry { key: target.key.clone(), original: target.original.clone(), applied: target.applied.clone()? })
            })
            .collect();
        if entries.is_empty() {
            return;
        }
        let journal = Journal { pid: std::process::id(), entries };
        if let Ok(text) = serde_json::to_string(&journal)
            && let Err(error) = std::fs::write(path(), text)
        {
            eprintln!("Loquara media: cannot write ducking journal: {error}");
        }
    }

    pub(super) fn clear() {
        let _ = std::fs::remove_file(path());
    }

    pub(super) fn recover() {
        let _serial = SERIAL.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        // Leftovers a crash can leave outside the journal (Linux null sink).
        if let Some(_thread) = platform::init_thread() {
            platform::recover_muffle();
        }
        let Ok(text) = std::fs::read_to_string(path()) else { return };
        let Ok(journal) = serde_json::from_str::<Journal>(&text) else {
            clear();
            return;
        };
        // Written by this very process: a live worker owns it.
        if journal.pid == std::process::id() {
            return;
        }
        if let Some(_thread) = platform::init_thread()
            && let Some((mut mixer, mut targets)) = platform::recovery_targets(&journal.entries)
        {
            // Jump straight back: there is nothing to glide from.
            restore(mixer.as_mut(), &mut targets, 1.0, 1);
        }
        clear();
    }
}

/// Builds recovery targets: `applied` comes from the journal, so restore
/// only fires for volumes still at the ducked value.
#[allow(dead_code)]
pub(crate) fn recovered_target(entry: &JournalEntry) -> Target {
    Target {
        key: entry.key.clone(),
        original: entry.original.clone(),
        applied: Some(entry.applied.clone()),
        owned: true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mode_parsing_is_lenient() {
        assert_eq!(MediaMode::parse("duck"), Some(MediaMode::Duck));
        assert_eq!(MediaMode::parse(" Pause "), Some(MediaMode::Pause));
        assert_eq!(MediaMode::parse("OFF"), Some(MediaMode::Off));
        assert_eq!(MediaMode::parse("loud"), None);
        assert_eq!(MediaMode::parse("muffle"), Some(MediaMode::Muffle));
        assert_eq!(MediaMode::from_setting("loud"), MediaMode::Muffle);
        assert_eq!(MediaMode::default(), MediaMode::Muffle);
        for mode in [MediaMode::Muffle, MediaMode::Duck, MediaMode::Pause, MediaMode::Off] {
            assert_eq!(MediaMode::parse(mode.as_str()), Some(mode));
        }
    }

    #[test]
    fn mode_serde_round_trips_and_tolerates_unknown_values() {
        assert_eq!(serde_json::to_string(&MediaMode::Pause).unwrap(), "\"pause\"");
        assert_eq!(serde_json::from_str::<MediaMode>("\"off\"").unwrap(), MediaMode::Off);
        assert_eq!(serde_json::from_str::<MediaMode>("\"duck\"").unwrap(), MediaMode::Duck);
        assert_eq!(serde_json::to_string(&MediaMode::Muffle).unwrap(), "\"muffle\"");
        assert_eq!(serde_json::from_str::<MediaMode>("\"whatever\"").unwrap(), MediaMode::Muffle);
    }

    #[test]
    fn off_mode_does_not_start_anything() {
        assert!(MediaController::default().begin(MediaMode::Off).is_none());
    }

    #[test]
    fn normalizes_paths_and_app_ids() {
        assert_eq!(normalize_app(r"C:\Users\me\AppData\Roaming\Spotify\Spotify.exe"), "spotify");
        assert_eq!(normalize_app("SpotifyAB.SpotifyMusic_zpdnekdrzrea0!Spotify"), "spotify");
        assert_eq!(normalize_app("/Applications/zoom.us.app/Contents/MacOS/zoom.us"), "zoom.us");
        assert_eq!(normalize_app("Chrome"), "chrome");
    }

    #[test]
    fn classifies_meeting_apps() {
        for name in [
            r"C:\Program Files\WindowsApps\MSTeams_1\ms-teams.exe",
            "MSTeams_8wekyb3d8bbwe!MSTeams",
            "Teams.exe",
            "Zoom.exe",
            "zoom.us",
            "CptHost",
            "slack",
            "Discord.exe",
            "CiscoCollabHost.exe",
            "atmgr.exe",
            "Skype.exe",
            "FaceTime",
            "msedgewebview2.exe",
            "signal",
            "WhatsApp.exe",
            "PhoneExperienceHost.exe",
        ] {
            assert!(is_meeting_app(name), "{name} should be a meeting app");
        }
        for name in ["Spotify.exe", "vlc", "chrome.exe", "firefox", "Music", "foobar2000.exe", "rhythmbox", "popcorn"] {
            assert!(!is_meeting_app(name), "{name} should not be a meeting app");
        }
    }

    #[test]
    fn groups_browser_processes() {
        assert_eq!(app_family("chrome.exe"), "chrome");
        assert_eq!(app_family("Chrome"), "chrome");
        assert_eq!(app_family("chromium-browser"), "chrome");
        assert_eq!(app_family("MSEdge"), "edge");
        assert_eq!(app_family("308046B0AF4A39CB"), "firefox");
        assert_eq!(app_family("firefox.instance_1_23"), "firefox");
        assert_eq!(app_family("Spotify.exe"), "spotify");
    }

    #[test]
    fn meeting_capture_stands_everything_down() {
        let policy = Policy::from_capturers(Some(&["Zoom.exe".to_owned()]));
        assert!(policy.skip_all);
        assert!(!policy.allows("Spotify.exe"));
    }

    #[test]
    fn browser_capture_spares_only_that_browser() {
        let policy = Policy::from_capturers(Some(&[r"C:\Program Files\Google\Chrome\Application\chrome.exe".to_owned()]));
        assert!(!policy.skip_all);
        assert!(policy.foreign_capture);
        assert!(!policy.allows("chrome.exe"));
        assert!(!policy.allows("Chrome"));
        assert!(policy.allows("firefox.exe"));
        assert!(policy.allows("Spotify.exe"));
    }

    #[test]
    fn meeting_apps_and_unknowns_are_never_touched() {
        let policy = Policy::from_capturers(Some(&[]));
        assert!(!policy.skip_all);
        assert!(!policy.foreign_capture);
        assert!(policy.allows("vlc.exe"));
        assert!(!policy.allows("Teams.exe"));
        assert!(!policy.allows("discord"));
        assert!(!policy.allows(""));
    }

    #[test]
    fn failed_or_unidentified_detection_skips_everything() {
        assert!(Policy::from_capturers(None).skip_all);
        assert!(Policy::from_capturers(Some(&[String::new()])).skip_all);
    }

    #[test]
    fn restores_only_untouched_volumes() {
        assert!(still_ours(Some(&[0.22]), Some(&[0.22])));
        assert!(still_ours(Some(&[0.215]), Some(&[0.22])));
        assert!(still_ours(Some(&[0.2, 0.3]), Some(&[0.2, 0.3])));
        // A near-silent muffle level is not mistaken for the user's zero.
        assert!(still_ours(Some(&[0.008]), Some(&[0.008])));
        assert!(!still_ours(Some(&[0.0]), Some(&[0.008])));
        // The user moved the slider meanwhile.
        assert!(!still_ours(Some(&[0.5]), Some(&[0.22])));
        assert!(!still_ours(Some(&[0.05]), Some(&[0.22])));
        // Channel layout changed, the stream vanished, or we never wrote.
        assert!(!still_ours(Some(&[0.22]), Some(&[0.22, 0.22])));
        assert!(!still_ours(None, Some(&[0.22])));
        assert!(!still_ours(Some(&[0.22]), None));
    }

    /// In-memory mixer for the fade/restore flow.
    struct FakeMixer {
        volumes: Vec<Vec<f32>>,
        writes: usize,
    }

    impl Mixer for FakeMixer {
        fn read(&mut self, _targets: &[Target]) -> Vec<Option<Vec<f32>>> {
            self.volumes.iter().cloned().map(Some).collect()
        }
        fn write(&mut self, index: usize, volume: &[f32]) -> Option<Vec<f32>> {
            self.writes += 1;
            self.volumes[index] = volume.to_vec();
            Some(volume.to_vec())
        }
        fn steps(&self) -> u32 {
            4
        }
    }

    #[test]
    fn ducks_then_restores_exact_original() {
        let mut mixer = FakeMixer { volumes: vec![vec![0.8, 0.6]], writes: 0 };
        let mut targets = vec![Target::new("a".into(), vec![0.8, 0.6])];
        let reached = fade(&mut mixer, &mut targets, 1.0, 0.25, Duration::ZERO, 4, None);
        assert_eq!(reached, 0.25);
        assert!(still_ours(Some(&mixer.volumes[0]), Some(&[0.2, 0.15])));
        restore(&mut mixer, &mut targets, reached, 4);
        assert_eq!(mixer.volumes[0], vec![0.8, 0.6]);
    }

    #[test]
    fn never_raises_a_volume_the_user_changed() {
        let mut mixer = FakeMixer { volumes: vec![vec![1.0], vec![0.5]], writes: 0 };
        let mut targets = vec![Target::new("a".into(), vec![1.0]), Target::new("b".into(), vec![0.5])];
        fade(&mut mixer, &mut targets, 1.0, 0.2, Duration::ZERO, 4, None);
        mixer.volumes[0] = vec![0.05];
        restore(&mut mixer, &mut targets, 0.2, 4);
        assert_eq!(mixer.volumes[0], vec![0.05]);
        assert!((mixer.volumes[1][0] - 0.5).abs() < f32::EPSILON);
    }

    #[test]
    fn recovery_restores_only_still_ducked_entries() {
        let entries = [
            JournalEntry { key: "a".into(), original: vec![0.9], applied: vec![0.2] },
            JournalEntry { key: "b".into(), original: vec![0.9], applied: vec![0.2] },
        ];
        let mut targets: Vec<Target> = entries.iter().map(recovered_target).collect();
        let mut mixer = FakeMixer { volumes: vec![vec![0.2], vec![0.7]], writes: 0 };
        restore(&mut mixer, &mut targets, 1.0, 1);
        assert_eq!(mixer.volumes, vec![vec![0.9], vec![0.7]]);
        assert_eq!(mixer.writes, 1);
    }

    #[test]
    fn early_stop_cuts_the_fade_short() {
        let (stop_tx, signal) = mpsc::channel();
        let mut stop = Stop { signal, fired: false };
        stop_tx.send(()).unwrap();
        let mut mixer = FakeMixer { volumes: vec![vec![1.0]], writes: 0 };
        let mut targets = vec![Target::new("a".into(), vec![1.0])];
        let reached = fade(&mut mixer, &mut targets, 1.0, 0.2, Duration::from_millis(400), 4, Some(&mut stop));
        assert!(reached > 0.2);
        assert_eq!(mixer.writes, 1);
    }

    #[test]
    fn dropped_guard_counts_as_ended() {
        let (stop_tx, signal) = mpsc::channel::<()>();
        let mut stop = Stop { signal, fired: false };
        assert!(!stop.fired());
        drop(stop_tx);
        assert!(stop.fired());
    }
}
