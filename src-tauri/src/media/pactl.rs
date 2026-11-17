//! Parsers for `pactl list sink-inputs|source-outputs` (run with `LC_ALL=C`)
//! and `dbus-send` MPRIS replies. Pure text handling, compiled everywhere so
//! the tests run on every CI platform.
#![cfg_attr(not(target_os = "linux"), allow(dead_code))]

use std::collections::HashMap;

/// PulseAudio's 100 % volume.
pub(crate) const VOLUME_NORM: f32 = 65536.0;
pub(crate) const MPRIS_PREFIX: &str = "org.mpris.MediaPlayer2.";

#[derive(Debug, Default, PartialEq)]
pub(crate) struct Stream {
    pub index: u32,
    pub corked: bool,
    pub muted: bool,
    /// Sink (for sink inputs) the stream plays to.
    pub sink: Option<u32>,
    /// Raw per-channel volumes (65536 = 100 %).
    pub volumes: Vec<u32>,
    pub properties: HashMap<String, String>,
}

impl Stream {
    pub fn pid(&self) -> Option<u32> {
        self.properties.get("application.process.id")?.parse().ok()
    }

    /// Process binary when known, otherwise the client's declared name.
    pub fn app(&self) -> String {
        ["application.process.binary", "application.name"]
            .iter()
            .filter_map(|key| self.properties.get(*key))
            .find(|value| !value.trim().is_empty())
            .cloned()
            .unwrap_or_default()
    }

    /// Volume meters (pavucontrol, desktop applets) open capture streams on
    /// every source; they are not someone talking.
    pub fn is_level_meter(&self) -> bool {
        let property = |key: &str| self.properties.get(key).map(|value| value.to_lowercase()).unwrap_or_default();
        property("media.name").contains("peak detect")
            || property("application.id").contains("pavucontrol")
            || matches!(property("application.process.binary").as_str(), "pavucontrol" | "pavucontrol-qt")
    }
}

/// Parses blocks that start with `header` followed by the index, e.g.
/// `Sink Input #42` or `Source Output #7`.
pub(crate) fn parse_streams(text: &str, header: &str) -> Vec<Stream> {
    let mut streams = Vec::new();
    let mut current: Option<Stream> = None;
    for line in text.lines() {
        let trimmed = line.trim();
        if let Some(index) = trimmed.strip_prefix(header) {
            if let Some(stream) = current.take() {
                streams.push(stream);
            }
            current = index.trim().parse().ok().map(|index| Stream { index, ..Stream::default() });
            continue;
        }
        let Some(stream) = current.as_mut() else { continue };
        if let Some(value) = trimmed.strip_prefix("Corked:") {
            stream.corked = value.trim() == "yes";
        } else if let Some(value) = trimmed.strip_prefix("Mute:") {
            stream.muted = value.trim() == "yes";
        } else if let Some(value) = trimmed.strip_prefix("Sink:") {
            stream.sink = value.trim().parse().ok();
        } else if let Some(value) = trimmed.strip_prefix("Volume:") {
            stream.volumes = parse_volumes(value);
        } else if let Some((key, value)) = trimmed.split_once(" = ") {
            stream.properties.insert(key.trim().to_owned(), value.trim().trim_matches('"').to_owned());
        }
    }
    streams.extend(current);
    streams
}

/// `front-left: 65536 / 100% / 0.00 dB,   front-right: 32768 / 50% / -18.06 dB`
fn parse_volumes(value: &str) -> Vec<u32> {
    value
        .split(',')
        .filter_map(|channel| channel.split_once(':')?.1.split('/').next()?.trim().parse().ok())
        .collect()
}

pub(crate) fn to_normalized(raw: &[u32]) -> Vec<f32> {
    raw.iter().map(|value| *value as f32 / VOLUME_NORM).collect()
}

pub(crate) fn to_raw(volume: &[f32]) -> Vec<u32> {
    volume.iter().map(|value| (value.max(0.0) * VOLUME_NORM).round() as u32).collect()
}

