//! Transcription through a provider's HTTP API.
//!
//! Every provider here takes a finished recording and answers with a
//! transcript. That fits the way Loquara works: the audio is captured to a
//! WAV file first, so the same take can be sent to the cloud or handed to the
//! local engine, and a failed request leaves a recording that can be retried
//! rather than lost.
//!
//! The request shapes differ in details — xAI puts its endpoint at `/v1/stt`
//! and authenticates with a bearer token, Deepgram wants a `Token` prefix and
//! raw bytes, ElevenLabs uses an `xi-api-key` header — but all of them can be
//! driven from one multipart builder and a small table, which is what this
//! module is.

use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::Serialize;
use serde_json::Value;

/// A dictation is something the user is waiting for, so a request that hangs
/// must fail rather than hold the "Przepisuję…" state forever.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(90);

/// xAI accepts at most 100 key terms of 50 characters each. The vocabulary
/// list is user-written and unbounded, so it is trimmed to fit.
const MAX_KEYTERMS: usize = 100;
const MAX_KEYTERM_CHARS: usize = 50;

/// How the provider expects the key to be presented.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Auth {
    /// `Authorization: Bearer <key>` — xAI, OpenAI, Groq, Mistral, custom.
    Bearer,
    /// `Authorization: Token <key>` — Deepgram.
    Token,
    /// `xi-api-key: <key>` — ElevenLabs.
    XiApiKey,
}

/// The request/response dialect a provider speaks.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Api {
    /// `POST /stt` with multipart fields and `{"text": ...}` back.
    Xai,
    /// OpenAI's `/audio/transcriptions` and everything that copies it.
    OpenAi,
    ElevenLabs,
    Deepgram,
}

pub struct Provider {
    pub key: &'static str,
    pub display: &'static str,
    /// Used when the user has not supplied an address of their own.
    pub base_url: &'static str,
    pub default_model: &'static str,
    pub models: &'static [&'static str],
    pub api: Api,
    pub auth: Auth,
    /// Whether the address above is a placeholder the user has to replace.
    pub custom: bool,
}

/// The providers Loquara can talk to, best-known first.
pub const PROVIDERS: &[Provider] = &[
    Provider {
        key: "xai",
        display: "xAI (Grok)",
        base_url: "https://api.x.ai/v1",
        default_model: "grok-voice-transcribe-2.0",
        models: &["grok-voice-transcribe-2.0", "grok-voice-transcribe-1.0"],
        api: Api::Xai,
        auth: Auth::Bearer,
        custom: false,
    },
    Provider {
        key: "openai",
        display: "OpenAI",
        base_url: "https://api.openai.com/v1",
        default_model: "gpt-transcribe",
        models: &[
            "gpt-transcribe",
            "gpt-4o-transcribe",
            "gpt-4o-mini-transcribe",
            "whisper-1",
        ],
        api: Api::OpenAi,
        auth: Auth::Bearer,
        custom: false,
    },
    Provider {
        key: "groq",
        display: "Groq",
        base_url: "https://api.groq.com/openai/v1",
        default_model: "whisper-large-v3-turbo",
        models: &[
            "whisper-large-v3-turbo",
            "whisper-large-v3",
            "distil-whisper-large-v3-en",
        ],
        api: Api::OpenAi,
        auth: Auth::Bearer,
        custom: false,
    },
    Provider {
        key: "mistral",
        display: "Mistral",
        base_url: "https://api.mistral.ai/v1",
        default_model: "voxtral-mini-latest",
        models: &["voxtral-mini-latest", "voxtral-small-latest"],
        api: Api::OpenAi,
        auth: Auth::Bearer,
        custom: false,
    },
    Provider {
        key: "elevenlabs",
        display: "ElevenLabs",
        base_url: "https://api.elevenlabs.io/v1",
        default_model: "scribe_v2",
        models: &["scribe_v2", "scribe_v2_medical"],
        api: Api::ElevenLabs,
        auth: Auth::XiApiKey,
        custom: false,
    },
    Provider {
        key: "deepgram",
        display: "Deepgram",
        base_url: "https://api.deepgram.com/v1",
        default_model: "nova-3",
        models: &["nova-3", "nova-2"],
        api: Api::Deepgram,
        auth: Auth::Token,
        custom: false,
    },
    Provider {
        key: "custom",
        display: "Inny (OpenAI-compatible)",
        base_url: "",
        default_model: "",
        models: &[],
        api: Api::OpenAi,
        auth: Auth::Bearer,
        custom: true,
    },
];

