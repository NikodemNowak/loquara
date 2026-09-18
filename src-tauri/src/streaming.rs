//! Live transcription over a provider's WebSocket.
//!
//! xAI sends locked pieces of the transcript while the audio is still
//! arriving — roughly every few seconds of speech. Loquara types those pieces
//! into whatever window has focus as they land, so dictating reads like
//! speaking into the application rather than recording first and waiting
//! afterwards.
//!
//! The recording is still written to disk the whole time. Live transcription
//! is best effort: if the socket never connects, drops, or the text cannot be
//! typed, the finished WAV is still there and the ordinary request path takes
//! over.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use rubato::{
    Resampler, SincFixedIn, SincInterpolationParameters, SincInterpolationType, WindowFunction,
};
use serde_json::Value;
use tokio::sync::mpsc;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::http::HeaderValue;
use tokio_tungstenite::connect_async;

/// Every provider's streaming mode wants raw PCM at this rate.
pub const TARGET_SAMPLE_RATE: u32 = 16_000;

/// How long the socket has to open, and to say it is ready.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const CREATED_TIMEOUT: Duration = Duration::from_secs(10);
/// How long the last words have to arrive after the take ends.
const FLUSH_TIMEOUT: Duration = Duration::from_secs(6);

const FINALIZE: &str = r#"{"type":"finalize"}"#;
const AUDIO_DONE: &str = r#"{"type":"audio.done"}"#;

/// The xAI endpoint that transcribes while the user speaks.
pub fn xai_stream_url(
    base_url: &str,
    model: &str,
    language: &str,
    keyterms: &[String],
) -> String {
    use crate::cloud::{keyterms as trim_keyterms, url_escape};

    let base = if base_url.trim().is_empty() {
        "https://api.x.ai/v1"
    } else {
        base_url.trim().trim_end_matches('/')
    };
    let base = match base.split_once("://") {
        Some((scheme, rest)) if scheme.eq_ignore_ascii_case("https") => format!("wss://{rest}"),
        Some((scheme, rest)) if scheme.eq_ignore_ascii_case("http") => format!("ws://{rest}"),
        _ => base.to_owned(),
    };
    let base = if base.ends_with("/stt") {
        base
    } else {
        format!("{base}/stt")
    };
    let mut url = format!(
        "{base}?model={}&sample_rate={TARGET_SAMPLE_RATE}&encoding=pcm",
        url_escape(model)
    );
    if !language.trim().is_empty() {
        url.push_str(&format!("&language={}", url_escape(language.trim())));
    }
    for term in trim_keyterms(keyterms) {
        url.push_str(&format!("&keyterm={}", url_escape(&term)));
    }
    url
}

/// What one provider event means to Loquara.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    /// The server is ready for audio.
    Created,
    /// A piece of transcript. `is_final` marks text that will not change
    /// anymore; `speech_final` marks the end of an utterance.
    Partial {
        text: String,
        is_final: bool,
        speech_final: bool,
    },
    /// The whole session's transcript, after `audio.done`.
    Done { text: String },
    Error { message: String },
    Other,
}

pub fn parse_event(json: &str) -> Event {
    let Ok(value) = serde_json::from_str::<Value>(json) else {
        return Event::Other;
    };
    match value.get("type").and_then(Value::as_str).unwrap_or("") {
        "transcript.created" => Event::Created,
        "transcript.partial" => Event::Partial {
            text: value
                .get("text")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned(),
            is_final: value
                .get("is_final")
                .and_then(Value::as_bool)
                .unwrap_or(false),
            speech_final: value
                .get("speech_final")
                .and_then(Value::as_bool)
                .unwrap_or(false),
        },
        "transcript.done" => Event::Done {
            text: value
                .get("text")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned(),
        },
        "error" => Event::Error {
            message: value
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or("the provider reported an error")
                .to_owned(),
        },
        _ => Event::Other,
    }
}

/// Turns the provider's overlapping results into pieces not yet typed.
///
/// Providers may send each locked chunk on its own, resend the whole
/// utterance each time, or do both. Whatever has already reached the target
/// application is never sent again, so a resend can never double a word.
#[derive(Default)]
pub struct DeltaTracker {
    /// What the target application has received, as model text.
    typed: String,
    /// The utterance currently being constructed.
    current: String,
}

