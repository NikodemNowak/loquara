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

use crate::storage::{VocabularyEntry, apply_vocabulary_text};
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

/// Maximum characters one correction may take back.
///
/// A model that rewrites more than this mid-utterance has lost the plot, and
/// a wider backspace is how a dictation session eats the user's own typing.
const MAX_CORRECTION_CHARS: usize = 160;

/// Turns the provider's live results into keystrokes.
///
/// The stream carries three kinds of text: interim results that may still
/// change, locked segments, and the finished utterance. Interim text is typed
/// as it arrives, so words appear while the user is speaking, and taken back
/// with backspaces when the model changes its mind. Everything removed was
/// typed by this session moments earlier; corrections never reach past the
/// segment in flight, and speculation stops for good if a correction cannot
/// be made — a caret that has moved is not a caret to delete around.
pub struct LiveTypist {
    vocabulary: Vec<VocabularyEntry>,
    /// Everything typed for the session, as the application received it.
    delivered: String,
    /// Delivered text of the utterance being spoken, including the segment in
    /// flight.
    utterance: String,
    /// Delivered text of the segment in flight.
    segment: String,
    /// False once correcting the segment stops being safe.
    speculating: bool,
}

impl LiveTypist {
    pub fn new(vocabulary: Vec<VocabularyEntry>) -> Self {
        Self {
            vocabulary,
            delivered: String::new(),
            utterance: String::new(),
            segment: String::new(),
            speculating: true,
        }
    }

    /// What the target application has received so far.
    pub fn delivered(&self) -> &str {
        &self.delivered
    }

    /// A result that may still change.
    pub fn speculate(&mut self, text: &str, sink: &dyn TranscriptSink) -> Result<(), String> {
        if !self.speculating {
            return Ok(());
        }
        let text = text.trim();
        if text.is_empty() || text == self.segment {
            return Ok(());
        }
        if self.segment.is_empty() {
            return self.append(text, sink, false);
        }
        if let Some(tail) = text.strip_prefix(self.segment.as_str()) {
            return self.append(tail, sink, true);
        }
        // The model changed its mind inside this segment.
        let common = common_prefix_chars(&self.segment, text);
        let remove = self.segment.chars().count().saturating_sub(common);
        if remove > MAX_CORRECTION_CHARS {
            self.speculating = false;
            return Ok(());
        }
        if remove > 0 && !self.remove(remove, sink)? {
            return Ok(());
        }
        let tail: String = text.chars().skip(common).collect();
        self.append(&tail, sink, true)
    }

    /// A locked result: one segment, or the finished utterance.
    pub fn accept(
        &mut self,
        text: &str,
        speech_final: bool,
        sink: &dyn TranscriptSink,
    ) -> Result<(), String> {
        let text = text.trim();
        if text.is_empty() {
            return Ok(());
        }
        if speech_final {
            // The stitched utterance supersedes everything this utterance
            // said, including whatever is still in flight.
            let desired = apply_vocabulary_text(text, &self.vocabulary);
            if desired != self.utterance {
                if self.utterance.is_empty() {
                    self.append(&desired, sink, false)?;
                } else if let Some(tail) = desired.strip_prefix(self.utterance.as_str()) {
                    self.append(tail, sink, true)?;
                } else {
                    // A replacement may only follow a successful retraction.
                    // Keep a large revision in history instead of appending
                    // a second copy to the text that is already visible.
                    let common = common_prefix_chars(&self.utterance, &desired);
                    let remove = self.utterance.chars().count().saturating_sub(common);
                    if remove <= MAX_CORRECTION_CHARS && self.remove(remove, sink)? {
                        let tail: String = desired.chars().skip(common).collect();
                        self.append(&tail, sink, true)?;
                    }
                }
            }
            self.utterance.clear();
            self.segment.clear();
            self.speculating = true;
            return Ok(());
        }
        // A locked segment: it replaces whatever was speculated for it.
        let desired = apply_vocabulary_text(text, &self.vocabulary);
        if desired != self.segment {
            if self.segment.is_empty() {
                self.append(&desired, sink, false)?;
            } else {
                let common = common_prefix_chars(&self.segment, &desired);
                let remove = self.segment.chars().count().saturating_sub(common);
                if remove <= MAX_CORRECTION_CHARS && self.remove(remove, sink)? {
                    let tail: String = desired.chars().skip(common).collect();
                    self.append(&tail, sink, true)?;
                } else {
                    self.speculating = false;
                }
            }
        }
        self.segment.clear();
        Ok(())
    }

