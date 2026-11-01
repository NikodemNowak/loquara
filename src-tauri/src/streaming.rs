//! Buffered live transcription. Drafts never inject keyboard input.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use rubato::{
    Resampler, SincFixedIn, SincInterpolationParameters, SincInterpolationType, WindowFunction,
};
use serde_json::Value;
use tokio::sync::mpsc;

use crate::buffered::DraftAssembler;
use tokio_tungstenite::connect_async;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::http::HeaderValue;

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
pub fn xai_stream_url(base_url: &str, model: &str, language: &str, keyterms: &[String]) -> String {
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
    // `interim_results` is what turns the stream from a piece every few
    // seconds into words as they are spoken; the typist decides how much of
    // that uncertainty is safe to show.
    let mut url = format!(
        "{base}?model={}&sample_rate={TARGET_SAMPLE_RATE}&encoding=pcm&interim_results=true",
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
    Done {
        text: String,
    },
    Error {
        message: String,
    },
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
        "transcript.done" => match value.get("text").and_then(Value::as_str) {
            Some(text) => Event::Done { text: text.to_owned() },
            None => Event::Error { message: "provider final is missing its transcript; using complete recording".into() },
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

/// Turns captured audio into the 16 kHz mono stream the provider wants.
pub struct AudioPump {
    channels: usize,
    resampler: Option<SincFixedIn<f32>>,
    pending: Vec<f32>,
    interleaved: Vec<i16>,
    input_frames: usize,
    output_frames: usize,
    sample_rate: u32,
    flushed: bool,
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
            interleaved: Vec::new(),
            input_frames: 0,
            output_frames: 0,
            sample_rate: sample_rate.max(1),
            flushed: false,
        })
    }

    /// Adds one captured packet and returns whatever is ready to send.
    pub fn push(&mut self, pcm: &[i16]) -> Result<Vec<i16>, String> {
        if self.flushed {
            return Err("audio pump already finalized".into());
        }
        self.interleaved.extend_from_slice(pcm);
        let complete = self.interleaved.len() / self.channels * self.channels;
        for frame in self.interleaved[..complete].chunks_exact(self.channels) {
            self.input_frames += 1;
            let sum: f32 = frame
                .iter()
                .map(|sample| f32::from(*sample) / 32768.0)
                .sum();
            self.pending.push(sum / self.channels as f32);
        }
        self.interleaved.drain(..complete);
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
        self.output_frames += out.len();
        Ok(out)
    }

    /// Drain fractional input and filter tail before audio.done. Output length
    /// tracks input frames exactly, including packets split inside a frame.
    pub fn flush(&mut self) -> Result<Vec<i16>, String> {
        if self.flushed {
            return Ok(Vec::new());
        }
        self.flushed = true;
        let expected = self.input_frames * TARGET_SAMPLE_RATE as usize / self.sample_rate as usize;
        let needed = expected.saturating_sub(self.output_frames);
        let mut out = Vec::new();
        if let Some(resampler) = self.resampler.as_mut() {
            let pending = std::mem::take(&mut self.pending);
            let mut first = true;
            while out.len() < needed {
                let waves = if first && !pending.is_empty() {
                    resampler.process_partial(Some(&[pending.as_slice()]), None)
                } else {
                    resampler.process_partial::<&[f32]>(None, None)
                }
                .map_err(|error| error.to_string())?;
                first = false;
                if let Some(wave) = waves.first() {
                    out.extend(wave.iter().map(|sample| f32_to_i16(*sample)));
                } else {
                    return Err("resampler produced no tail".into());
                }
            }
        }
        out.truncate(needed);
        self.output_frames += out.len();
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

/// Whole replaceable draft; no keyboard/focus capability is exposed.
pub trait TranscriptSink: Send + Sync {
    fn publish(&self, text: &str) -> Result<(), String>;
    fn failed(&self, message: &str);
}

#[derive(Clone, Debug, Default)]
pub struct Shared {
    pub final_text: Option<String>,
    pub error: Option<String>,
}
impl Shared {
    pub fn authoritative_final(&self) -> Option<&str> {
        if self.error.is_some() {
            None
        } else {
            self.final_text.as_deref()
        }
    }
}

enum Ending {
    /// The take ended normally; the provider should be asked to flush.
    Flushed,
    /// The provider already said it was done; there is nothing to flush.
    Completed,
    Failed(String),
}

pub enum Control {
    Flush,
}

/// Serializes shutdown with synchronous Windows input already in progress.
/// Aborting an async task alone cannot interrupt a synchronous sink callback.
struct SessionSink {
    inner: Arc<dyn TranscriptSink>,
    active: Mutex<bool>,
}

impl SessionSink {
    fn new(inner: Arc<dyn TranscriptSink>) -> Self {
        Self {
            inner,
            active: Mutex::new(true),
        }
    }

    fn stop(&self) {
        if let Ok(mut active) = self.active.lock() {
            *active = false;
        }
    }
}

impl TranscriptSink for SessionSink {
    fn publish(&self, text: &str) -> Result<(), String> {
        let active = self.active.lock().map_err(|_| "draft output lock failed")?;
        if !*active {
            return Err("live transcription stopped".into());
        }
        self.inner.publish(text)
    }
    fn failed(&self, message: &str) {
        if let Ok(active) = self.active.lock()
            && *active
        {
            self.inner.failed(message);
        }
    }
}

/// A live transcription in flight.
pub struct Session {
    control: mpsc::Sender<Control>,
    shared: Arc<Mutex<Shared>>,
    task: tauri::async_runtime::JoinHandle<()>,
    sink: Arc<SessionSink>,
}

/// Remains usable after the session has moved into its finishing task.
#[derive(Clone)]
pub struct Cancellation {
    sink: Arc<SessionSink>,
    task: tokio::task::AbortHandle,
}

impl Cancellation {
    pub fn cancel(&self) {
        self.sink.stop();
        self.task.abort();
    }
}

impl Session {
    pub fn invalidate(&self, message: String) {
        if let Ok(mut shared) = self.shared.lock() {
            shared.error = Some(message);
        }
    }
    pub fn cancellation(&self) -> Cancellation {
        Cancellation {
            sink: self.sink.clone(),
            task: self.task.inner().abort_handle(),
        }
    }
    /// Ends the take and waits for the provider's last words.
    pub async fn finish(self, timeout: Duration) -> Shared {
        let _ = self.control.try_send(Control::Flush);
        let Session {
            shared,
            mut task,
            sink,
            ..
        } = self;
        if tokio::time::timeout(timeout, &mut task).await.is_err() {
            sink.stop();
            task.abort();
            if let Ok(mut outcome) = shared.lock() {
                outcome.error = Some("live final timed out; using recording".into());
            }
            // No old writer may survive into batch fallback or a new take.
            let _ = task.await;
        }
        sink.stop();
        shared
            .lock()
            .map(|shared| shared.clone())
            .unwrap_or_default()
    }

    /// Drops the live session without waiting for anything it still owes.
    pub fn cancel(self) {
        self.cancellation().cancel();
    }
}

/// Opens a session and starts feeding it in the background.
pub fn spawn(
    config: Config,
    sink: Arc<dyn TranscriptSink>,
    audio: mpsc::Receiver<Vec<i16>>,
) -> Session {
    let gated_sink = Arc::new(SessionSink::new(sink));
    let sink: Arc<dyn TranscriptSink> = gated_sink.clone();
    let shared = Arc::new(Mutex::new(Shared::default()));
    let (control, control_receiver) = mpsc::channel(1);
    let task_shared = Arc::clone(&shared);
    let task = tauri::async_runtime::spawn(async move {
        let ending = match run_session(
            config,
            Arc::clone(&sink),
            audio,
            control_receiver,
            &task_shared,
        )
        .await
        {
            Ok(ending) => ending,
            Err(message) => Ending::Failed(message),
        };
        if let Ending::Failed(message) = ending {
            fail(&task_shared, sink.as_ref(), message);
        }
    });
    Session {
        control,
        shared,
        task,
        sink: gated_sink,
    }
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
    let Config { url, api_key } = config;
    let mut request = url
        .as_str()
        .into_client_request()
        .map_err(|error| error.to_string())?;
    let authorization =
        HeaderValue::from_str(&format!("Bearer {api_key}")).map_err(|error| error.to_string())?;
    request.headers_mut().insert("Authorization", authorization);

    let mut socket = match tokio::time::timeout(CONNECT_TIMEOUT, connect_async(request)).await {
        Ok(Ok((socket, _))) => socket,
        Ok(Err(error)) => return Err(error.to_string()),
        Err(_) => return Err("the live transcription service did not answer".to_owned()),
    };

    let mut draft = DraftAssembler::default();

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
            },
            message = socket.next() => match message {
                Some(Ok(Message::Text(text))) => {
                    if handle_event(&parse_event(text.as_str()), &mut draft, sink.as_ref(), shared) {
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
        // Flush may win the select while captured packets are still queued.
        // Close the input and send every accepted packet before finalizing.
        audio.close();
        while let Some(frame) = audio.recv().await {
            if !frame.is_empty() {
                socket
                    .send(Message::binary(pcm_bytes(&frame)))
                    .await
                    .map_err(|error| error.to_string())?;
            }
        }
        // Push-to-talk: lock the last utterance, then ask for the summary.
        socket
            .send(Message::text(FINALIZE))
            .await
            .map_err(|error| error.to_string())?;
        socket
            .send(Message::text(AUDIO_DONE))
            .await
            .map_err(|error| error.to_string())?;
        let deadline = tokio::time::Instant::now() + FLUSH_TIMEOUT;
        loop {
            match tokio::time::timeout_at(deadline, socket.next()).await {
                Ok(Some(Ok(Message::Text(text)))) => {
                    if handle_event(
                        &parse_event(text.as_str()),
                        &mut draft,
                        sink.as_ref(),
                        shared,
                    ) {
                        break;
                    }
                }
                Ok(Some(Ok(_))) => {}
                Ok(Some(Err(_))) | Ok(None) | Err(_) => break,
            }
        }
    }
    let _ = socket.close(None).await;
    if matches!(ending, Ending::Completed) {
        return Err(
            "provider completed before captured audio was finalized; using complete recording"
                .into(),
        );
    }
    Ok(ending)
}

/// Handles one provider event; returns true when the session is complete.
fn handle_event(
    event: &Event,
    draft: &mut DraftAssembler,
    sink: &dyn TranscriptSink,
    shared: &Mutex<Shared>,
) -> bool {
    match event {
        Event::Partial {
            text,
            is_final,
            speech_final,
        } => {
            let text = draft.update(text, *is_final, *speech_final);
            // Clipboard lock is recoverable: final recognition and delivery continue.
            let _ = sink.publish(&text);
            false
        }
        Event::Done { text } => {
            if let Ok(mut shared) = shared.lock() {
                shared.final_text = Some(text.clone());
            }
            true
        }
        Event::Error { message } => {
            fail(shared, sink, message.clone());
            true
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

    #[derive(Default)]
    struct RecordingSink {
        document: Mutex<String>,
        failures: Mutex<Vec<String>>,
    }
    impl RecordingSink {
        fn document(&self) -> String {
            self.document.lock().unwrap().clone()
        }
    }
    impl TranscriptSink for RecordingSink {
        fn publish(&self, text: &str) -> Result<(), String> {
            *self.document.lock().unwrap() = text.to_owned();
            Ok(())
        }
        fn failed(&self, message: &str) {
            self.failures.lock().unwrap().push(message.into());
        }
    }
    #[test]
    fn a_ready_signal_is_recognized() {
        assert_eq!(
            parse_event(r#"{"type":"transcript.created"}"#),
            Event::Created
        );
    }
    #[test]
    fn a_final_piece_carries_both_kinds_of_finality() {
        assert_eq!(
            parse_event(
                r#"{"type":"transcript.partial","text":"Dzień dobry","is_final":true,"speech_final":false}"#
            ),
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
        assert_eq!(
            parse_event(r#"{"type":"transcript.partial"}"#),
            Event::Partial {
                text: String::new(),
                is_final: false,
                speech_final: false,
            }
        );
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
        assert!(url.contains("interim_results=true"), "{url}");
        assert!(url.contains("language=pl"), "{url}");
        assert!(url.contains("keyterm=Parakeet"), "{url}");
        assert!(url.contains("keyterm=Loquara"), "{url}");
    }
    #[test]
    fn a_custom_base_url_becomes_a_socket_address() {
        assert_eq!(
            xai_stream_url("http://127.0.0.1:8080/v1/stt", "m", "", &[]),
            "ws://127.0.0.1:8080/v1/stt?model=m&sample_rate=16000&encoding=pcm&interim_results=true"
        );
    }
    #[test]
    fn pcm_samples_travel_little_endian() {
        assert_eq!(pcm_bytes(&[1, -2]), vec![0x01, 0x00, 0xFE, 0xFF]);
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
        assert_eq!(recorder.document(), "Dzień dobry, jak się masz?");
        assert_eq!(
            shared.lock().unwrap().final_text.as_deref(),
            Some("Dzień dobry, jak się masz?")
        );
        assert!(saw_audio_done.load(Ordering::SeqCst));
        assert!(
            saw_authorization.load(Ordering::SeqCst),
            "the key must travel"
        );
        server.await.unwrap();
    }
}
#[cfg(test)]
mod session_lifecycle_tests {
    use super::*;

    #[derive(Default)]
    struct Sink {
        document: Mutex<String>,
    }

    impl TranscriptSink for Sink {
        fn publish(&self, text: &str) -> Result<(), String> {
            *self.document.lock().unwrap() = text.to_owned();
            Ok(())
        }
        fn failed(&self, _message: &str) {}
    }

    fn config(port: u16) -> Config {
        Config {
            url: format!("ws://127.0.0.1:{port}/stt?model=test"),
            api_key: "test-key".into(),
        }
    }

    #[tokio::test]
    async fn flush_sends_all_queued_audio_before_audio_done() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let (received_tx, received_rx) = tokio::sync::oneshot::channel();
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut socket = tokio_tungstenite::accept_async(stream).await.unwrap();
            socket
                .send(Message::text(r#"{"type":"transcript.created"}"#))
                .await
                .unwrap();
            let mut frames = 0;
            let mut audio_done = false;
            while let Some(Ok(message)) = socket.next().await {
                match message {
                    Message::Binary(_) if !audio_done => frames += 1,
                    Message::Text(text) if text.as_str().contains("audio.done") => {
                        audio_done = true;
                        break;
                    }
                    Message::Binary(_) => panic!("audio frame arrived after audio.done"),
                    _ => {}
                }
            }
            let _ = received_tx.send((frames, audio_done));
        });

        let sink: Arc<dyn TranscriptSink> = Arc::new(Sink::default());
        let (control_tx, control_rx) = mpsc::channel(1);
        control_tx.try_send(Control::Flush).unwrap();
        let (audio_tx, audio_rx) = mpsc::channel(256);
        for index in 0..128_i16 {
            audio_tx.send(vec![index]).await.unwrap();
        }
        drop(audio_tx);
        let shared = Arc::new(Mutex::new(Shared::default()));
        let ending = run_session(config(port), sink, audio_rx, control_rx, &shared)
            .await
            .unwrap();
        let (frames, audio_done) = received_rx.await.unwrap();

        assert!(matches!(ending, Ending::Flushed));
        assert_eq!(frames, 128);
        assert!(audio_done);
        server.await.unwrap();
    }
}

#[cfg(test)]
mod buffered_regressions {
    use super::*;
    #[test]
    fn empty_final_field_is_valid_but_absent_or_nontext_field_requires_fallback() {
        assert_eq!(parse_event(r#"{"type":"transcript.done","text":""}"#), Event::Done { text: String::new() });
        assert!(matches!(parse_event(r#"{"type":"transcript.done"}"#), Event::Error { .. }));
        assert!(matches!(parse_event(r#"{"type":"transcript.done","text":null}"#), Event::Error { .. }));
    }
    #[test]
    fn a_received_empty_final_is_not_missing() {
        let shared = Shared {
            final_text: Some(String::new()),
            ..Default::default()
        };
        assert_eq!(shared.authoritative_final(), Some(""));
        assert_eq!(Shared::default().authoritative_final(), None);
    }
    #[test]
    fn any_audio_gap_forces_complete_wav_fallback_even_with_final() {
        let shared = Shared {
            final_text: Some("prefix".into()),
            error: Some("overflow".into()),
        };
        assert_eq!(shared.authoritative_final(), None);
    }
    #[test]
    fn pump_flush_preserves_every_frame_for_common_rates_and_channels() {
        for rate in [16_000, 44_100, 48_000] {
            for channels in [1, 2, 3] {
                for frames in [1, 200, 1024, 1999, rate as usize + 73] {
                    let mut pump = AudioPump::new(channels, rate).unwrap();
                    let input = vec![1000; frames * channels as usize];
                    let mut out = Vec::new();
                    for packet in input.chunks(137) {
                        out.extend(pump.push(packet).unwrap());
                    }
                    out.extend(pump.flush().unwrap());
                    assert_eq!(
                        out.len(),
                        frames * 16_000 / rate as usize,
                        "rate {rate} channels {channels} frames {frames}"
                    );
                    assert!(pump.flush().unwrap().is_empty());
                }
            }
        }
    }
}
