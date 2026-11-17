//! Linux: per-stream volume through `pactl` (PulseAudio or PipeWire-Pulse)
//! and pause/resume through MPRIS via `dbus-send`.

use super::command::run;
use super::pactl::{self, Stream};
use super::{JournalEntry, Mixer, Policy, Target, recovered_target};
use std::time::Duration;

const TIMEOUT: Duration = Duration::from_secs(2);

pub(crate) struct ThreadGuard;

pub(crate) fn init_thread() -> Option<ThreadGuard> {
    Some(ThreadGuard)
}

fn pactl_list(kind: &str, header: &str) -> Option<Vec<Stream>> {
    run("pactl", &["list", kind], TIMEOUT).map(|text| pactl::parse_streams(&text, header))
}

fn sink_inputs() -> Option<Vec<Stream>> {
    pactl_list("sink-inputs", "Sink Input #")
}

pub(crate) fn foreign_capturers() -> Option<Vec<String>> {
    let own = std::process::id();
    let outputs = pactl_list("source-outputs", "Source Output #")?;
    Some(
        outputs
            .iter()
            .filter(|stream| !stream.corked && !stream.is_level_meter() && stream.pid() != Some(own) && !is_ours(stream))
            .map(Stream::app)
            .collect(),
    )
}

/// Policy id of one sink input (see [Policy::skips]).
fn stream_id(index: u32) -> String {
    format!("sink-input:{index}")
}

/// Our own muffle helpers (`parec`/`pacat`) run as separate processes.
fn is_ours(stream: &Stream) -> bool {
    stream.properties.get("application.name").is_some_and(|name| name.starts_with(CLIENT_NAME))
}

struct PactlMixer {
    indices: Vec<u32>,
}

impl Mixer for PactlMixer {
    fn read(&mut self, _targets: &[Target]) -> Vec<Option<Vec<f32>>> {
        let streams = sink_inputs().unwrap_or_default();
        self.indices
            .iter()
            .map(|index| {
                streams.iter().find(|stream| stream.index == *index).map(|stream| pactl::to_normalized(&stream.volumes))
            })
            .collect()
    }

    fn write(&mut self, index: usize, volume: &[f32]) -> Option<Vec<f32>> {
        let stream = self.indices.get(index)?.to_string();
        let raw_volume = pactl::to_raw(volume);
        let raw: Vec<String> = raw_volume.iter().map(u32::to_string).collect();
        let mut args = vec!["set-sink-input-volume", stream.as_str()];
        args.extend(raw.iter().map(String::as_str));
        run("pactl", &args, TIMEOUT)?;
        Some(pactl::to_normalized(&raw_volume))
    }

    fn steps(&self) -> u32 {
        6
    }
}

pub(crate) fn duck_targets(policy: &Policy) -> Option<(Box<dyn Mixer>, Vec<Target>)> {
    let own = std::process::id();
    let mut indices = Vec::new();
    let mut targets = Vec::new();
    for stream in sink_inputs()? {
        let app = stream.app();
        let audible = stream.volumes.iter().any(|volume| (*volume as f32) > pactl::VOLUME_NORM * 0.01);
        if stream.corked || stream.muted || !audible || stream.pid() == Some(own) || is_ours(&stream) || !policy.allows(&app) || policy.skips(&stream_id(stream.index)) {
            continue;
        }
        indices.push(stream.index);
        targets.push(Target::new(app, pactl::to_normalized(&stream.volumes)));
    }
    Some((Box::new(PactlMixer { indices }), targets))
}

/// Streams are matched by app: indices do not survive an app restart, and
/// stream-restore may have re-applied the ducked volume to the new stream.
pub(crate) fn recovery_targets(entries: &[JournalEntry]) -> Option<(Box<dyn Mixer>, Vec<Target>)> {
    let mut indices = Vec::new();
    let mut targets = Vec::new();
    for stream in sink_inputs()? {
        if let Some(entry) = entries.iter().find(|entry| entry.key == stream.app()) {
            indices.push(stream.index);
            targets.push(recovered_target(entry));
        }
    }
    Some((Box::new(PactlMixer { indices }), targets))
}

