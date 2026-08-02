"""JSONL protocol and deterministic WAV preparation for the worker."""

from __future__ import annotations

import json
import wave
from dataclasses import asdict, dataclass
from pathlib import Path
from typing import Any, Protocol

import numpy as np
from numpy.typing import NDArray


MODEL_ID = "nvidia/parakeet-tdt-0.6b-v3"
MODEL_REVISION = "7c35754d166cca382ad1e53e68b01e7c575f3a1d"
SAMPLE_RATE = 16_000


@dataclass(frozen=True)
class ModelSpec:
    key: str
    id: str
    revision: str | None
    kind: str  # "pipeline" | "cohere"
    display: str
    min_vram_gb: float
    min_ram_gb: float
    languages: str


MODELS: dict[str, ModelSpec] = {
    "parakeet": ModelSpec(
        key="parakeet",
        id="nvidia/parakeet-tdt-0.6b-v3",
        revision="7c35754d166cca382ad1e53e68b01e7c575f3a1d",
        kind="pipeline",
        display="Parakeet TDT 0.6B v3",
        min_vram_gb=3.0,
        min_ram_gb=8.0,
        languages="auto (PL/EN)",
    ),
    "whisper-turbo": ModelSpec(
        key="whisper-turbo",
        id="openai/whisper-large-v3-turbo",
        revision="41f01f3fe87f28c78e2fbf8b568835947dd65ed9",
        kind="pipeline",
        display="Whisper Large v3 Turbo",
        min_vram_gb=4.0,
        min_ram_gb=8.0,
        languages="auto (99)",
    ),
    "whisper-small": ModelSpec(
        key="whisper-small",
        id="openai/whisper-small",
        revision="973afd24965f72e36ca33b3055d56a652f456b4d",
        kind="pipeline",
        display="Whisper Small",
        min_vram_gb=1.5,
        min_ram_gb=4.0,
        languages="auto (99)",
    ),
    "cohere": ModelSpec(
        key="cohere",
        id="AEmotionStudio/cohere-transcribe-03-2026-models",
        revision="d114f701a80b2150943f5dbae71458f4d1fcb37b",
        kind="cohere",
        display="Cohere Transcribe 2B",
        min_vram_gb=5.0,
        min_ram_gb=8.0,
        languages="pl/en/fr/de/...",
    ),
}

DEFAULT_MODEL_KEY = "parakeet"


def model_spec(key: str | None) -> ModelSpec:
    return MODELS.get(key or "", MODELS[DEFAULT_MODEL_KEY])


@dataclass(frozen=True)
class EngineInfo:
    model: str
    device: str


class TranscriptionEngine(Protocol):
    def set_model(self, spec: ModelSpec) -> None: ...

    def kind(self) -> str: ...

    def load(self) -> EngineInfo: ...

    def transcribe(
        self,
        audio: NDArray[np.float32],
        sample_rate: int,
        language: str | None,
    ) -> str: ...


class MissingDependencyError(RuntimeError):
    """A required local runtime dependency is unavailable."""


def _error(
    request_id: str | None,
    code: str,
    message: str,
    retryable: bool = False,
) -> dict[str, Any]:
    return {
        "request_id": request_id,
        "ok": False,
        "error": {
            "code": code,
            "message": message,
            "retryable": retryable,
        },
    }


def _success(request_id: str, result: dict[str, Any]) -> dict[str, Any]:
    return {"request_id": request_id, "ok": True, "result": result}