/// Name of the null sink muffled streams are parked on.
pub(crate) const MUFFLE_SINK: &str = "loquara_muffle";

/// `pactl list short sinks`: (index, name) pairs.
pub(crate) fn parse_short_sinks(text: &str) -> Vec<(u32, String)> {
    text.lines()
        .filter_map(|line| {
            let mut fields = line.split('\t');
            Some((fields.next()?.trim().parse().ok()?, fields.next()?.trim().to_owned()))
        })
        .collect()
}

/// `pactl list short modules`: indices of null sinks we loaded.
pub(crate) fn parse_muffle_modules(text: &str) -> Vec<u32> {
    text.lines()
        .filter_map(|line| {
            let mut fields = line.split('\t');
            let index = fields.next()?.trim().parse().ok()?;
            let name = fields.next()?.trim();
            let arguments = fields.next().unwrap_or_default();
            let ours = name == "module-null-sink" && arguments.split_whitespace().any(|argument| argument == format!("sink_name={MUFFLE_SINK}"));
            ours.then_some(index)
        })
        .collect()
}

/// `pactl info`: the `Default Sink:` line.
pub(crate) fn parse_default_sink(text: &str) -> Option<String> {
    text.lines().find_map(|line| Some(line.trim().strip_prefix("Default Sink:")?.trim().to_owned())).filter(|name| !name.is_empty())
}

/// Bus names from `org.freedesktop.DBus.ListNames` that are MPRIS players.
pub(crate) fn parse_mpris_names(text: &str) -> Vec<String> {
    text.lines()
        .filter_map(|line| line.trim().strip_prefix("string \"")?.strip_suffix('"'))
        .filter(|name| name.starts_with(MPRIS_PREFIX))
        .map(str::to_owned)
        .collect()
}

/// `PlaybackStatus` from a `Properties.Get` reply: `variant string "Playing"`.
pub(crate) fn parse_playback_status(text: &str) -> Option<String> {
    let start = text.find("string \"")? + "string \"".len();
    let rest = &text[start..];
    Some(rest[..rest.find('"')?].to_owned())
}