pub fn provider(key: &str) -> Option<&'static Provider> {
    PROVIDERS.iter().find(|provider| provider.key == key)
}

pub fn default_provider() -> &'static Provider {
    &PROVIDERS[0]
}

/// The catalogue as the interface needs it: enough to render a provider
/// dropdown and suggest model names, with nothing about requests in it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderInfo {
    pub key: String,
    pub display: String,
    pub default_model: String,
    pub models: Vec<String>,
    pub custom: bool,
}

pub fn catalog() -> Vec<ProviderInfo> {
    PROVIDERS
        .iter()
        .map(|provider| ProviderInfo {
            key: provider.key.to_owned(),
            display: provider.display.to_owned(),
            default_model: provider.default_model.to_owned(),
            models: provider.models.iter().map(|model| (*model).to_owned()).collect(),
            custom: provider.custom,
        })
        .collect()
}

#[derive(Debug, thiserror::Error)]
pub enum CloudError {
    #[error("unknown transcription provider: {0}")]
    UnknownProvider(String),
    #[error("no API key is set for {provider}; add one in Settings")]
    MissingKey { provider: String },
    #[error("no API address is set for {provider}; add one in Settings")]
    MissingBaseUrl { provider: String },
    #[error("could not reach the transcription service: {0}")]
    Network(String),
    #[error("the transcription service refused the request ({status}): {message}")]
    Rejected { status: u16, message: String },
    #[error("the transcription service returned an unexpected response: {0}")]
    Response(String),
    #[error("could not read the recording: {0}")]
    Audio(String),
    #[error("transcription cancelled")]
    Cancelled,
}

/// One recording on its way to a provider.
pub struct Request<'a> {
    pub provider: &'a str,
    /// Empty means "use the provider's default model".
    pub model: &'a str,
    /// Empty means "use the provider's own address".
    pub base_url: &'a str,
    pub api_key: &'a str,
    /// Empty means "let the model detect it".
    pub language: &'a str,
    /// Vocabulary hints, used by providers that support them.
    pub keyterms: &'a [String],
    pub audio: &'a [u8],
    pub file_name: &'a str,
    pub cancel: Option<&'a AtomicBool>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Transcript {
    pub text: String,
    pub language: Option<String>,
}

pub fn transcribe(request: Request<'_>) -> Result<Transcript, CloudError> {
    if request.cancel.is_some_and(|flag| flag.load(Ordering::SeqCst)) {
        return Err(CloudError::Cancelled);
    }
    let provider = provider(request.provider)
        .ok_or_else(|| CloudError::UnknownProvider(request.provider.to_owned()))?;
    if request.api_key.trim().is_empty() {
        return Err(CloudError::MissingKey {
            provider: provider.display.to_owned(),
        });
    }
    if provider.custom && request.base_url.trim().is_empty() {
        return Err(CloudError::MissingBaseUrl {
            provider: provider.display.to_owned(),
        });
    }
    let model = if request.model.trim().is_empty() {
        provider.default_model
    } else {
        request.model.trim()
    };
    let (url, content_type, body) = build(provider, model, &request);
    let response = send(&url, provider.auth, request.api_key.trim(), &content_type, &body)?;
    if request.cancel.is_some_and(|flag| flag.load(Ordering::SeqCst)) {
        return Err(CloudError::Cancelled);
    }
    parse(provider.api, &response)
}

/// Where a request goes.
///
/// A custom provider may be configured with either a base like
/// `https://host/v1` or the full transcription path; both are accepted so a
/// pasted endpoint from the provider's own documentation works.
pub fn endpoint(provider: &Provider, base_url: &str) -> String {
    let base = if base_url.trim().is_empty() {
        provider.base_url
    } else {
        base_url.trim()
    };
    let base = base.trim_end_matches('/');
    match provider.api {
        Api::Xai => format!("{base}/stt"),
        Api::OpenAi => {
            if base.ends_with("/audio/transcriptions") {
                base.to_owned()
            } else {
                format!("{base}/audio/transcriptions")
            }
        }
        Api::ElevenLabs => format!("{base}/speech-to-text"),
        Api::Deepgram => format!("{base}/listen"),
    }
}