    /// Takes back the last `count` characters. Returns whether it worked.
    fn remove(&mut self, count: usize, sink: &dyn TranscriptSink) -> Result<bool, String> {
        if count == 0 {
            return Ok(true);
        }
        if !self.speculating {
            // Speculation was abandoned; letting the wrong text stand is
            // better than deleting around a caret that may no longer be ours.
            return Ok(false);
        }
        if let Err(error) = sink.retract(count) {
            self.speculating = false;
            return Err(error);
        }
        truncate_chars(&mut self.delivered, count);
        truncate_chars(&mut self.utterance, count);
        truncate_chars(&mut self.segment, count);
        Ok(true)
    }

    /// Types one piece, spacing it away from whatever came before.
    fn append(
        &mut self,
        text: &str,
        sink: &dyn TranscriptSink,
        continuation: bool,
    ) -> Result<(), String> {
        if text.is_empty() {
            return Ok(());
        }
        let delivered = if continuation {
            text.to_owned()
        } else {
            self.spaced(text)
        };
        sink.type_text(&delivered)?;
        self.delivered.push_str(&delivered);
        // Separators between utterances/segments belong to the document,
        // not the provider's segment text used for later comparisons.
        self.utterance.push_str(if self.utterance.is_empty() {
            text
        } else {
            &delivered
        });
        self.segment.push_str(text);
        Ok(())
    }

    /// Keeps a new piece from running into the previous one.
    fn spaced(&self, text: &str) -> String {
        if !self.segment.is_empty() || self.delivered.is_empty() {
            return text.to_owned();
        }
        let previous = self.delivered.chars().last();
        let next = text.chars().next();
        let glued = matches!(
            (previous, next),
            (Some(previous), Some(next)) if !previous.is_whitespace() && next.is_alphanumeric()
        );
        if glued {
            format!(" {text}")
        } else {
            text.to_owned()
        }
    }
}

fn common_prefix_chars(left: &str, right: &str) -> usize {
    left.chars()
        .zip(right.chars())
        .take_while(|(left, right)| left == right)
        .count()
}

