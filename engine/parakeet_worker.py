#!/usr/bin/env python
"""Long-lived NVIDIA Parakeet JSONL worker.

The module intentionally imports torch and transformers only inside the default
runtime loader. A ping therefore remains fast and works before model setup.
"""

from __future__ import annotations

import json
import sys
from dataclasses import dataclass
from typing import Any, Callable, Protocol

try:
    from engine.protocol import (
        MODEL_ID,
        EngineInfo,
        handle_line,
    )
except ModuleNotFoundError as error:
    if error.name not in {"engine", "engine.protocol"}:
        raise
    from protocol import MODEL_ID, EngineInfo, handle_line


class RecognitionPipeline(Protocol):
    def __call__(self, audio: dict[str, Any], **kwargs: Any) -> Any: ...


@dataclass
class TransformersRuntime:
    recognizer: RecognitionPipeline
    model: str
    device: str

    def transcribe(
        self,
        audio: Any,
        sample_rate: int,
        language: str | None,
    ) -> str:
        options: dict[str, Any] = {}
        if language is not None:
            options["generate_kwargs"] = {"language": language}
        output = self.recognizer(
            {"raw": audio, "sampling_rate": sample_rate},
            **options,
        )
        if not isinstance(output, dict) or not isinstance(
            output.get("text"),
            str,
        ):
            raise RuntimeError("model returned an invalid transcription result")
        return output["text"]


RuntimeLoader = Callable[[str], TransformersRuntime]


def load_transformers_runtime(model_id: str) -> TransformersRuntime:
    """Load the real runtime; this is the only heavy-import boundary."""
    import torch
    from transformers import pipeline

    device = "cuda" if torch.cuda.is_available() else "cpu"
    recognizer = pipeline(
        "automatic-speech-recognition",
        model=model_id,
        device=0 if device == "cuda" else -1,
        dtype=torch.float16 if device == "cuda" else torch.float32,
    )
    return TransformersRuntime(
        recognizer=recognizer,
        model=model_id,
        device=device,
    )


class ParakeetEngine:
    """Lazy, cacheable adapter around the Transformers ASR pipeline."""

    def __init__(
        self,
        model_id: str = MODEL_ID,
        runtime_loader: RuntimeLoader = load_transformers_runtime,
    ):
        self._model_id = model_id
        self._runtime_loader = runtime_loader
        self._runtime: TransformersRuntime | None = None

    def load(self) -> EngineInfo:
        if self._runtime is None:
            self._runtime = self._runtime_loader(self._model_id)
        return EngineInfo(
            model=self._runtime.model,
            device=self._runtime.device,
        )

    def transcribe(
        self,
        audio: Any,
        sample_rate: int,
        language: str | None,
    ) -> str:
        self.load()
        if self._runtime is None:
            raise RuntimeError("model runtime is unavailable")
        return self._runtime.transcribe(audio, sample_rate, language)


def run_worker() -> None:
    engine = ParakeetEngine()
    for line in sys.stdin:
        if not line.strip():
            continue
        response = handle_line(line, engine)
        sys.stdout.write(
            json.dumps(response, ensure_ascii=False, separators=(",", ":"))
            + "\n"
        )
        sys.stdout.flush()


if __name__ == "__main__":
    run_worker()
