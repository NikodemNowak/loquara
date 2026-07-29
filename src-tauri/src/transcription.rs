//! Typed, synchronous JSONL client for the long-lived Python worker.
//!
//! A client is owned mutably by its caller and does not use a global mutex.
//! Stdout is drained on a dedicated reader thread, so response waits are
//! bounded without holding a lock across unrelated application work.

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use std::ffi::OsStr;
use std::fmt;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::thread;
use std::time::Duration;

#[derive(Debug, Serialize)]
pub struct WorkerRequest {
    request_id: String,
    #[serde(flatten)]
    command: WorkerCommand,
}

#[derive(Debug, Serialize)]
#[serde(tag = "command", rename_all = "snake_case")]
enum WorkerCommand {
    Ping,
    Load,
    Transcribe {
        audio_path: PathBuf,
        #[serde(skip_serializing_if = "Option::is_none")]
        language: Option<String>,
    },
}

impl WorkerRequest {
    pub fn ping(request_id: impl Into<String>) -> Self {
        Self {
            request_id: request_id.into(),
            command: WorkerCommand::Ping,
        }
    }

    pub fn load(request_id: impl Into<String>) -> Self {
        Self {
            request_id: request_id.into(),
            command: WorkerCommand::Load,
        }
    }

    pub fn transcribe(
        request_id: impl Into<String>,
        audio_path: impl AsRef<Path>,
        language: Option<String>,
    ) -> Result<Self, ClientError> {
        let audio_path = audio_path.as_ref();
        if !audio_path.is_file() {
            return Err(ClientError::MissingAudio(audio_path.to_owned()));
        }
        Ok(Self {
            request_id: request_id.into(),
            command: WorkerCommand::Transcribe {
                audio_path: audio_path.to_owned(),
                language,
            },
        })
    }

    fn request_id(&self) -> &str {
        &self.request_id
    }
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct WorkerError {
    pub code: String,
    pub message: String,
    pub retryable: bool,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct PingResult {
    pub status: String,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct LoadResult {
    pub model: String,
    pub device: String,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct TranscriptionResult {
    pub text: String,
    pub model: String,
    #[serde(default)]
    pub language: Option<String>,
    pub duration_ms: u64,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct WorkerResponse<T> {
    request_id: String,
    ok: bool,
    result: Option<T>,
    error: Option<WorkerError>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClientError {
    MissingWorker(PathBuf),
    MissingAudio(PathBuf),
    Spawn(String),
    Io(String),
    Timeout { request_id: String, timeout_ms: u64 },
    Crashed { exit_code: Option<i32> },
    WorkerUnavailable,
    Protocol(String),
    MismatchedRequestId { expected: String, actual: String },
    Worker(WorkerError),
}

impl fmt::Display for ClientError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingWorker(path) => {
                write!(formatter, "worker does not exist: {}", path.display())
            }
            Self::MissingAudio(path) => {
                write!(formatter, "audio file does not exist: {}", path.display())
            }
            Self::Spawn(message) => write!(formatter, "could not start worker: {message}"),
            Self::Io(message) => write!(formatter, "worker I/O failed: {message}"),
            Self::Timeout {
                request_id,
                timeout_ms,
            } => write!(
                formatter,
                "worker request {request_id} timed out after {timeout_ms} ms"
            ),
            Self::Crashed { exit_code } => {
                write!(formatter, "worker crashed with exit code {exit_code:?}")
            }
            Self::WorkerUnavailable => write!(formatter, "worker is unavailable"),
            Self::Protocol(message) => write!(formatter, "invalid worker response: {message}"),
            Self::MismatchedRequestId { expected, actual } => write!(
                formatter,
                "worker response request_id mismatch: expected {expected}, got {actual}"
            ),
            Self::Worker(error) => {
                write!(formatter, "worker error {}: {}", error.code, error.message)
            }
        }
    }
}

impl std::error::Error for ClientError {}

pub fn parse_response<T: DeserializeOwned>(
    line: &str,
    expected_request_id: &str,
) -> Result<T, ClientError> {
    let response: WorkerResponse<T> =
        serde_json::from_str(line).map_err(|error| ClientError::Protocol(error.to_string()))?;

    if response.request_id != expected_request_id {
        return Err(ClientError::MismatchedRequestId {
            expected: expected_request_id.to_owned(),
            actual: response.request_id,
        });
    }

    match (response.ok, response.result, response.error) {
        (true, Some(result), None) => Ok(result),
        (false, None, Some(error)) => Err(ClientError::Worker(error)),
        (true, _, _) => Err(ClientError::Protocol(
            "successful response must contain only result".to_owned(),
        )),
        (false, _, _) => Err(ClientError::Protocol(
            "failed response must contain only error".to_owned(),
        )),
    }
}

pub struct WorkerClient {
    child: Child,
    stdin: Option<ChildStdin>,
    responses: Option<Receiver<Result<String, String>>>,
    timeout: Duration,
    next_request_id: u64,
}

impl WorkerClient {
    pub fn spawn(
        python: impl AsRef<OsStr>,
        worker_path: impl AsRef<Path>,
        timeout: Duration,
    ) -> Result<Self, ClientError> {
        let worker_path = worker_path.as_ref();
        if !worker_path.is_file() {
            return Err(ClientError::MissingWorker(worker_path.to_owned()));
        }

        let mut command = Command::new(python);
        command
            .arg("-X")
            .arg("utf8")
            .arg("-u")
            .arg(worker_path)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit());

        let mut child = command
            .spawn()
            .map_err(|error| ClientError::Spawn(error.to_string()))?;
        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| ClientError::Spawn("worker stdin was not piped".to_owned()))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| ClientError::Spawn("worker stdout was not piped".to_owned()))?;
        let (sender, responses) = mpsc::channel();
        thread::spawn(move || {
            let mut reader = BufReader::new(stdout);
            loop {
                let mut line = String::new();
                match reader.read_line(&mut line) {
                    Ok(0) => break,
                    Ok(_) => {
                        if sender.send(Ok(line)).is_err() {
                            break;
                        }
                    }
                    Err(error) => {
                        let _ = sender.send(Err(error.to_string()));
                        break;
                    }
                }
            }
        });

        Ok(Self {
            child,
            stdin: Some(stdin),
            responses: Some(responses),
            timeout,
            next_request_id: 1,
        })
    }