fn truncate_chars(text: &mut String, count: usize) {
    let keep = text.chars().count().saturating_sub(count);
    *text = text.chars().take(keep).collect();
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

/// Where characters go. Implemented by the dictation layer, which types them
/// into the focused window; tests simulate a document.
pub trait TranscriptSink: Send + Sync {
    /// Types text at the caret.
    fn type_text(&self, text: &str) -> Result<(), String>;
    /// Removes the last `chars` characters, which this session typed there.
    fn retract(&self, chars: usize) -> Result<(), String>;
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
    /// Input may have reached the target even though its call returned an error.
    pub delivery_uncertain: bool,
}

impl Shared {
    pub fn can_auto_deliver(&self) -> bool {
        self.typed.trim().is_empty() && !self.delivery_uncertain
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
    fn type_text(&self, text: &str) -> Result<(), String> {
        let active = self.active.lock().map_err(|_| "live input lock failed")?;
        if !*active {
            return Err("live transcription stopped".into());
        }
        self.inner.type_text(text)
    }

    fn retract(&self, chars: usize) -> Result<(), String> {
        let active = self.active.lock().map_err(|_| "live input lock failed")?;
        if !*active {
            return Err("live transcription stopped".into());
        }
        self.inner.retract(chars)
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
    /// Applied to locked text, so the words that land in the application use
    /// the user's own spelling.
    pub vocabulary: Vec<VocabularyEntry>,
}

async fn run_session(
    config: Config,
    sink: Arc<dyn TranscriptSink>,
    mut audio: mpsc::Receiver<Vec<i16>>,
    mut control: mpsc::Receiver<Control>,
    shared: &Mutex<Shared>,
) -> Result<Ending, String> {
    let Config {
        url,
        api_key,
        vocabulary,
    } = config;
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

    let mut typist = LiveTypist::new(vocabulary);
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
            },
            message = socket.next() => match message {
                Some(Ok(Message::Text(text))) => {
                    if handle_event(&parse_event(text.as_str()), &mut typist, sink.as_ref(), shared, &mut typing_ok) {
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
        let _ = socket.send(Message::text(FINALIZE)).await;
        let _ = socket.send(Message::text(AUDIO_DONE)).await;
        let deadline = tokio::time::Instant::now() + FLUSH_TIMEOUT;
        loop {
            match tokio::time::timeout_at(deadline, socket.next()).await {
                Ok(Some(Ok(Message::Text(text)))) => {
                    if handle_event(
                        &parse_event(text.as_str()),
                        &mut typist,
                        sink.as_ref(),
                        shared,
                        &mut typing_ok,
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
    Ok(ending)
}

/// Handles one provider event; returns true when the session is complete.
fn handle_event(
    event: &Event,
    typist: &mut LiveTypist,
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
            if *typing_ok {
                let outcome = if *is_final {
                    typist.accept(text, *speech_final, sink)
                } else {
                    typist.speculate(text, sink)
                };
                if let Err(error) = outcome {
                    // Typing itself is broken — an elevated window, say.
                    // Receiving continues so the provider's own transcript
                    // can still be kept.
                    *typing_ok = false;
                    if let Ok(mut shared) = shared.lock() {
                        shared.delivery_uncertain = true;
                        if shared.error.is_none() {
                            shared.error = Some(error.clone());
                        }
                    }
                    sink.failed(&error);
                }
            }
            if let Ok(mut shared) = shared.lock() {
                shared.typed = typist.delivered().to_owned();
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

    use std::sync::atomic::{AtomicBool, Ordering};

    /// Stands in for the target application: text lands, backspaces take it
    /// away, and the test reads the result like a document.
    #[derive(Default)]
    struct RecordingSink {
        document: Mutex<String>,
        failures: Mutex<Vec<String>>,
        /// Set to make retraction fail, as a moved caret would.
        retract_fails: AtomicBool,
        partial_type_failure: AtomicBool,
    }

    impl RecordingSink {
        fn document(&self) -> String {
            self.document.lock().unwrap().clone()
        }
    }

    impl TranscriptSink for RecordingSink {
        fn type_text(&self, text: &str) -> Result<(), String> {
            if self.partial_type_failure.load(Ordering::SeqCst) {
                self.document.lock().unwrap().extend(text.chars().take(3));
                return Err("input failed after accepting part of the batch".into());
            }
            self.document.lock().unwrap().push_str(text);
            Ok(())
        }

        fn retract(&self, chars: usize) -> Result<(), String> {
            if self.retract_fails.load(Ordering::SeqCst) {
                return Err("the caret is not where the words went".into());
            }
            let mut document = self.document.lock().unwrap();
            let keep = document.chars().count().saturating_sub(chars);
            *document = document.chars().take(keep).collect();
            Ok(())
        }

        fn failed(&self, message: &str) {
            self.failures.lock().unwrap().push(message.to_owned());
        }
    }

    fn typist() -> (LiveTypist, Arc<RecordingSink>) {
        (
            LiveTypist::new(Vec::new()),
            Arc::new(RecordingSink::default()),
        )
    }

    struct TaskDropSignal(Option<tokio::sync::oneshot::Sender<()>>);

    impl Drop for TaskDropSignal {
        fn drop(&mut self) {
            if let Some(sender) = self.0.take() {
                let _ = sender.send(());
            }
        }
    }

    async fn pending_test_session() -> (
        Session,
        tokio::sync::oneshot::Receiver<()>,
        tokio::task::AbortHandle,
    ) {
        let (started_tx, started_rx) = tokio::sync::oneshot::channel();
        let (dropped_tx, dropped_rx) = tokio::sync::oneshot::channel();
        let task = tauri::async_runtime::spawn(async move {
            let _signal = TaskDropSignal(Some(dropped_tx));
            let _ = started_tx.send(());
            std::future::pending::<()>().await;
        });
        let abort = task.inner().abort_handle();
        let (control, _receiver) = mpsc::channel(1);
        let session = Session {
            control,
            shared: Arc::new(Mutex::new(Shared::default())),
            task,
            sink: Arc::new(SessionSink::new(Arc::new(RecordingSink::default()))),
        };
        started_rx.await.unwrap();
        (session, dropped_rx, abort)
    }

    #[tokio::test]
    async fn finish_timeout_joins_the_writer_before_allowing_fallback() {
        let (session, mut dropped, abort) = pending_test_session().await;
        session.finish(Duration::from_millis(10)).await;
        let stopped = dropped.try_recv().is_ok();
        abort.abort();
        assert!(
            stopped,
            "a timed-out session must not leave its writer running"
        );
    }

    #[tokio::test]
    async fn cancellation_aborts_a_writer_waiting_for_the_provider() {
        let (session, dropped, abort) = pending_test_session().await;
        session.cancel();
        let stopped = tokio::time::timeout(Duration::from_secs(1), dropped)
            .await
            .is_ok();
        abort.abort();
        assert!(
            stopped,
            "cancellation must work while handshake or flush is waiting"
        );
    }

    #[tokio::test]
    async fn cancellation_stays_reachable_after_a_session_moves_into_finish() {
        let (session, dropped, _abort) = pending_test_session().await;
        let cancellation = session.cancellation();
        let gated = session.sink.clone();
        let finishing = tokio::spawn(session.finish(Duration::from_secs(10)));
        cancellation.cancel();
        tokio::time::timeout(Duration::from_secs(1), finishing)
            .await
            .unwrap()
            .unwrap();
        dropped.await.unwrap();
        assert!(gated.type_text("late words").is_err());
    }

    #[test]
    fn a_stopped_session_cannot_type_retract_or_report_late_errors() {
        let sink = Arc::new(RecordingSink::default());
        let gated = SessionSink::new(sink.clone());
        gated.type_text("Keep this").unwrap();
        gated.stop();
        assert!(gated.type_text(" twice").is_err());
        assert!(gated.retract(4).is_err());
        gated.failed("late failure from an old session");
        assert_eq!(sink.document(), "Keep this");
        assert!(sink.failures.lock().unwrap().is_empty());
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
    fn partials_are_typed_as_they_arrive() {
        let (mut typist, sink) = typist();

        typist.speculate("Dzień", sink.as_ref()).unwrap();
        typist.speculate("Dzień dobry", sink.as_ref()).unwrap();

        assert_eq!(sink.document(), "Dzień dobry");
    }

    #[test]
    fn a_changed_partial_is_taken_back_and_retyped() {
        let (mut typist, sink) = typist();

        typist.speculate("Send it to Anna", sink.as_ref()).unwrap();
        typist.speculate("Send it to Ala", sink.as_ref()).unwrap();

        assert_eq!(sink.document(), "Send it to Ala");
    }

    #[test]
    fn a_locked_segment_replaces_its_speculation() {
        let (mut typist, sink) = typist();

        typist.speculate("Send it to Anna", sink.as_ref()).unwrap();
        typist
            .accept("Send it to Ala", false, sink.as_ref())
            .unwrap();

        assert_eq!(sink.document(), "Send it to Ala");
    }

    #[test]
    fn interim_segments_keep_word_boundaries() {
        let (mut typist, sink) = typist();
        typist.accept("Hello", false, sink.as_ref()).unwrap();
        typist.speculate("world", sink.as_ref()).unwrap();
        typist.speculate("world again", sink.as_ref()).unwrap();
        assert_eq!(sink.document(), "Hello world again");
        typist
            .accept("Hello world again.", true, sink.as_ref())
            .unwrap();
        assert_eq!(sink.document(), "Hello world again.");
        typist.speculate("Hello", sink.as_ref()).unwrap();
        assert_eq!(sink.document(), "Hello world again. Hello");
    }

    #[test]
    fn a_long_interim_after_a_locked_chunk_matches_the_stitched_final() {
        let (mut typist, sink) = typist();
        let continuation = "kolejne słowa wypowiedzi ".repeat(12).trim().to_owned();
        typist.accept("To są", false, sink.as_ref()).unwrap();
        typist.speculate(&continuation, sink.as_ref()).unwrap();
        let final_text = format!("To są {continuation}.");
        typist.accept(&final_text, true, sink.as_ref()).unwrap();
        assert_eq!(sink.document(), final_text);
    }

    #[test]
    fn corrected_segments_preserve_the_separator_after_previous_utterances() {
        let (mut typist, sink) = typist();
        typist.accept("Gotowe.", true, sink.as_ref()).unwrap();
        typist.speculate("Żaba", sink.as_ref()).unwrap();
        typist.accept("Żółw", false, sink.as_ref()).unwrap();
        typist.accept("Żółw.", true, sink.as_ref()).unwrap();
        assert_eq!(sink.document(), "Gotowe. Żółw.");
    }

    #[test]
    fn identical_locked_chunks_remain_distinct() {
        let (mut typist, sink) = typist();
        typist.accept("tak", false, sink.as_ref()).unwrap();
        typist.speculate("tak", sink.as_ref()).unwrap();
        typist.accept("tak", false, sink.as_ref()).unwrap();
        typist.accept("tak tak", true, sink.as_ref()).unwrap();
        assert_eq!(sink.document(), "tak tak");
    }

    #[test]
    fn a_long_final_revision_does_not_append_a_second_copy() {
        let (mut typist, sink) = typist();
        let original = format!("hello {}", "a long spoken sentence ".repeat(12))
            .trim()
            .to_owned();
        typist.speculate(&original, sink.as_ref()).unwrap();
        let revised = format!("Hello{}.", &original[5..]);
        typist.accept(&revised, true, sink.as_ref()).unwrap();
        assert_eq!(sink.document(), original);
    }

    #[test]
    fn a_failed_final_correction_never_appends_its_replacement() {
        let (mut typist, sink) = typist();
        typist.speculate("Hello wrld", sink.as_ref()).unwrap();
        sink.retract_fails.store(true, Ordering::SeqCst);
        assert!(typist.accept("Hello world", true, sink.as_ref()).is_err());
        assert_eq!(sink.document(), "Hello wrld");
    }

    #[test]
    fn repeated_utterances_are_preserved_with_live_interims() {
        let (mut typist, sink) = typist();
        for _ in 0..2 {
            typist.speculate("Tak", sink.as_ref()).unwrap();
            typist.accept("Tak.", true, sink.as_ref()).unwrap();
        }
        assert_eq!(sink.document(), "Tak. Tak.");
    }

    #[test]
    fn partial_input_failure_blocks_automatic_fallback_and_further_typing() {
        let (mut typist, sink) = typist();
        sink.partial_type_failure.store(true, Ordering::SeqCst);
        let shared = Mutex::new(Shared::default());
        let mut typing_ok = true;
        let partial = Event::Partial {
            text: "Hello world".into(),
            is_final: false,
            speech_final: false,
        };
        handle_event(
            &partial,
            &mut typist,
            sink.as_ref(),
            &shared,
            &mut typing_ok,
        );
        assert_eq!(sink.document(), "Hel");
        assert!(!shared.lock().unwrap().can_auto_deliver());
        handle_event(
            &partial,
            &mut typist,
            sink.as_ref(),
            &shared,
            &mut typing_ok,
        );
        assert_eq!(sink.document(), "Hel");
        handle_event(
            &Event::Done {
                text: "Hello world.".into(),
            },
            &mut typist,
            sink.as_ref(),
            &shared,
            &mut typing_ok,
        );
        assert_eq!(shared.lock().unwrap().final_text, "Hello world.");
        assert!(!shared.lock().unwrap().can_auto_deliver());
    }

    #[test]
    fn provider_failure_before_input_still_allows_batch_delivery() {
        let (mut typist, sink) = typist();
        let shared = Mutex::new(Shared::default());
        let mut typing_ok = true;
        handle_event(
            &Event::Error {
                message: "connection failed".into(),
            },
            &mut typist,
            sink.as_ref(),
            &shared,
            &mut typing_ok,
        );
        assert!(shared.lock().unwrap().can_auto_deliver());
    }

    #[test]
    fn the_finished_utterance_adds_only_its_tail() {
        let (mut typist, sink) = typist();

        typist.speculate("Hello", sink.as_ref()).unwrap();
        typist.accept("Hello", false, sink.as_ref()).unwrap();
        typist.accept("Hello world.", true, sink.as_ref()).unwrap();

        assert_eq!(sink.document(), "Hello world.");
    }

    #[test]
    fn a_new_utterance_is_spaced_after_the_previous_one() {
        let (mut typist, sink) = typist();

        typist
            .accept("Pierwsze zdanie.", true, sink.as_ref())
            .unwrap();
        typist.accept("Drugie.", true, sink.as_ref()).unwrap();

        assert_eq!(sink.document(), "Pierwsze zdanie. Drugie.");
    }

    #[test]
    fn a_revised_utterance_corrects_what_it_can() {
        let (mut typist, sink) = typist();

        typist.speculate("Send it to Anna", sink.as_ref()).unwrap();
        typist
            .accept("Send it to Ala", true, sink.as_ref())
            .unwrap();

        assert_eq!(sink.document(), "Send it to Ala");
    }

    #[test]
    fn vocabulary_lands_with_the_locked_text_not_the_speculation() {
        let vocabulary = vec![VocabularyEntry {
            id: 1,
            heard: "parakit".into(),
            replacement: "Parakeet".into(),
        }];
        let sink = Arc::new(RecordingSink::default());
        let mut typist = LiveTypist::new(vocabulary);

        typist.speculate("parakit", sink.as_ref()).unwrap();
        assert_eq!(sink.document(), "parakit");
        typist.accept("parakit", false, sink.as_ref()).unwrap();

        assert_eq!(sink.document(), "Parakeet");
    }

    #[test]
    fn a_caret_that_moved_stops_the_corrections_rather_than_deleting() {
        let (mut typist, sink) = typist();
        typist.speculate("Hello", sink.as_ref()).unwrap();
        typist.speculate("Hello wrld", sink.as_ref()).unwrap();
        sink.retract_fails.store(true, Ordering::SeqCst);

        // The model corrects itself, but the caret is no longer ours: the
        // wrong word stands and nothing is removed.
        assert!(typist.speculate("Hello world", sink.as_ref()).is_err());
        assert_eq!(sink.document(), "Hello wrld");

        // And no further corrections are attempted for the rest of the take.
        typist.accept("Hello world", false, sink.as_ref()).unwrap();
        assert_eq!(sink.document(), "Hello wrld");
    }

    #[test]
    fn empty_events_do_nothing() {
        let (mut typist, sink) = typist();

        typist.speculate("   ", sink.as_ref()).unwrap();
        typist.accept("", true, sink.as_ref()).unwrap();

        assert_eq!(sink.document(), "");
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
            vocabulary: Vec::new(),
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
            shared.lock().unwrap().final_text,
            "Dzień dobry, jak się masz?"
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
        fn type_text(&self, text: &str) -> Result<(), String> {
            self.document.lock().unwrap().push_str(text);
            Ok(())
        }

        fn retract(&self, chars: usize) -> Result<(), String> {
            let mut document = self.document.lock().unwrap();
            let keep = document.chars().count().saturating_sub(chars);
            *document = document.chars().take(keep).collect();
            Ok(())
        }

        fn failed(&self, _message: &str) {}
    }

    fn config(port: u16) -> Config {
        Config {
            url: format!("ws://127.0.0.1:{port}/stt?model=test"),
            api_key: "test-key".into(),
            vocabulary: Vec::new(),
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