/// The address, the content type and the bytes of one request.
fn build(provider: &Provider, model: &str, request: &Request<'_>) -> (String, String, Vec<u8>) {
    let base = endpoint(provider, request.base_url);
    match provider.api {
        Api::Deepgram => {
            let mut url = format!("{base}?model={}&smart_format=true", url_escape(model));
            if !request.language.is_empty() {
                url.push_str(&format!("&language={}", url_escape(request.language)));
            }
            // Deepgram takes the audio as the body rather than in a form.
            (url, "audio/wav".to_owned(), request.audio.to_vec())
        }
        api => {
            let (content_type, body) = multipart_request(api, provider, model, request, request.audio);
            (base, content_type, body)
        }
    }
}

fn multipart_request(
    api: Api,
    provider: &Provider,
    model: &str,
    request: &Request<'_>,
    audio: &[u8],
) -> (String, Vec<u8>) {
    let mut form = Multipart::new();
    match api {
        Api::Xai => {
            form.field("model", model);
            // `format` needs a language to normalise numbers and currency
            // into; without one xAI rejects the pair.
            if !request.language.is_empty() {
                form.field("language", request.language);
                form.field("format", "true");
            }
            for term in keyterms(request.keyterms) {
                form.field("keyterm", &term);
            }
        }
        Api::OpenAi => {
            // gpt-transcribe dropped the single-language field, so it is only
            // sent to the models that still accept it.
            if !request.language.is_empty() && !model.starts_with("gpt-transcribe") {
                form.field("language", request.language);
            }
            form.field("model", model);
            if provider.key == "groq" {
                form.field("response_format", "json");
            }
        }
        Api::ElevenLabs => {
            form.field("model_id", model);
            if !request.language.is_empty() {
                form.field("language_code", request.language);
            }
        }
        Api::Deepgram => unreachable!("Deepgram sends raw bytes"),
    }
    form.file("file", request.file_name, "audio/wav", audio);
    form.finish()
}

/// Sends the request and returns the body text, or a described failure.
fn send(
    url: &str,
    auth: Auth,
    api_key: &str,
    content_type: &str,
    body: &[u8],
) -> Result<String, CloudError> {
    let mut request = agent()
        .post(url)
        .set("Content-Type", content_type);
    request = match auth {
        Auth::Bearer => request.set("Authorization", &format!("Bearer {api_key}")),
        Auth::Token => request.set("Authorization", &format!("Token {api_key}")),
        Auth::XiApiKey => request.set("xi-api-key", api_key),
    };
    match request.send_bytes(body) {
        Ok(response) => response
            .into_string()
            .map_err(|error| CloudError::Network(error.to_string())),
        Err(ureq::Error::Status(status, response)) => {
            let body = response.into_string().unwrap_or_default();
            Err(CloudError::Rejected {
                status,
                message: describe_error(&body),
            })
        }
        Err(ureq::Error::Transport(error)) => Err(CloudError::Network(error.to_string())),
    }
}

/// One pooled agent, so a second dictation reuses the TLS connection the
/// first one opened instead of paying for the handshake again.
fn agent() -> &'static ureq::Agent {
    static AGENT: OnceLock<ureq::Agent> = OnceLock::new();
    AGENT.get_or_init(|| ureq::AgentBuilder::new().timeout(REQUEST_TIMEOUT).build())
}

fn parse(api: Api, body: &str) -> Result<Transcript, CloudError> {
    let value: Value = serde_json::from_str(body)
        .map_err(|error| CloudError::Response(format!("{error}: {}", truncate(body, 300))))?;
    let text = match api {
        Api::Deepgram => value
            .pointer("/results/channels/0/alternatives/0/transcript")
            .and_then(Value::as_str),
        _ => value.get("text").and_then(Value::as_str),
    }
    .ok_or_else(|| CloudError::Response(format!("no transcript in {}", truncate(body, 300))))?;
    let language = match api {
        Api::Xai => value.get("language").and_then(Value::as_str),
        Api::ElevenLabs => value.get("language_code").and_then(Value::as_str),
        _ => None,
    };
    Ok(Transcript {
        text: text.trim().to_owned(),
        language: language.map(str::to_owned),
    })
}