    pub fn ping(&mut self) -> Result<PingResult, ClientError> {
        let request_id = self.allocate_request_id();
        self.send(WorkerRequest::ping(request_id))
    }

    pub fn load(&mut self) -> Result<LoadResult, ClientError> {
        let request_id = self.allocate_request_id();
        self.send(WorkerRequest::load(request_id))
    }

    pub fn transcribe(
        &mut self,
        audio_path: impl AsRef<Path>,
        language: Option<String>,
    ) -> Result<TranscriptionResult, ClientError> {
        let request_id = self.allocate_request_id();
        let request = WorkerRequest::transcribe(request_id, audio_path, language)?;
        self.send(request)
    }

    fn allocate_request_id(&mut self) -> String {
        let request_id = format!("rust-{}", self.next_request_id);
        self.next_request_id += 1;
        request_id
    }

    fn send<T: DeserializeOwned>(&mut self, request: WorkerRequest) -> Result<T, ClientError> {
        if self.stdin.is_none() || self.responses.is_none() {
            return Err(ClientError::WorkerUnavailable);
        }

        let request_id = request.request_id().to_owned();
        let mut encoded = serde_json::to_vec(&request)
            .map_err(|error| ClientError::Protocol(error.to_string()))?;
        encoded.push(b'\n');
        let write_result = self
            .stdin
            .as_mut()
            .ok_or(ClientError::WorkerUnavailable)?
            .write_all(&encoded)
            .and_then(|()| {
                self.stdin
                    .as_mut()
                    .ok_or_else(|| {
                        std::io::Error::new(
                            std::io::ErrorKind::BrokenPipe,
                            "worker stdin is closed",
                        )
                    })?
                    .flush()
            });
        if let Err(error) = write_result {
            let error = self.crash_or(ClientError::Io(error.to_string()));
            self.shutdown();
            return Err(error);
        }

        let response = self
            .responses
            .as_ref()
            .ok_or(ClientError::WorkerUnavailable)?
            .recv_timeout(self.timeout);
        match response {
            Ok(Ok(line)) => parse_response(&line, &request_id),
            Ok(Err(message)) => {
                let error = self.crash_or(ClientError::Io(message));
                self.shutdown();
                Err(error)
            }
            Err(RecvTimeoutError::Timeout) => {
                let timeout_error = ClientError::Timeout {
                    request_id,
                    timeout_ms: self.timeout.as_millis().try_into().unwrap_or(u64::MAX),
                };
                self.shutdown();
                Err(timeout_error)
            }
            Err(RecvTimeoutError::Disconnected) => {
                let exit_code = self.shutdown();
                Err(ClientError::Crashed { exit_code })
            }
        }
    }

