#!/usr/bin/env python
"""Long-lived local ASR JSONL worker (Parakeet, Whisper, Cohere).

The module intentionally imports torch and transformers only inside the
runtime loaders. A ping therefore remains fast and works before model setup.
"""

from __future__ import annotations

import json
import os
import sys
from dataclasses import dataclass
from typing import Any, Callable, Protocol

try:
    from engine.protocol import (
        DEFAULT_MODEL_KEY,
        EngineInfo,
        MissingDependencyError,
        ModelSpec,
        handle_line,
        model_spec,
    )
except ModuleNotFoundError as error:
    if error.name not in {"engine", "engine.protocol"}:
        raise
    from protocol import (
        DEFAULT_MODEL_KEY,
        EngineInfo,
        MissingDependencyError,
        ModelSpec,
        handle_line,
        model_spec,
    )


class RecognitionPipeline(Protocol):
    def __call__(self, audio: dict[str, Any], **kwargs: Any) -> Any: ...


@dataclass
class TransformersRuntime:
    recognizer: RecognitionPipeline
    model: str
    device: str
    chunk_length_s: float | None = None
    stride_length_s: float | None = None
    whisper: bool = False

    def transcribe(
        self,
        audio: Any,
        sample_rate: int,
        language: str | None,
    ) -> str:
        kwargs: dict[str, Any] = {"raw": audio, "sampling_rate": sample_rate}
        if language is not None and self.whisper:
            # Whisper accepts an explicit language and skips detection.
            kwargs["language"] = language
        call_kwargs: dict[str, Any] = {}
        if self.chunk_length_s:
            # Chunk long audio so Whisper never hits the >30 s mel-feature
            # limit (which forces timestamp generation and fails without
            # return_timestamps=True).
            call_kwargs["chunk_length_s"] = self.chunk_length_s
            call_kwargs["stride_length_s"] = self.stride_length_s
        output = self.recognizer(kwargs, **call_kwargs)
        if not isinstance(output, dict) or not isinstance(
            output.get("text"),
            str,
        ):
            raise RuntimeError("model returned an invalid transcription result")
        return output["text"]


@dataclass
class CohereRuntime:
    processor: Any
    model: Any
    device: str

    def transcribe(
        self,
        audio: Any,
        sample_rate: int,
        language: str | None,
    ) -> str:
        resolved_language = language or "pl"
        texts = self.model.transcribe(
            processor=self.processor,
            audio_arrays=[audio],
            sample_rates=[sample_rate],
            language=resolved_language,
        )
        if not isinstance(texts, list) or not texts or not isinstance(texts[0], str):
            raise RuntimeError("Cohere model returned an invalid result")
        return texts[0]


RuntimeLoader = Callable[[ModelSpec], TransformersRuntime | CohereRuntime]


def local_model_path(spec: ModelSpec) -> str | None:
    root = os.environ.get("LOQUARA_MODEL_HOME")
    if not root or not spec.revision:
        return None
    path = os.path.join(root, spec.key, spec.revision)
    return path if os.path.isdir(path) else None


def load_pipeline_runtime(spec: ModelSpec) -> TransformersRuntime:
    """Load a transformers ASR pipeline (Parakeet or Whisper)."""
    import torch
    from transformers import pipeline

    device = "cuda" if torch.cuda.is_available() else "cpu"
    kwargs: dict[str, Any] = {
        "model": local_model_path(spec) or spec.id,
        "device": 0 if device == "cuda" else -1,
        "dtype": torch.float16 if device == "cuda" else torch.float32,
    }
    if spec.revision:
        kwargs["revision"] = spec.revision
    recognizer = pipeline("automatic-speech-recognition", **kwargs)
    is_whisper = "whisper" in spec.id.lower()
    return TransformersRuntime(
        recognizer=recognizer,
        model=spec.id,
        device=device,
        chunk_length_s=30.0 if is_whisper else None,
        stride_length_s=5.0 if is_whisper else None,
        whisper=is_whisper,
    )