impl DeltaTracker {
    /// Feeds one final event; returns the text to type, if any.
    pub fn accept(&mut self, text: &str, speech_final: bool) -> Option<String> {
        let text = text.trim();
        if text.is_empty() {
            return None;
        }
        let delta = if speech_final {
            let delta = if self.current.is_empty() {
                // A new utterance: it follows whatever came before.
                self.spaced(text)
            } else if let Some(rest) = text.strip_prefix(self.current.as_str()) {
                // The stitched utterance continues the locked pieces, so the
                // spacing is already in the text.
                rest.to_owned()
            } else if self.current.starts_with(text) {
                // Shorter than what is already accounted for.
                String::new()
            } else {
                // The provider revised earlier words. Its version wins for
                // whatever has not been typed; typed text cannot be rewritten.
                self.spaced(&tail_after_common_prefix(&self.current, text))
            };
            self.current.clear();
            delta
        } else if self.current.is_empty() {
            let delta = self.spaced(text);
            self.current = delta.clone();
            delta
        } else if let Some(rest) = text.strip_prefix(self.current.as_str()) {
            self.current = text.to_owned();
            rest.to_owned()
        } else if self.current.starts_with(text) {
            // A shorter resend of something already accounted for.
            String::new()
        } else {
            // A new chunk of the same utterance.
            let delta = format!(" {text}");
            self.current.push_str(&delta);
            delta
        };
        if delta.is_empty() {
            return None;
        }
        self.typed.push_str(&delta);
        Some(delta)
    }

    /// Keeps a new utterance from running into the previous one.
    fn spaced(&self, text: &str) -> String {
        let previous = self.typed.chars().last();
        let next = text.chars().next();
        let glued = matches!(
            (previous, next),
            (Some(previous), Some(next)) if !previous.is_whitespace() && next.is_alphanumeric()
        );
        if glued { format!(" {text}") } else { text.to_owned() }
    }
}

fn tail_after_common_prefix(previous: &str, text: &str) -> String {
    let common = previous
        .chars()
        .zip(text.chars())
        .take_while(|(left, right)| left == right)
        .count();
    text.chars().skip(common).collect::<String>().trim_start().to_owned()
}

/// Turns captured audio into the 16 kHz mono stream the provider wants.
pub struct AudioPump {
    channels: usize,
    resampler: Option<SincFixedIn<f32>>,
    pending: Vec<f32>,
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
        })
    }

    /// Adds one captured packet and returns whatever is ready to send.
    pub fn push(&mut self, pcm: &[i16]) -> Result<Vec<i16>, String> {
        for frame in pcm.chunks_exact(self.channels) {
            let sum: f32 = frame
                .iter()
                .map(|sample| f32::from(*sample) / 32768.0)
                .sum();
            self.pending.push(sum / self.channels as f32);
        }
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
        Ok(out)
    }
}

fn f32_to_i16(sample: f32) -> i16 {
    (sample.clamp(-1.0, 1.0) * 32767.0).round() as i16
}

/// Little-endian PCM16, which is what `encoding=pcm` means.
fn pcm_bytes(samples: &[i16]) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(samples.len() * 2);
    for sample in samples {
        bytes.extend_from_slice(&sample.to_le_bytes());
    }
    bytes
}

/// Where completed pieces go. Implemented by the dictation layer, which
/// types them and shows them; tests use a recorder.
pub trait TranscriptSink: Send + Sync {
    /// Hands one locked piece to the target application and returns it as
    /// delivered — the user's vocabulary may have rewritten it on the way.
    fn commit(&self, text: &str) -> Result<String, String>;
    /// The session stopped being useful; `message` says why.
    fn failed(&self, message: &str);
}

/// What a finished session produced.
#[derive(Clone, Debug, Default)]
pub struct Shared {
    /// Everything successfully typed into the target window.
    pub typed: String,
    /// The provider's final answer for the whole session, when it arrives.
    pub final_text: String,
    /// The first thing that went wrong, if anything did.
    pub error: Option<String>,
}

enum Ending {
    /// The take ended normally; the provider should be asked to flush.
    Flushed,
    /// The provider already said it was done; there is nothing to flush.
    Completed,
    /// The take was discarded; nothing more goes to the target application.
    Abandoned,
    Failed(String),
}