/// Pulls the most useful sentence out of an error body.
///
/// Providers disagree on where they put it: OpenAI nests it under `error`,
/// ElevenLabs under `detail`, Deepgram uses `err_msg`, and some answer with
/// plain text. Falling back to the raw body keeps an unfamiliar provider
/// debuggable.
fn describe_error(body: &str) -> String {
    if let Ok(value) = serde_json::from_str::<Value>(body) {
        for pointer in ["/error/message", "/detail/message", "/message", "/err_msg", "/error"] {
            if let Some(text) = value.pointer(pointer).and_then(Value::as_str) {
                let text = text.trim();
                if !text.is_empty() {
                    return truncate(text, 300);
                }
            }
        }
    }
    let body = body.trim();
    if body.is_empty() {
        "the service did not say why".to_owned()
    } else {
        truncate(body, 300)
    }
}

fn truncate(text: &str, limit: usize) -> String {
    if text.chars().count() <= limit {
        return text.to_owned();
    }
    let kept: String = text.chars().take(limit).collect();
    format!("{kept}…")
}

/// Trims the vocabulary to what a provider will accept.
fn keyterms(terms: &[String]) -> Vec<String> {
    let mut seen = std::collections::BTreeSet::new();
    terms
        .iter()
        .map(|term| term.trim())
        .filter(|term| !term.is_empty())
        .map(|term| term.chars().take(MAX_KEYTERM_CHARS).collect::<String>())
        .filter(|term| seen.insert(term.clone()))
        .take(MAX_KEYTERMS)
        .collect()
}

fn url_escape(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len());
    for byte in value.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                escaped.push(byte as char)
            }
            other => escaped.push_str(&format!("%{other:02X}")),
        }
    }
    escaped
}

/// A single-part-at-a-time multipart writer.
///
/// The providers that stream the file care that it comes last, so fields are
/// appended first and the audio is added once at the end.
struct Multipart {
    boundary: String,
    body: Vec<u8>,
}

impl Multipart {
    fn new() -> Self {
        static SEQUENCE: AtomicU64 = AtomicU64::new(0);
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        Self {
            boundary: format!("----loquara{nanos:x}{:x}", SEQUENCE.fetch_add(1, Ordering::Relaxed)),
            body: Vec::new(),
        }
    }

    fn field(&mut self, name: &str, value: &str) {
        self.body.extend_from_slice(
            format!(
                "--{boundary}\r\nContent-Disposition: form-data; name=\"{name}\"\r\n\r\n{value}\r\n",
                boundary = self.boundary,
            )
            .as_bytes(),
        );
    }

    fn file(&mut self, name: &str, file_name: &str, content_type: &str, bytes: &[u8]) {
        self.body.extend_from_slice(
            format!(
                "--{boundary}\r\nContent-Disposition: form-data; name=\"{name}\"; filename=\"{file_name}\"\r\nContent-Type: {content_type}\r\n\r\n",
                boundary = self.boundary,
            )
            .as_bytes(),
        );
        self.body.extend_from_slice(bytes);
        self.body.extend_from_slice(b"\r\n");
    }

    fn finish(mut self) -> (String, Vec<u8>) {
        self.body
            .extend_from_slice(format!("--{}--\r\n", self.boundary).as_bytes());
        (
            format!("multipart/form-data; boundary={}", self.boundary),
            self.body,
        )
    }
}

