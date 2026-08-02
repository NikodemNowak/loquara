"""Download the exact model revision used by the Loquara worker."""

from __future__ import annotations

import json
from dataclasses import dataclass
from pathlib import Path
from typing import Callable

try:
    from engine.protocol import DEFAULT_MODEL_KEY, model_spec
except ModuleNotFoundError as error:
    if error.name not in {"engine", "engine.protocol"}:
        raise
    from protocol import DEFAULT_MODEL_KEY, model_spec


@dataclass(frozen=True)
class DownloadReport:
    path: Path
    bytes: int


def directory_size(path: Path) -> int:
    return sum(file.stat().st_size for file in path.rglob("*") if file.is_file())


def _has_nonempty(path: Path) -> bool:
    return path.is_file() and path.stat().st_size > 0


def validate_snapshot(path: Path) -> None:
    required_groups = (
        ("config.json",),
        ("processor_config.json", "preprocessor_config.json"),
        ("tokenizer.json", "tokenizer.model", "vocab.json"),
    )
    missing = [
        " / ".join(group)
        for group in required_groups
        if not any(_has_nonempty(path / name) for name in group)
    ]
    if not (
        _has_nonempty(path / "model.safetensors")
        or _has_nonempty(path / "pytorch_model.bin")
        or _has_nonempty(path / "model.safetensors.index.json")
        or _has_nonempty(path / "pytorch_model.bin.index.json")
    ):
        missing.append("model weights")
    if missing:
        raise RuntimeError(
            "model snapshot is incomplete: " + ", ".join(missing)
        )


def patch_cohere_modeling(snapshot: Path) -> bool:
    """Idempotently make the snapshot's Cohere modeling compatible with fp16.

    The custom modeling masks attention with a hard-coded ``-1e9`` which
    overflows fp16 on current transformers/torch. Rewrite those fills to a
    dtype-aware minimum so both fp16 and int8 loading keep working.
    """
    path = snapshot / "modeling_cohere_asr.py"
    if not path.is_file():
        return False
    text = path.read_text(encoding="utf-8")
    updated = text.replace(
        "scores.masked_fill(expanded_mask, -1e9)",
        "scores.masked_fill(expanded_mask, torch.finfo(scores.dtype).min)",
    )
    updated = updated.replace(
        ") * -1e9",
        ") * (torch.finfo(dtype).min)",
    )
    if updated == text:
        return False
    path.write_text(updated, encoding="utf-8")
    return True


def download_exact_model(
    downloader: Callable[..., str] | None = None,
    *,
    local_files_only: bool = False,
    model_key: str = DEFAULT_MODEL_KEY,
    local_dir: Path | None = None,
) -> DownloadReport:
    spec = model_spec(model_key)
    using_hf_downloader = downloader is None
    if downloader is None:
        from huggingface_hub import snapshot_download
        from tqdm.auto import tqdm

        downloader = snapshot_download
    kwargs = {"repo_id": spec.id, "revision": spec.revision}
    if using_hf_downloader:
        class JsonProgress(tqdm):
            def update(self, n=1):
                result = super().update(n)
                print(
                    json.dumps(
                        {
                            "event": "progress",
                            "downloaded_bytes": int(self.n),
                            "total_bytes": int(self.total) if self.total else None,
                        },
                        separators=(",", ":"),
                    ),
                    flush=True,
                )
                return result

        kwargs["tqdm_class"] = JsonProgress
        kwargs["max_workers"] = 1
        kwargs["ignore_patterns"] = [
            "*.nemo",
            "*.onnx",
            "*.md",
            "plots/*",
            ".eval_results/*",
            ".cache/*",
            "assets/*",
            "demo/*",
        ]
    if local_dir is not None:
        local_dir.mkdir(parents=True, exist_ok=True)
        kwargs["local_dir"] = str(local_dir / model_key / (spec.revision or "main"))
    if local_files_only:
        kwargs["local_files_only"] = True
    snapshot = Path(downloader(**kwargs)).resolve()
    validate_snapshot(snapshot)
    if spec.kind == "cohere":
        patch_cohere_modeling(snapshot)
    return DownloadReport(path=snapshot, bytes=directory_size(snapshot))


def main() -> None:
    import argparse

    parser = argparse.ArgumentParser()
    parser.add_argument("--local-only", action="store_true")
    parser.add_argument("--model", default=DEFAULT_MODEL_KEY)
    parser.add_argument("--local-dir", type=Path, default=None)
    args = parser.parse_args()
    spec = model_spec(args.model)
    report = download_exact_model(
        local_files_only=args.local_only,
        model_key=args.model,
        local_dir=args.local_dir,
    )
    print(f"Model: {spec.id}")
    print(f"Revision: {spec.revision}")
    print(f"Cache: {report.path}")
    print(f"Bytes: {report.bytes}")


if __name__ == "__main__":
    main()