def _custom_cohere_modules(model_dir: str):
    """Import the snapshot's custom Cohere modeling + config.

    transformers ships its own native cohere_asr implementation that is
    incompatible with the 03-2026 checkpoint config, so this worker loads the
    modeling code that ships inside the downloaded snapshot instead.
    """
    import importlib.util

    pkg_name = "_loquara_cohere_remote"
    cfg_path = os.path.join(model_dir, "configuration_cohere_asr.py")
    model_path = os.path.join(model_dir, "modeling_cohere_asr.py")
    if not (os.path.isfile(cfg_path) and os.path.isfile(model_path)):
        raise RuntimeError(
            "Cohere snapshot is missing its custom modeling code files"
        )
    package = importlib.util.module_from_spec(
        importlib.util.spec_from_loader(pkg_name, loader=None)
    )
    package.__path__ = [model_dir]
    sys.modules[pkg_name] = package

    cfg_module = importlib.util.module_from_spec(
        importlib.util.spec_from_file_location(
            f"{pkg_name}.configuration_cohere_asr", cfg_path
        )
    )
    cfg_module.__loader__.exec_module(cfg_module)
    sys.modules[f"{pkg_name}.configuration_cohere_asr"] = cfg_module

    modeling_module = importlib.util.module_from_spec(
        importlib.util.spec_from_file_location(
            f"{pkg_name}.modeling_cohere_asr", model_path
        )
    )
    modeling_module.__loader__.exec_module(modeling_module)
    sys.modules[f"{pkg_name}.modeling_cohere_asr"] = modeling_module
    return cfg_module, modeling_module


def _prepare_cohere_class(model_cls: Any) -> Any:
    """Make the snapshot's custom Cohere class load on modern transformers.

    transformers >= 4.50 stopped giving PreTrainedModel a generate() and
    requires the ignore-key attributes to be sets instead of lists.
    """
    from transformers.generation.utils import GenerationMixin

    for attr in (
        "_keys_to_ignore_on_load_missing",
        "_keys_to_ignore_on_load_unexpected",
        "_keys_to_ignore_on_save",
    ):
        value = getattr(model_cls, attr, None)
        if isinstance(value, list):
            setattr(model_cls, attr, set(value))
    if not issubclass(model_cls, GenerationMixin):
        model_cls.__bases__ = (GenerationMixin, model_cls.__bases__[0])
    return model_cls


def load_cohere_runtime(spec: ModelSpec) -> CohereRuntime:
    """Load Cohere Transcribe via the snapshot's own modeling code.

    Uses the already-downloaded local snapshot (no HF authentication
    required) so generation works with the currently pinned transformers.
    """
    import torch
    from transformers import AutoProcessor

    model_path = local_model_path(spec)
    if model_path is None:
        raise MissingDependencyError(
            "Cohere model is not downloaded. Pobierz model w Ustawieniach."
        )
    processor = AutoProcessor.from_pretrained(model_path, trust_remote_code=True)
    cfg_module, modeling_module = _custom_cohere_modules(model_path)
    model_cls = _prepare_cohere_class(modeling_module.CohereAsrForConditionalGeneration)
    config = cfg_module.CohereAsrConfig.from_pretrained(model_path)

    load_kwargs: dict[str, Any] = {
        "config": config,
        "trust_remote_code": True,
        "device_map": "auto",
        "torch_dtype": torch.float16 if torch.cuda.is_available() else torch.float32,
    }
    model = model_cls.from_pretrained(model_path, **load_kwargs)
    return CohereRuntime(
        processor=processor,
        model=model,
        device="cuda" if torch.cuda.is_available() else "cpu",
    )


class AsrEngine:
    """Lazy, cacheable adapter around a selected local ASR model."""

    def __init__(
        self,
        default_key: str = DEFAULT_MODEL_KEY,
        loaders: dict[str, RuntimeLoader] | None = None,
    ):
        self._spec = model_spec(default_key)
        self._loaders = loaders or {
            "pipeline": load_pipeline_runtime,
            "cohere": load_cohere_runtime,
        }
        self._runtime: TransformersRuntime | CohereRuntime | None = None

    def set_model(self, spec: ModelSpec) -> None:
        if spec == self._spec and self._runtime is not None:
            return
        self._spec = spec
        self._runtime = None

    def kind(self) -> str:
        return self._spec.kind

    def load(self) -> EngineInfo:
        if self._runtime is None:
            loader = self._loaders.get(self._spec.kind)
            if loader is None:
                raise RuntimeError(
                    f"unsupported model kind: {self._spec.kind}"
                )
            self._runtime = loader(self._spec)
        assert self._runtime is not None
        return EngineInfo(
            model=self._spec.id,
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
    for stream in (sys.stdin, sys.stdout):
        reconfigure = getattr(stream, "reconfigure", None)
        if reconfigure is not None:
            reconfigure(encoding="utf-8", errors="strict")

    engine = AsrEngine()
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