/// `org.mpris.MediaPlayer2.firefox.instance_1_23` → `firefox`.
pub(crate) fn mpris_app(bus_name: &str) -> String {
    let player = bus_name.strip_prefix(MPRIS_PREFIX).unwrap_or(bus_name);
    player.split('.').next().unwrap_or(player).to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    const SINK_INPUTS: &str = r#"Sink Input #42
	Driver: protocol-native.c
	Owner Module: 10
	Client: 61
	Sink: 1
	Sample Specification: float32le 2ch 44100Hz
	Channel Map: front-left,front-right
	Format: pcm, format.sample_format = "\"float32le\""  format.rate = "44100"  format.channels = "2"  format.channel_map = "\"front-left,front-right\""
	Corked: no
	Mute: no
	Volume: front-left: 65536 / 100% / 0.00 dB,   front-right: 32768 /  50% / -18.06 dB
	        balance -0.50
	Buffer Latency: 69659 usec
	Sink Latency: 23000 usec
	Resample method: n/a
	Properties:
		media.name = "Spotify"
		application.name = "Spotify"
		native-protocol.peer = "UNIX socket client"
		application.process.id = "4242"
		application.process.binary = "spotify"
		module-stream-restore.id = "sink-input-by-application-name:Spotify"

Sink Input #43
	Driver: protocol-native.c
	Corked: yes
	Mute: no
	Volume: mono: 45875 /  70% / -9.29 dB
	        balance 0.00
	Properties:
		application.name = "Firefox"
		application.process.id = "5000"
"#;

    const SOURCE_OUTPUTS: &str = r#"Source Output #7
	Driver: PipeWire
	Corked: no
	Mute: no
	Volume: mono: 65536 / 100% / 0.00 dB
	Properties:
		media.name = "Peak detect"
		application.id = "org.PulseAudio.pavucontrol"
		application.process.id = "900"
Source Output #8
	Corked: no
	Properties:
		application.name = "ZOOM VoiceEngine"
		application.process.binary = "zoom"
		application.process.id = "1200"
"#;

    #[test]
    fn parses_sink_inputs() {
        let streams = parse_streams(SINK_INPUTS, "Sink Input #");
        assert_eq!(streams.len(), 2);
        let spotify = &streams[0];
        assert_eq!(spotify.index, 42);
        assert!(!spotify.corked && !spotify.muted);
        assert_eq!(spotify.volumes, vec![65536, 32768]);
        assert_eq!(spotify.pid(), Some(4242));
        assert_eq!(spotify.app(), "spotify");
        assert_eq!(spotify.sink, Some(1));
        let firefox = &streams[1];
        assert_eq!(firefox.index, 43);
        assert!(firefox.corked);
        assert_eq!(firefox.volumes, vec![45875]);
        assert_eq!(firefox.app(), "Firefox");
    }

    #[test]
    fn parses_source_outputs_and_spots_level_meters() {
        let streams = parse_streams(SOURCE_OUTPUTS, "Source Output #");
        assert_eq!(streams.len(), 2);
        assert!(streams[0].is_level_meter());
        assert!(!streams[1].is_level_meter());
        assert_eq!(streams[1].app(), "zoom");
        assert_eq!(streams[1].pid(), Some(1200));
    }

    #[test]
    fn parses_short_lists_and_finds_our_null_sinks() {
        let sinks = "0\talsa_output.pci-0000_00_1f.3.analog-stereo\tPipeWire\tfloat32le 2ch 48000Hz\tRUNNING\n57\tloquara_muffle\tPipeWire\tfloat32le 2ch 48000Hz\tIDLE\n";
        assert_eq!(parse_short_sinks(sinks), vec![(0, "alsa_output.pci-0000_00_1f.3.analog-stereo".to_owned()), (57, MUFFLE_SINK.to_owned())]);
        let modules = "6\tmodule-null-sink\tsink_name=other\t
536870913\tmodule-null-sink\tsink_name=loquara_muffle sink_properties=device.description=Loquara\t
12\tmodule-alsa-card\tdevice_id=0\t";
        assert_eq!(parse_muffle_modules(modules), vec![536870913]);
        let info = "Server Name: PulseAudio (on PipeWire 1.0.5)\nDefault Sink: alsa_output.usb-headset\nDefault Source: alsa_input.usb";
        assert_eq!(parse_default_sink(info).as_deref(), Some("alsa_output.usb-headset"));
        assert_eq!(parse_default_sink("Default Sink: "), None);
    }

    #[test]
    fn empty_output_has_no_streams() {
        assert!(parse_streams("", "Sink Input #").is_empty());
    }

    #[test]
    fn volume_conversion_round_trips() {
        let raw = vec![65536, 32768, 0];
        assert_eq!(to_raw(&to_normalized(&raw)), raw);
        assert_eq!(to_raw(&[0.22]), vec![14418]);
    }

    #[test]
    fn parses_mpris_replies() {
        let names = r#"method return time=1 sender=org.freedesktop.DBus -> destination=:1.9 serial=3 reply_serial=2
   array [
      string "org.freedesktop.DBus"
      string ":1.7"
      string "org.mpris.MediaPlayer2.spotify"
      string "org.mpris.MediaPlayer2.firefox.instance_1_84"
   ]"#;
        assert_eq!(
            parse_mpris_names(names),
            vec!["org.mpris.MediaPlayer2.spotify", "org.mpris.MediaPlayer2.firefox.instance_1_84"]
        );
        let status = "method return time=1 sender=:1.50 -> destination=:1.9 serial=9 reply_serial=2\n   variant       string \"Playing\"\n";
        assert_eq!(parse_playback_status(status).as_deref(), Some("Playing"));
        assert_eq!(parse_playback_status("Error org.freedesktop.DBus.Error.ServiceUnknown"), None);
        assert_eq!(mpris_app("org.mpris.MediaPlayer2.firefox.instance_1_84"), "firefox");
        assert_eq!(mpris_app("org.mpris.MediaPlayer2.vlc"), "vlc");
    }
}