    fn crash_or(&mut self, fallback: ClientError) -> ClientError {
        match self.child.try_wait() {
            Ok(Some(status)) => ClientError::Crashed {
                exit_code: status.code(),
            },
            Ok(None) => fallback,
            Err(error) => ClientError::Io(error.to_string()),
        }
    }

    fn shutdown(&mut self) -> Option<i32> {
        self.stdin.take();
        self.responses.take();
        match self.child.try_wait() {
            Ok(Some(status)) => status.code(),
            Ok(None) => {
                let _ = self.child.kill();
                self.child.wait().ok().and_then(|status| status.code())
            }
            Err(_) => None,
        }
    }
}

impl Drop for WorkerClient {
    fn drop(&mut self) {
        self.shutdown();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::fs;
    use std::path::PathBuf;
    use std::time::Duration;

    fn missing_path(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!("mow-{name}-{}", std::process::id()))
    }

    fn fake_worker(name: &str, source: &str) -> PathBuf {
        let path = missing_path(name).with_extension("py");
        fs::write(&path, source).expect("fake worker should be written");
        path
    }

    fn python() -> String {
        match std::env::var_os("PYTHON").map(PathBuf::from) {
            Some(path) if path.is_file() => path.to_string_lossy().into_owned(),
            Some(path) if path.join("python.exe").is_file() => {
                path.join("python.exe").to_string_lossy().into_owned()
            }
            _ => "python".to_owned(),
        }
    }

    #[test]
    fn serializes_ping_request_with_exact_protocol_fields() {
        let request = WorkerRequest::ping("ping-1");

        assert_eq!(
            serde_json::to_value(request).unwrap(),
            json!({"request_id": "ping-1", "command": "ping"})
        );
    }

    #[test]
    fn serializes_transcribe_request_and_omits_absent_language() {
        let path = fake_worker("request-audio", "");
        let request = WorkerRequest::transcribe("tx-1", &path, None).unwrap();

        assert_eq!(
            serde_json::to_value(request).unwrap(),
            json!({
                "request_id": "tx-1",
                "command": "transcribe",
                "audio_path": path,
            })
        );
        fs::remove_file(path).unwrap();
    }

    #[test]
    fn parses_success_response_and_checks_request_id() {
        let result: PingResult = parse_response(
            r#"{"request_id":"ping-1","ok":true,"result":{"status":"ready"}}"#,
            "ping-1",
        )
        .unwrap();

        assert_eq!(
            result,
            PingResult {
                status: "ready".to_owned()
            }
        );
    }

    #[test]
    fn parses_worker_error_with_retryability() {
        let error = parse_response::<PingResult>(
            r#"{"request_id":"load-1","ok":false,"error":{"code":"model_load_failed","message":"out of memory","retryable":true}}"#,
            "load-1",
        )
        .unwrap_err();

        assert_eq!(
            error,
            ClientError::Worker(WorkerError {
                code: "model_load_failed".to_owned(),
                message: "out of memory".to_owned(),
                retryable: true,
            })
        );
    }

    #[test]
    fn rejects_unknown_success_response_shape() {
        let error = parse_response::<PingResult>(
            r#"{"request_id":"ping-1","ok":true,"result":{"mystery":true}}"#,
            "ping-1",
        )
        .unwrap_err();

        assert!(matches!(error, ClientError::Protocol(_)));
    }