fn dbus(args: &[&str]) -> Option<String> {
    let mut full = vec!["--session", "--print-reply"];
    full.extend_from_slice(args);
    run("dbus-send", &full, TIMEOUT)
}

fn players() -> Vec<String> {
    dbus(&["--dest=org.freedesktop.DBus", "/org/freedesktop/DBus", "org.freedesktop.DBus.ListNames"])
        .map(|text| pactl::parse_mpris_names(&text))
        .unwrap_or_default()
}

fn status(player: &str) -> Option<String> {
    let dest = format!("--dest={player}");
    let reply = dbus(&[
        dest.as_str(),
        "/org/mpris/MediaPlayer2",
        "org.freedesktop.DBus.Properties.Get",
        "string:org.mpris.MediaPlayer2.Player",
        "string:PlaybackStatus",
    ])?;
    pactl::parse_playback_status(&reply)
}

fn command(player: &str, method: &str) -> bool {
    let dest = format!("--dest={player}");
    let method = format!("org.mpris.MediaPlayer2.Player.{method}");
    dbus(&[dest.as_str(), "/org/mpris/MediaPlayer2", method.as_str()]).is_some()
}

pub(crate) fn pause(policy: &Policy) -> Vec<String> {
    players()
        .into_iter()
        .filter(|player| policy.allows(&pactl::mpris_app(player)))
        .filter(|player| status(player).as_deref() == Some("Playing"))
        .filter(|player| command(player, "Pause"))
        .collect()
}

pub(crate) fn resume(paused: Vec<String>) {
    for player in paused {
        // Still paused means nobody touched it; anything else is the user's.
        if status(&player).as_deref() == Some("Paused") {
            command(&player, "Play");
        }
    }
}

// ---------------------------------------------------------------------------
// Muffle ("music in the next room")
// ---------------------------------------------------------------------------
//
// Muffled streams are moved onto a private null sink. `parec` records that
// sink's monitor, the samples run through the shared DSP, and `pacat` plays
// them on the default sink. Ending opens the filter, moves every stream
// that is still parked with us back where it was, and unloads the sink.

use super::dsp::Muffler;
use super::{Muffle, Stop};
use std::io::{Read, Write};
use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};

const CLIENT_NAME: &str = "loquara-muffle";
const RATE: u32 = 48_000;
const CHANNELS: usize = 2;
/// 10 ms per hop through the pipe.
const CHUNK_FRAMES: usize = RATE as usize / 100;
const MUFFLE_RAMP: Duration = Duration::from_millis(300);

fn default_sink() -> Option<String> {
    run("pactl", &["get-default-sink"], TIMEOUT)
        .map(|text| text.trim().to_owned())
        .filter(|name| !name.is_empty())
        .or_else(|| run("pactl", &["info"], TIMEOUT).and_then(|text| pactl::parse_default_sink(&text)))
}

fn sink_index(name: &str) -> Option<u32> {
    let text = run("pactl", &["list", "short", "sinks"], TIMEOUT)?;
    pactl::parse_short_sinks(&text).into_iter().find(|(_, sink)| sink == name).map(|(index, _)| index)
}

fn move_stream(stream: u32, sink: &str) -> bool {
    run("pactl", &["move-sink-input", &stream.to_string(), sink], TIMEOUT).is_some()
}

fn unload(module: u32) {
    let _ = run("pactl", &["unload-module", &module.to_string()], TIMEOUT);
}

/// Kills the helper processes when dropped.
struct Helpers {
    recorder: Child,
    player: Child,
}