pub enum Control {
    Flush,
    Abandon,
}

/// A live transcription in flight.
pub struct Session {
    control: mpsc::Sender<Control>,
    shared: Arc<Mutex<Shared>>,
    task: tauri::async_runtime::JoinHandle<()>,
}

impl Session {
    /// Ends the take and waits for the provider's last words.
    pub async fn finish(self, timeout: Duration) -> Shared {
        let _ = self.control.try_send(Control::Flush);
        let Session { shared, task, .. } = self;
        let _ = tokio::time::timeout(timeout, task).await;
        shared.lock().map(|shared| shared.clone()).unwrap_or_default()
    }

    /// Drops the live session without waiting for anything it still owes.
    pub fn cancel(self) {
        let _ = self.control.try_send(Control::Abandon);
    }
}

/// Opens a session and starts feeding it in the background.
pub fn spawn(config: Config, sink: Arc<dyn TranscriptSink>, audio: mpsc::Receiver<Vec<i16>>) -> Session {
    let shared = Arc::new(Mutex::new(Shared::default()));
    let (control, control_receiver) = mpsc::channel(1);
    let task_shared = Arc::clone(&shared);
    let task = tauri::async_runtime::spawn(async move {
        let ending =
            match run_session(config, Arc::clone(&sink), audio, control_receiver, &task_shared).await
            {
                Ok(ending) => ending,
                Err(message) => Ending::Failed(message),
            };
        if let Ending::Failed(message) = ending {
            fail(&task_shared, sink.as_ref(), message);
        }
    });
    Session { control, shared, task }
}

pub struct Config {
    pub url: String,
    pub api_key: String,
}

async fn run_session(
    config: Config,
    sink: Arc<dyn TranscriptSink>,
    mut audio: mpsc::Receiver<Vec<i16>>,
    mut control: mpsc::Receiver<Control>,
    shared: &Mutex<Shared>,
) -> Result<Ending, String> {
    let mut request = config
        .url
        .as_str()
        .into_client_request()
        .map_err(|error| error.to_string())?;
    let authorization = HeaderValue::from_str(&format!("Bearer {}", config.api_key))
        .map_err(|error| error.to_string())?;
    request.headers_mut().insert("Authorization", authorization);

    let mut socket = match tokio::time::timeout(CONNECT_TIMEOUT, connect_async(request)).await {
        Ok(Ok((socket, _))) => socket,
        Ok(Err(error)) => return Err(error.to_string()),
        Err(_) => return Err("the live transcription service did not answer".to_owned()),
    };

    let mut tracker = DeltaTracker::default();
    let mut typing_ok = true;

    // The server wants a ready signal before the first frame.
    loop {
        match tokio::time::timeout(CREATED_TIMEOUT, socket.next()).await {
            Ok(Some(Ok(Message::Text(text)))) => {
                if parse_event(text.as_str()) == Event::Created {
                    break;
                }
            }
            Ok(Some(Ok(Message::Close(_)))) | Ok(None) => {
                return Err("the live transcription service closed the connection".to_owned());
            }
            Ok(Some(Err(error))) => return Err(error.to_string()),
            Ok(Some(Ok(_))) => {}
            Err(_) => return Err("the live transcription service did not start".to_owned()),
        }
    }

    let ending = 'stream: loop {
        tokio::select! {
            frame = audio.recv() => match frame {
                Some(frame) if !frame.is_empty() => {
                    socket
                        .send(Message::binary(pcm_bytes(&frame)))
                        .await
                        .map_err(|error| error.to_string())?;
                }
                Some(_) => {}
                // The recorder let go of the audio: the take is over.
                None => break 'stream Ending::Flushed,
            },
            command = control.recv() => match command {
                Some(Control::Flush) | None => break 'stream Ending::Flushed,
                Some(Control::Abandon) => break 'stream Ending::Abandoned,
            },
            message = socket.next() => match message {
                Some(Ok(Message::Text(text))) => {
                    if handle_event(&parse_event(text.as_str()), &mut tracker, sink.as_ref(), shared, &mut typing_ok) {
                        break 'stream Ending::Completed;
                    }
                }
                Some(Ok(Message::Close(_))) | None => {
                    break 'stream Ending::Failed(
                        "the live transcription service closed the connection".to_owned()
                    );
                }
                Some(Err(error)) => break 'stream Ending::Failed(error.to_string()),
                Some(Ok(_)) => {}
            },
        }
    };

    if matches!(ending, Ending::Flushed) {
        // Push-to-talk: lock the last utterance, then ask for the summary.
        let _ = socket.send(Message::text(FINALIZE)).await;
        let _ = socket.send(Message::text(AUDIO_DONE)).await;
        let deadline = tokio::time::Instant::now() + FLUSH_TIMEOUT;
        loop {
            match tokio::time::timeout_at(deadline, socket.next()).await {
                Ok(Some(Ok(Message::Text(text)))) => {
                    if handle_event(&parse_event(text.as_str()), &mut tracker, sink.as_ref(), shared, &mut typing_ok) {
                        break;
                    }
                }
                Ok(Some(Ok(_))) => {}
                Ok(Some(Err(_))) | Ok(None) | Err(_) => break,
            }
        }
    }
    let _ = socket.close(None).await;
    Ok(ending)
}