    #[test]
    fn rejects_mismatched_request_id() {
        let error = parse_response::<PingResult>(
            r#"{"request_id":"other","ok":true,"result":{"status":"ready"}}"#,
            "ping-1",
        )
        .unwrap_err();

        assert_eq!(
            error,
            ClientError::MismatchedRequestId {
                expected: "ping-1".to_owned(),
                actual: "other".to_owned(),
            }
        );
    }

    #[test]
    fn maps_missing_worker_before_spawning_python() {
        let path = missing_path("missing-worker");

        let error = match WorkerClient::spawn(python(), &path, Duration::from_secs(1)) {
            Ok(_) => panic!("missing worker must fail"),
            Err(error) => error,
        };

        assert_eq!(error, ClientError::MissingWorker(path));
    }

    #[test]
    fn maps_missing_wav_before_sending_request() {
        let path = missing_path("missing-audio");

        let error = WorkerRequest::transcribe("tx-1", &path, Some("pl".to_owned())).unwrap_err();

        assert_eq!(error, ClientError::MissingAudio(path));
    }

    #[test]
    fn maps_worker_timeout() {
        let path = fake_worker(
            "timeout-worker",
            "import sys, time\nfor line in sys.stdin:\n    time.sleep(1)\n",
        );
        let mut client = WorkerClient::spawn(python(), &path, Duration::from_millis(20)).unwrap();

        let error = client.ping().unwrap_err();

        assert!(matches!(error, ClientError::Timeout { .. }));
        assert_eq!(client.ping().unwrap_err(), ClientError::WorkerUnavailable);
        drop(client);
        fs::remove_file(path).unwrap();
    }

    #[test]
    fn maps_worker_crash() {
        let path = fake_worker("crash-worker", "raise SystemExit(7)\n");
        let mut client = WorkerClient::spawn(python(), &path, Duration::from_secs(2)).unwrap();

        let error = client.ping().unwrap_err();

        assert!(matches!(error, ClientError::Crashed { .. }));
        drop(client);
        fs::remove_file(path).unwrap();
    }

    #[test]
    fn subprocess_round_trips_utf8_request_id_message_and_path() {
        let worker = fake_worker(
            "utf8-worker",
            r#"import json
import sys

if "-X" not in sys.orig_argv or "utf8" not in sys.orig_argv:
    raise SystemExit(3)

for line in sys.stdin:
    request = json.loads(line)
    message = "Ścieżka: " + request["audio_path"]
    print(json.dumps({
        "request_id": request["request_id"],
        "ok": False,
        "error": {
            "code": "echo_path",
            "message": message,
            "retryable": False,
        },
    }, ensure_ascii=False), flush=True)
"#,
        );
        let audio_path = missing_path("zażółć-gęślą-jaźń").with_extension("wav");
        fs::write(&audio_path, b"not a real wav").unwrap();
        let request = WorkerRequest::transcribe("żądanie-ąęłóśźż", &audio_path, None).unwrap();
        let mut client = WorkerClient::spawn(python(), &worker, Duration::from_secs(2)).unwrap();

        let error = client.send::<TranscriptionResult>(request).unwrap_err();

        assert_eq!(
            error,
            ClientError::Worker(WorkerError {
                code: "echo_path".to_owned(),
                message: format!("Ścieżka: {}", audio_path.display()),
                retryable: false,
            })
        );
        drop(client);
        fs::remove_file(worker).unwrap();
        fs::remove_file(audio_path).unwrap();
    }

    #[test]
    fn public_result_types_match_protocol() {
        let load: LoadResult = parse_response(
            r#"{"request_id":"load-1","ok":true,"result":{"model":"nvidia/parakeet-tdt-0.6b-v3","device":"cuda"}}"#,
            "load-1",
        )
        .unwrap();
        let transcription: TranscriptionResult = parse_response(
            r#"{"request_id":"tx-1","ok":true,"result":{"text":"Mów.","model":"nvidia/parakeet-tdt-0.6b-v3","language":"pl","duration_ms":1250}}"#,
            "tx-1",
        )
        .unwrap();

        assert_eq!(load.device, "cuda");
        assert_eq!(transcription.text, "Mów.");
        assert_eq!(transcription.language.as_deref(), Some("pl"));
        assert_eq!(transcription.duration_ms, 1250);
    }
}