/// A second of silence, used by the connection test.
///
/// Sending real speech would need a recording the app does not have at that
/// moment; an empty response from a silent clip still proves the key, the
/// address and the model are accepted.
pub fn silence_wav() -> Vec<u8> {
    let spec = hound::WavSpec {
        channels: 1,
        sample_rate: 16_000,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };
    let mut cursor = std::io::Cursor::new(Vec::new());
    {
        let mut writer = hound::WavWriter::new(&mut cursor, spec).expect("cursor writes cannot fail");
        for _ in 0..16_000 {
            writer.write_sample(0_i16).expect("cursor writes cannot fail");
        }
        writer.finalize().expect("cursor writes cannot fail");
    }
    cursor.into_inner()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request<'a>(provider: &'a str, audio: &'a [u8], keyterms: &'a [String]) -> Request<'a> {
        Request {
            provider,
            model: "",
            base_url: "",
            api_key: "test-key",
            language: "",
            keyterms,
            audio,
            file_name: "take.wav",
            cancel: None,
        }
    }

    #[test]
    fn every_catalogue_entry_has_a_key_and_a_default_model() {
        for provider in PROVIDERS {
            assert!(!provider.key.is_empty());
            assert!(!provider.display.is_empty());
            if provider.custom {
                assert!(provider.default_model.is_empty());
            } else {
                assert!(
                    provider.models.contains(&provider.default_model),
                    "{} default model must be selectable",
                    provider.key
                );
            }
        }
    }

    #[test]
    fn xai_uses_its_own_endpoint_and_keeps_the_model_last() {
        let audio = [1_u8, 2, 3];
        let terms = vec!["Parakeet".to_owned()];
        let (content_type, body) = multipart_request(
            Api::Xai,
            provider("xai").unwrap(),
            "grok-voice-transcribe-2.0",
            &request("xai", &audio, &terms),
            &audio,
        );

        assert!(content_type.starts_with("multipart/form-data; boundary="));
        let body = String::from_utf8_lossy(&body);
        let model_at = body.find("name=\"model\"").unwrap();
        let file_at = body.find("name=\"file\"").unwrap();
        assert!(model_at < file_at, "the file has to come last");
        assert!(body.contains("grok-voice-transcribe-2.0"));
        assert!(body.contains("name=\"keyterm\""));
        assert!(body.contains("Parakeet"));
    }

    #[test]
    fn a_language_turns_on_xai_formatting() {
        let audio = [];
        let terms = [];
        let request = Request {
            language: "pl",
            ..request("xai", &audio, &terms)
        };
        let (_, body) = multipart_request(
            Api::Xai,
            provider("xai").unwrap(),
            "grok-voice-transcribe-2.0",
            &request,
            &audio,
        );
        let body = String::from_utf8_lossy(&body);

        assert!(body.contains("name=\"language\"\r\n\r\npl"));
        assert!(body.contains("name=\"format\"\r\n\r\ntrue"));
    }

    #[test]
    fn gpt_transcribe_does_not_get_the_language_field_it_dropped() {
        let audio = [];
        let terms = [];
        let request = Request {
            language: "pl",
            ..request("openai", &audio, &terms)
        };
        let (_, body) = multipart_request(
            Api::OpenAi,
            provider("openai").unwrap(),
            "gpt-transcribe",
            &request,
            &audio,
        );

        assert!(!String::from_utf8_lossy(&body).contains("name=\"language\""));
    }

    #[test]
    fn whisper_still_gets_the_language_field() {
        let audio = [];
        let terms = [];
        let request = Request {
            language: "pl",
            ..request("openai", &audio, &terms)
        };
        let (_, body) = multipart_request(
            Api::OpenAi,
            provider("openai").unwrap(),
            "whisper-1",
            &request,
            &audio,
        );

        assert!(String::from_utf8_lossy(&body).contains("name=\"language\"\r\n\r\npl"));
    }

    #[test]
    fn elevenlabs_uses_its_own_field_names() {
        let audio = [];
        let terms = [];
        let request = Request {
            language: "pl",
            ..request("elevenlabs", &audio, &terms)
        };
        let (_, body) = multipart_request(
            Api::ElevenLabs,
            provider("elevenlabs").unwrap(),
            "scribe_v2",
            &request,
            &audio,
        );
        let body = String::from_utf8_lossy(&body);

        assert!(body.contains("name=\"model_id\""));
        assert!(body.contains("name=\"language_code\""));
    }

    #[test]
    fn a_custom_provider_gets_the_openai_path_appended() {
        let custom = provider("custom").unwrap();

        assert_eq!(
            endpoint(custom, "https://host.example/v1"),
            "https://host.example/v1/audio/transcriptions"
        );
        assert_eq!(
            endpoint(custom, "https://host.example/v1/audio/transcriptions"),
            "https://host.example/v1/audio/transcriptions"
        );
    }

    #[test]
    fn endpoints_are_built_from_the_catalogue() {
        assert_eq!(endpoint(provider("xai").unwrap(), ""), "https://api.x.ai/v1/stt");
        assert_eq!(
            endpoint(provider("groq").unwrap(), ""),
            "https://api.groq.com/openai/v1/audio/transcriptions"
        );
        assert_eq!(
            endpoint(provider("elevenlabs").unwrap(), ""),
            "https://api.elevenlabs.io/v1/speech-to-text"
        );
        assert_eq!(
            endpoint(provider("deepgram").unwrap(), ""),
            "https://api.deepgram.com/v1/listen"
        );
    }

    #[test]
    fn a_base_url_override_wins_over_the_catalogue() {
        assert_eq!(
            endpoint(provider("xai").unwrap(), "http://127.0.0.1:9/v1/"),
            "http://127.0.0.1:9/v1/stt"
        );
    }

    #[test]
    fn xai_replies_are_read_as_text() {
        let transcript = parse(
            Api::Xai,
            r#"{"text":"Dzień dobry.","language":"pl","duration":1.2}"#,
        )
        .unwrap();

        assert_eq!(transcript.text, "Dzień dobry.");
        assert_eq!(transcript.language.as_deref(), Some("pl"));
    }

    #[test]
    fn deepgram_replies_are_read_from_their_nested_shape() {
        let transcript = parse(
            Api::Deepgram,
            r#"{"results":{"channels":[{"alternatives":[{"transcript":"Hello there"}]}]}}"#,
        )
        .unwrap();

        assert_eq!(transcript.text, "Hello there");
        assert_eq!(transcript.language, None);
    }

    #[test]
    fn a_reply_without_a_transcript_is_an_error() {
        let error = parse(Api::OpenAi, r#"{"error":"nope"}"#).unwrap_err();

        assert!(matches!(error, CloudError::Response(_)), "{error:?}");
    }

    #[test]
    fn provider_errors_are_read_wherever_they_are_hidden() {
        assert_eq!(
            describe_error(r#"{"error":{"message":"Incorrect API key provided"}}"#),
            "Incorrect API key provided"
        );
        assert_eq!(
            describe_error(r#"{"detail":{"message":"quota reached"}}"#),
            "quota reached"
        );
        assert_eq!(describe_error(r#"{"err_msg":"bad audio"}"#), "bad audio");
        assert_eq!(describe_error("gateway timeout"), "gateway timeout");
        assert_eq!(describe_error(""), "the service did not say why");
    }

    #[test]
    fn keyterms_are_trimmed_deduplicated_and_capped() {
        let long = "x".repeat(80);
        let mut terms: Vec<String> = vec!["Parakeet".into(), "  Parakeet  ".into()];
        terms.extend((0..200).map(|index| format!("term{index}")));
        terms.push(long.clone());

        let trimmed = keyterms(&terms);

        assert_eq!(trimmed.len(), MAX_KEYTERMS);
        assert_eq!(
            trimmed.iter().filter(|term| *term == "Parakeet").count(),
            1,
            "the same term never travels twice"
        );
        assert_eq!(trimmed[0], "Parakeet", "trimmed before it is compared");

        let truncated = keyterms(&[long]);
        assert_eq!(truncated[0].chars().count(), MAX_KEYTERM_CHARS);
    }

    #[test]
    fn a_missing_key_is_refused_before_any_request() {
        let audio = [];
        let terms = [];
        let request = Request {
            api_key: "  ",
            ..request("xai", &audio, &terms)
        };
        let error = transcribe(request).unwrap_err();

        assert!(matches!(error, CloudError::MissingKey { .. }), "{error:?}");
    }

    #[test]
    fn a_custom_provider_without_an_address_is_refused() {
        let audio = [];
        let terms = [];
        let error = transcribe(request("custom", &audio, &terms)).unwrap_err();

        assert!(matches!(error, CloudError::MissingBaseUrl { .. }), "{error:?}");
    }

    #[test]
    fn an_unknown_provider_is_refused_rather_than_guessed() {
        let audio = [];
        let terms = [];
        let error = transcribe(request("acme", &audio, &terms)).unwrap_err();

        assert!(matches!(error, CloudError::UnknownProvider(_)), "{error:?}");
    }

    #[test]
    fn a_cancelled_request_never_leaves_the_process() {
        let cancel = AtomicBool::new(true);
        let audio = [];
        let terms = [];
        let request = Request {
            cancel: Some(&cancel),
            ..request("xai", &audio, &terms)
        };
        let error = transcribe(request).unwrap_err();

        assert!(matches!(error, CloudError::Cancelled), "{error:?}");
    }

    /// A one-shot HTTP server: answers with `status`/`body` and hands back
    /// everything the client sent, so a test can inspect the request itself.
    fn local_server(status: &'static str, response: &'static str) -> (String, std::thread::JoinHandle<String>) {
        use std::io::{BufRead, Read, Write};

        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let handle = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut reader = std::io::BufReader::new(stream.try_clone().unwrap());
            let mut head = String::new();
            loop {
                let mut line = String::new();
                if reader.read_line(&mut line).unwrap() == 0 || line == "\r\n" {
                    break;
                }
                head.push_str(&line);
            }
            let length = head
                .lines()
                .find_map(|line| {
                    let (name, value) = line.split_once(':')?;
                    name.eq_ignore_ascii_case("content-length")
                        .then(|| value.trim().parse::<usize>().ok())?
                })
                .unwrap_or(0);
            let mut sent = vec![0_u8; length];
            reader.read_exact(&mut sent).unwrap();
            let reply = format!(
                "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{response}",
                response.len()
            );
            stream.write_all(reply.as_bytes()).unwrap();
            stream.flush().unwrap();
            head + &String::from_utf8_lossy(&sent)
        });
        (format!("http://127.0.0.1:{port}/v1"), handle)
    }

    #[test]
    fn a_request_carries_the_key_and_the_audio_to_the_provider() {
        let (base, server) = local_server("200 OK", r#"{"text":"Hello there"}"#);
        let audio = b"RIFFnot-really-a-wav";
        let terms = [];
        let request = Request {
            base_url: &base,
            ..request("openai", audio, &terms)
        };

        let transcript = transcribe(request).unwrap();

        assert_eq!(transcript.text, "Hello there");
        let seen = server.join().unwrap();
        assert!(
            seen.to_ascii_lowercase().contains("authorization: bearer test-key"),
            "{seen}"
        );
        assert!(seen.contains("name=\"file\"; filename=\"take.wav\""), "{seen}");
        assert!(seen.contains("RIFFnot-really-a-wav"), "{seen}");
    }

    #[test]
    fn a_refusal_comes_back_with_the_providers_own_words() {
        let (base, server) = local_server(
            "401 Unauthorized",
            r#"{"error":{"message":"Incorrect API key provided"}}"#,
        );
        let audio = [];
        let terms = [];
        let request = Request {
            base_url: &base,
            ..request("openai", &audio, &terms)
        };

        let error = transcribe(request).unwrap_err();
        let _ = server.join();

        assert!(matches!(&error, CloudError::Rejected { status: 401, .. }), "{error:?}");
        assert!(error.to_string().contains("Incorrect API key provided"), "{error}");
    }

    #[test]
    fn a_custom_provider_reaches_the_address_the_user_gave() {
        let (base, server) = local_server("200 OK", r#"{"text":""}"#);
        let audio = [];
        let terms = [];
        let request = Request {
            base_url: &base,
            language: "pl",
            ..request("custom", &audio, &terms)
        };

        let transcript = transcribe(request).unwrap();

        assert_eq!(transcript.text, "");
        let seen = server.join().unwrap();
        assert!(seen.starts_with("POST /v1/audio/transcriptions "), "{seen}");
    }

    #[test]
    fn the_connection_check_audio_is_a_readable_wav() {
        let bytes = silence_wav();
        let reader = hound::WavReader::new(std::io::Cursor::new(bytes)).unwrap();

        assert_eq!(reader.spec().sample_rate, 16_000);
        assert_eq!(reader.spec().channels, 1);
        assert_eq!(reader.duration(), 16_000);
    }

    #[test]
    fn deepgram_travels_in_the_query_string() {
        let audio = [1_u8, 2, 3];
        let terms = [];
        let request = Request {
            language: "pl",
            ..request("deepgram", &audio, &terms)
        };
        let (url, content_type, body) = build(provider("deepgram").unwrap(), "nova-3", &request);

        assert_eq!(
            url,
            "https://api.deepgram.com/v1/listen?model=nova-3&smart_format=true&language=pl"
        );
        assert_eq!(content_type, "audio/wav");
        assert_eq!(body, audio);
    }

    #[test]
    fn url_escaping_keeps_codes_intact_and_escapes_the_rest() {
        assert_eq!(url_escape("pl"), "pl");
        assert_eq!(url_escape("zh-cn"), "zh-cn");
        assert_eq!(url_escape("a b"), "a%20b");
    }
}