/// Handles one provider event; returns true when the session is complete.
fn handle_event(
    event: &Event,
    tracker: &mut DeltaTracker,
    sink: &dyn TranscriptSink,
    shared: &Mutex<Shared>,
    typing_ok: &mut bool,
) -> bool {
    match event {
        Event::Partial {
            text,
            is_final,
            speech_final,
        } => {
            if !*is_final {
                return false;
            }
            if let Some(delta) = tracker.accept(text, *speech_final) {
                if *typing_ok {
                    match sink.commit(&delta) {
                        Ok(delivered) => {
                            if let Ok(mut shared) = shared.lock() {
                                shared.typed.push_str(&delivered);
                            }
                        }
                        Err(error) => {
                            *typing_ok = false;
                            if let Ok(mut shared) = shared.lock()
                                && shared.error.is_none()
                            {
                                shared.error = Some(error.clone());
                            }
                            sink.failed(&error);
                        }
                    }
                }
            }
            false
        }
        Event::Done { text } => {
            if let Ok(mut shared) = shared.lock()
                && !text.trim().is_empty()
            {
                shared.final_text = text.clone();
            }
            true
        }
        Event::Error { message } => {
            if let Ok(mut shared) = shared.lock()
                && shared.error.is_none()
            {
                shared.error = Some(message.clone());
            }
            sink.failed(message);
            false
        }
        Event::Created | Event::Other => false,
    }
}