impl Drop for Helpers {
    fn drop(&mut self) {
        for child in [&mut self.recorder, &mut self.player] {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

fn spawn_helpers(output: &str) -> Option<Helpers> {
    let format = ["--format=float32le", "--rate=48000", "--channels=2", "--raw"];
    let recorder = Command::new("parec")
        .arg(format!("--device={}.monitor", pactl::MUFFLE_SINK))
        .args(format)
        .args(["--latency-msec=10", &format!("--client-name={CLIENT_NAME}-in")])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let player = Command::new("pacat")
        .arg("--playback")
        .arg(format!("--device={output}"))
        .args(format)
        .args(["--latency-msec=20", &format!("--client-name={CLIENT_NAME}-out")])
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn();
    let player = match player {
        Ok(player) => player,
        Err(_) => {
            let mut recorder = recorder;
            let _ = recorder.kill();
            let _ = recorder.wait();
            return None;
        }
    };
    Some(Helpers { recorder, player })
}

struct Controls {
    stop: AtomicBool,
    amount: AtomicU32,
    ramp_ms: AtomicU32,
    generation: AtomicU32,
}

/// Moves audio from the recorder through the filter to the player until
/// stopped or either helper goes away.
fn pump(mut input: impl Read, mut output: impl Write, controls: Arc<Controls>) {
    let mut muffler = Muffler::new(RATE, CHANNELS);
    let mut generation = 0;
    let mut bytes = vec![0u8; CHUNK_FRAMES * CHANNELS * 4];
    let mut samples = vec![0f32; CHUNK_FRAMES * CHANNELS];
    while !controls.stop.load(Ordering::SeqCst) {
        if input.read_exact(&mut bytes).is_err() {
            break;
        }
        let current = controls.generation.load(Ordering::SeqCst);
        if current != generation {
            generation = current;
            let amount = f32::from_bits(controls.amount.load(Ordering::SeqCst));
            muffler.set_target(amount, Duration::from_millis(u64::from(controls.ramp_ms.load(Ordering::SeqCst))));
        }
        for (sample, raw) in samples.iter_mut().zip(bytes.chunks_exact(4)) {
            *sample = f32::from_le_bytes([raw[0], raw[1], raw[2], raw[3]]);
        }
        muffler.process(&mut samples);
        for (raw, sample) in bytes.chunks_exact_mut(4).zip(&samples) {
            raw.copy_from_slice(&sample.to_le_bytes());
        }
        if output.write_all(&bytes).is_err() {
            break;
        }
    }
}

struct Parked {
    stream: u32,
    /// Sink name it came from.
    home: String,
}

struct Muffled {
    module: u32,
    sink: u32,
    output: String,
    parked: Vec<Parked>,
    controls: Arc<Controls>,
    pump: Option<std::thread::JoinHandle<()>>,
    helpers: Option<Helpers>,
    policy: Policy,
    known: Vec<u32>,
}

impl Muffled {
    fn set_amount(&self, amount: f32, ramp: Duration) {
        self.controls.amount.store(amount.to_bits(), Ordering::SeqCst);
        self.controls.ramp_ms.store(ramp.as_millis() as u32, Ordering::SeqCst);
        self.controls.generation.fetch_add(1, Ordering::SeqCst);
    }

    fn eligible(&self, stream: &Stream) -> bool {
        let own = std::process::id();
        let audible = stream.volumes.iter().any(|volume| (*volume as f32) > pactl::VOLUME_NORM * 0.01);
        !stream.corked
            && !stream.muted
            && audible
            && stream.pid() != Some(own)
            && !is_ours(stream)
            && stream.sink != Some(self.sink)
            && self.policy.allows(&stream.app())
    }

    /// Parks every eligible stream not seen before.
    fn park_new(&mut self, streams: &[Stream]) {
        let names = run("pactl", &["list", "short", "sinks"], TIMEOUT).map(|text| pactl::parse_short_sinks(&text)).unwrap_or_default();
        for stream in streams {
            if self.known.contains(&stream.index) || !self.eligible(stream) {
                continue;
            }
            self.known.push(stream.index);
            let Some(home) = stream.sink.and_then(|sink| names.iter().find(|(index, _)| *index == sink)).map(|(_, name)| name.clone()) else {
                continue;
            };
            if move_stream(stream.index, pactl::MUFFLE_SINK) {
                self.parked.push(Parked { stream: stream.index, home });
            }
        }
    }
}

pub(crate) fn muffle_begin(policy: &Policy, stop: &mut Stop) -> Option<Box<dyn Muffle>> {
    let output = default_sink().filter(|sink| sink != pactl::MUFFLE_SINK)?;
    let streams = sink_inputs()?;
    let candidates = streams.iter().filter(|stream| {
        let audible = stream.volumes.iter().any(|volume| (*volume as f32) > pactl::VOLUME_NORM * 0.01);
        !stream.corked && !stream.muted && audible && stream.pid() != Some(std::process::id()) && !is_ours(stream) && policy.allows(&stream.app())
    });
    if candidates.count() == 0 {
        return None;
    }
    let loaded = run(
        "pactl",
        &["load-module", "module-null-sink", &format!("sink_name={}", pactl::MUFFLE_SINK), "sink_properties=device.description=Loquara"],
        TIMEOUT,
    )?;
    let module: u32 = loaded.trim().parse().ok()?;
    let Some(sink) = sink_index(pactl::MUFFLE_SINK) else {
        unload(module);
        return None;
    };
    let Some(mut helpers) = spawn_helpers(&output) else {
        unload(module);
        return None;
    };
    let controls = Arc::new(Controls { stop: AtomicBool::new(false), amount: AtomicU32::new(0), ramp_ms: AtomicU32::new(0), generation: AtomicU32::new(0) });
    let (Some(input), Some(output_pipe)) = (helpers.recorder.stdout.take(), helpers.player.stdin.take()) else {
        unload(module);
        return None;
    };
    let pump_controls = controls.clone();
    let pump = std::thread::Builder::new().name("loquara-muffle".into()).spawn(move || pump(input, output_pipe, pump_controls)).ok();
    let mut muffled = Muffled {
        module,
        sink,
        output,
        parked: Vec::new(),
        controls,
        pump,
        helpers: Some(helpers),
        policy: policy.clone(),
        known: Vec::new(),
    };
    // Give the helpers a moment to connect so the first moved audio has
    // somewhere to go.
    let _ = stop.wait_for(Duration::from_millis(60));
    muffled.park_new(&streams);
    if muffled.parked.is_empty() {
        Box::new(muffled).end();
        return None;
    }
    muffled.set_amount(1.0, MUFFLE_RAMP);
    Some(Box::new(muffled))
}

impl Muffle for Muffled {
    fn claimed(&self) -> Vec<String> {
        self.parked.iter().map(|parked| stream_id(parked.stream)).collect()
    }

    fn targets(&self) -> &[Target] {
        &[]
    }

    fn tick(&mut self) {
        if let Some(streams) = sink_inputs() {
            self.park_new(&streams);
        }
    }

    fn end(self: Box<Self>) {
        let mut this = *self;
        this.set_amount(0.0, MUFFLE_RAMP);
        std::thread::sleep(MUFFLE_RAMP + Duration::from_millis(40));
        let streams = sink_inputs().unwrap_or_default();
        for parked in &this.parked {
            // Only streams still parked with us: one the user moved is theirs.
            let still_parked = streams.iter().any(|stream| stream.index == parked.stream && stream.sink == Some(this.sink));
            if still_parked && !move_stream(parked.stream, &parked.home) {
                let _ = move_stream(parked.stream, &this.output);
            }
        }
        this.controls.stop.store(true, Ordering::SeqCst);
        drop(this.helpers.take());
        if let Some(pump) = this.pump.take() {
            let _ = pump.join();
        }
        unload(this.module);
    }
}

/// After a crash: send anything left on our null sink to the default sink
/// and unload it.
pub(crate) fn recover_muffle() {
    let Some(text) = run("pactl", &["list", "short", "modules"], TIMEOUT) else { return };
    let modules = pactl::parse_muffle_modules(&text);
    if modules.is_empty() {
        return;
    }
    if let (Some(sink), Some(output)) = (sink_index(pactl::MUFFLE_SINK), default_sink().filter(|sink| sink != pactl::MUFFLE_SINK)) {
        for stream in sink_inputs().unwrap_or_default() {
            if stream.sink == Some(sink) {
                let _ = move_stream(stream.index, &output);
            }
        }
    }
    for module in modules {
        unload(module);
    }
}