def handle_line(line: str, engine: TranscriptionEngine) -> dict[str, Any]:
    """Handle one request without allowing malformed input to stop the worker."""
    try:
        request = json.loads(line)
    except (json.JSONDecodeError, TypeError):
        return _error(
            None,
            "invalid_json",
            "request is not valid JSON",
        )

    if not isinstance(request, dict):
        return _error(None, "invalid_request", "request must be a JSON object")

    request_id = request.get("request_id")
    if not isinstance(request_id, str) or not request_id:
        return _error(
            None,
            "invalid_request",
            "request_id must be a non-empty string",
        )

    command = request.get("command")
    if not isinstance(command, str) or not command:
        return _error(
            request_id,
            "invalid_request",
            "command must be a non-empty string",
        )

    if command == "ping":
        return _success(request_id, {"status": "ready"})

    if command == "load":
        model_key = request.get("model")
        if model_key is not None and (not isinstance(model_key, str) or not model_key):
            return _error(
                request_id,
                "invalid_request",
                "model must be a non-empty string when provided",
            )
        try:
            engine.set_model(model_spec(model_key))
            info = engine.load()
        except Exception as error:  # The boundary must keep the process alive.
            return _error(request_id, "model_load_failed", str(error), True)
        return _success(request_id, asdict(info))

    if command == "transcribe":
        audio_path = request.get("audio_path")
        if not isinstance(audio_path, str) or not audio_path:
            return _error(
                request_id,
                "invalid_request",
                "audio_path must be a non-empty string",
            )

        path = Path(audio_path)
        if not path.is_file():
            return _error(
                request_id,
                "audio_not_found",
                f"audio file does not exist: {audio_path}",
            )

        language = request.get("language")
        if language is not None and (
            not isinstance(language, str) or not language
        ):
            return _error(
                request_id,
                "invalid_request",
                "language must be a non-empty string when provided",
            )
        if language is not None and engine.kind() == "pipeline":
            return _error(
                request_id,
                "unsupported_language_hint",
                (
                    "language hints are not supported for this model; "
                    "Parakeet and Whisper detect language automatically"
                ),
            )

        try:
            audio = read_wav_mono_16khz(path)
        except MissingDependencyError as error:
            return _error(request_id, "missing_dependency", str(error))
        except (OSError, ValueError, wave.Error) as error:
            return _error(request_id, "invalid_audio", str(error))

        try:
            info = engine.load()
            # ASR models often invent sentences for silence. Avoid showing
            # hallucinated text when the microphone captured no speech.
            if _is_effectively_silent(audio):
                text = ""
            else:
                text = engine.transcribe(audio, SAMPLE_RATE, language)
        except Exception as error:  # The boundary must keep the process alive.
            return _error(request_id, "transcription_failed", str(error), True)

        result: dict[str, Any] = {
            "text": text,
            "model": info.model,
            "duration_ms": round(len(audio) * 1000 / SAMPLE_RATE),
        }
        return _success(request_id, result)

    return _error(
        request_id,
        "unknown_command",
        f"unknown command: {command}",
    )


def _is_effectively_silent(audio: NDArray[np.float32]) -> bool:
    if audio.size == 0:
        return True
    peak = float(np.max(np.abs(audio)))
    rms = float(np.sqrt(np.mean(np.square(audio))))
    return peak < 0.02 and rms < 0.003


def _decode_pcm(frames: bytes, sample_width: int) -> NDArray[np.float32]:
    if sample_width == 1:
        samples = np.frombuffer(frames, dtype=np.uint8).astype(np.float32)
        return (samples - 128.0) / 128.0

    if sample_width == 2:
        samples = np.frombuffer(frames, dtype="<i2").astype(np.float32)
        return samples / 32768.0

    if sample_width == 3:
        octets = np.frombuffer(frames, dtype=np.uint8)
        if len(octets) % 3:
            raise ValueError("24-bit PCM data has an incomplete sample")
        triples = octets.reshape(-1, 3).astype(np.int32)
        samples = triples[:, 0] | (triples[:, 1] << 8) | (triples[:, 2] << 16)
        samples = np.where(samples & 0x800000, samples - 0x1000000, samples)
        return samples.astype(np.float32) / 8388608.0

    if sample_width == 4:
        samples = np.frombuffer(frames, dtype="<i4").astype(np.float64)
        return (samples / 2147483648.0).astype(np.float32)

    raise ValueError(f"unsupported PCM sample width: {sample_width} bytes")


def _resample_with_soxr(
    samples: NDArray[np.float32],
    source_rate: int,
) -> NDArray[np.float32]:
    try:
        import soxr
    except ImportError as error:
        raise MissingDependencyError(
            "Python package 'soxr' is required for audio resampling"
        ) from error

    resampled = soxr.resample(
        samples,
        source_rate,
        SAMPLE_RATE,
        quality="VHQ",
    )
    return np.asarray(resampled, dtype=np.float32)


def read_wav_mono_16khz(path: str | Path) -> NDArray[np.float32]:
    """Read PCM WAV, mix channels, and linearly resample to exactly 16 kHz."""
    with wave.open(str(path), "rb") as wav_file:
        if wav_file.getcomptype() != "NONE":
            raise ValueError("compressed WAV audio is not supported")
        channels = wav_file.getnchannels()
        source_rate = wav_file.getframerate()
        sample_width = wav_file.getsampwidth()
        frame_count = wav_file.getnframes()
        frames = wav_file.readframes(frame_count)

    if channels <= 0:
        raise ValueError("WAV must contain at least one channel")
    if source_rate <= 0:
        raise ValueError("WAV sample rate must be positive")

    samples = _decode_pcm(frames, sample_width)
    if len(samples) % channels:
        raise ValueError("WAV data has an incomplete channel frame")

    mono = samples.reshape(-1, channels).mean(axis=1, dtype=np.float32)
    if source_rate == SAMPLE_RATE or len(mono) == 0:
        return mono.astype(np.float32, copy=False)
    return _resample_with_soxr(mono, source_rate)