fn fail(shared: &Mutex<Shared>, sink: &dyn TranscriptSink, message: String) {
    if let Ok(mut shared) = shared.lock()
        && shared.error.is_none()
    {
        shared.error = Some(message.clone());
    }
    sink.failed(&message);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sink() -> Arc<dyn TranscriptSink> {
        Arc::new(RecordingSink::default())
    }

    #[derive(Default)]
    struct RecordingSink {
        committed: Mutex<Vec<String>>,
        failures: Mutex<Vec<String>>,
    }

    impl TranscriptSink for RecordingSink {
        fn commit(&self, text: &str) -> Result<String, String> {
            self.committed.lock().unwrap().push(text.to_owned());
            Ok(text.to_owned())
        }

        fn failed(&self, message: &str) {
            self.failures.lock().unwrap().push(message.to_owned());
        }
    }

    #[test]
    fn a_ready_signal_is_recognized() {
        assert_eq!(parse_event(r#"{"type":"transcript.created"}"#), Event::Created);
    }

    #[test]
    fn a_final_piece_carries_both_kinds_of_finality() {
        assert_eq!(
            parse_event(r#"{"type":"transcript.partial","text":"Dzień dobry","is_final":true,"speech_final":false}"#),
            Event::Partial {
                text: "Dzień dobry".into(),
                is_final: true,
                speech_final: false,
            }
        );
    }

    #[test]
    fn a_done_event_carries_the_whole_transcript() {
        assert_eq!(
            parse_event(r#"{"type":"transcript.done","text":"Całe zdanie."}"#),
            Event::Done {
                text: "Całe zdanie.".into(),
            }
        );
    }

    #[test]
    fn an_error_event_carries_its_message() {
        assert_eq!(
            parse_event(r#"{"type":"error","message":"quota exceeded"}"#),
            Event::Error {
                message: "quota exceeded".into(),
            }
        );
    }

    #[test]
    fn unknown_or_broken_messages_are_ignored_rather_than_fatal() {
        assert_eq!(parse_event("not json"), Event::Other);
        assert_eq!(parse_event(r#"{"type":"transcript.partial"}"#), Event::Partial {
            text: String::new(),
            is_final: false,
            speech_final: false,
        });
    }

    #[test]
    fn chunk_finals_are_typed_once_each() {
        let mut tracker = DeltaTracker::default();

        assert_eq!(tracker.accept("Hello", false), Some("Hello".into()));
        assert_eq!(tracker.accept("world", false), Some(" world".into()));
    }

    #[test]
    fn an_utterance_final_does_not_repeat_what_was_typed() {
        let mut tracker = DeltaTracker::default();
        tracker.accept("Hello", false);
        tracker.accept("world", false);

        // The server stitches the utterance and marks it final.
        assert_eq!(tracker.accept("Hello world.", true), Some(".".into()));
        // And the next utterance starts fresh.
        assert_eq!(tracker.accept("How are you", true), Some(" How are you".into()));
    }

    #[test]
    fn cumulative_results_only_contribute_their_tail() {
        let mut tracker = DeltaTracker::default();

        assert_eq!(tracker.accept("Hello", false), Some("Hello".into()));
        assert_eq!(tracker.accept("Hello world", false), Some(" world".into()));
        assert_eq!(tracker.accept("Hello world", false), None, "nothing new");
        assert_eq!(tracker.accept("Hello world!", true), Some("!".into()));
    }

    #[test]
    fn a_revised_utterance_only_types_what_was_missing() {
        // Chunk finals lock the first pieces...
        let mut tracker = DeltaTracker::default();
        tracker.accept("send it to Ala", false);

        // ...and the stitched utterance spells the name out.
        assert_eq!(
            tracker.accept("send it to Alan now", true),
            Some("n now".into())
        );
    }

    #[test]
    fn a_rewritten_utterance_does_not_repeat_the_typed_prefix() {
        // The model replaced the name entirely. Typed text cannot be taken
        // back, but the rest of the sentence still has to arrive.
        let mut tracker = DeltaTracker::default();
        tracker.accept("send it to Ala", false);

        assert_eq!(tracker.accept("send it to Ola", true), Some(" Ola".into()));
    }

    #[test]
    fn a_lone_utterance_is_typed_in_full() {
        let mut tracker = DeltaTracker::default();

        assert_eq!(
            tracker.accept("Pierwsze zdanie.", true),
            Some("Pierwsze zdanie.".into())
        );
        assert_eq!(tracker.accept("Drugie.", true), Some(" Drugie.".into()));
    }

    #[test]
    fn punctuation_is_not_pushed_away_with_a_space() {
        let mut tracker = DeltaTracker::default();
        tracker.accept("Gotowe", true);

        assert_eq!(tracker.accept(".", true), Some(".".into()));
    }

    #[test]
    fn empty_events_contribute_nothing() {
        let mut tracker = DeltaTracker::default();

        assert_eq!(tracker.accept("   ", false), None);
        assert_eq!(tracker.accept("", true), None);
    }

    #[test]
    fn the_stream_url_carries_the_model_language_and_keyterms() {
        let url = xai_stream_url(
            "https://api.x.ai/v1",
            "grok-voice-transcribe-2.0",
            "pl",
            &["Parakeet".into(), "Loquara".into()],
        );

        assert!(url.starts_with("wss://api.x.ai/v1/stt?"), "{url}");
        assert!(url.contains("model=grok-voice-transcribe-2.0"), "{url}");
        assert!(url.contains("encoding=pcm"), "{url}");
        assert!(url.contains("sample_rate=16000"), "{url}");
        assert!(url.contains("language=pl"), "{url}");
        assert!(url.contains("keyterm=Parakeet"), "{url}");
        assert!(url.contains("keyterm=Loquara"), "{url}");
    }

    #[test]
    fn a_custom_base_url_becomes_a_socket_address() {
        assert_eq!(
            xai_stream_url("http://127.0.0.1:8080/v1/stt", "m", "", &[]),
            "ws://127.0.0.1:8080/v1/stt?model=m&sample_rate=16000&encoding=pcm"
        );
    }

    #[test]
    fn pcm_samples_travel_little_endian() {
        assert_eq!(pcm_bytes(&[1, -2]), vec![0x01, 0x00, 0xFE, 0xFF]);
    }

    #[test]
    fn a_48k_stereo_packet_becomes_16k_mono() {
        let mut pump = AudioPump::new(2, 48_000).unwrap();
        // 4800 frames of stereo = 9600 samples = one tenth of a second.
        let packet: Vec<i16> = (0..9_600).map(|index| (index % 100) as i16).collect();

        let out = pump.push(&packet).unwrap();

        // Four whole chunks are processed (4096 of 4800 frames); the rest
        // waits for the next packet.
        assert!((1_300..=1_400).contains(&out.len()), "got {}", out.len());
    }

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

    /// The whole conversation, against a local socket: headers, framing,
    /// finalize/audio.done, and the text that comes out the other end.
    #[tokio::test]
    async fn a_session_types_what_the_server_streams() {
        use std::sync::atomic::{AtomicBool, Ordering};

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let saw_authorization = Arc::new(AtomicBool::new(false));
        let saw_audio_done = Arc::new(AtomicBool::new(false));
        let server_authorization = Arc::clone(&saw_authorization);
        let server_done = Arc::clone(&saw_audio_done);
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut socket = tokio_tungstenite::accept_hdr_async(
                stream,
                move |request: &tokio_tungstenite::tungstenite::handshake::server::Request,
                      response| {
                    if request
                        .headers()
                        .get("authorization")
                        .and_then(|value| value.to_str().ok())
                        == Some("Bearer test-key")
                    {
                        server_authorization.store(true, Ordering::SeqCst);
                    }
                    Ok(response)
                },
            )
            .await
            .unwrap();
            socket
                .send(Message::text(r#"{"type":"transcript.created"}"#))
                .await
                .unwrap();
            let mut frames = 0;
            while let Some(Ok(message)) = socket.next().await {
                match message {
                    Message::Binary(_) => frames += 1,
                    Message::Text(text) if text.as_str().contains("audio.done") => {
                        server_done.store(true, Ordering::SeqCst);
                        break;
                    }
                    _ => {}
                }
            }
            assert!(frames > 0, "audio must reach the server");
            socket
                .send(Message::text(
                    r#"{"type":"transcript.partial","text":"Dzień dobry","is_final":true,"speech_final":false}"#,
                ))
                .await
                .unwrap();
            socket
                .send(Message::text(
                    r#"{"type":"transcript.partial","text":"Dzień dobry, jak się masz?","is_final":true,"speech_final":true}"#,
                ))
                .await
                .unwrap();
            socket
                .send(Message::text(
                    r#"{"type":"transcript.done","text":"Dzień dobry, jak się masz?"}"#,
                ))
                .await
                .unwrap();
        });

        let recorder = Arc::new(RecordingSink::default());
        let sink: Arc<dyn TranscriptSink> = recorder.clone();
        let (audio_tx, audio_rx) = mpsc::channel(16);
        let (_control_tx, control_rx) = mpsc::channel(1);
        let shared = Arc::new(Mutex::new(Shared::default()));
        let config = Config {
            url: format!("ws://127.0.0.1:{port}/stt?model=test"),
            api_key: "test-key".into(),
        };

        for _ in 0..3 {
            audio_tx.send(vec![0_i16; 320]).await.unwrap();
        }
        drop(audio_tx);

        let ending = run_session(config, sink, audio_rx, control_rx, &shared)
            .await
            .unwrap();

        assert!(matches!(ending, Ending::Flushed));
        assert_eq!(
            recorder.committed.lock().unwrap().as_slice(),
            ["Dzień dobry", ", jak się masz?"]
        );
        assert_eq!(
            shared.lock().unwrap().final_text,
            "Dzień dobry, jak się masz?"
        );
        assert!(saw_audio_done.load(Ordering::SeqCst));
        assert!(saw_authorization.load(Ordering::SeqCst), "the key must travel");
        server.await.unwrap();
    }
}
