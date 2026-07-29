"""Download the exact model revision used by the Mów worker."""

from __future__ import annotations

from dataclasses import dataclass
from pathlib import Path
from typing import Callable

from engine.protocol import MODEL_ID, MODEL_REVISION


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


def download_exact_model(
    downloader: Callable[..., str] | None = None,
    *,
    local_files_only: bool = False,
) -> DownloadReport:
    if downloader is None:
        from huggingface_hub import snapshot_download

        downloader = snapshot_download
    kwargs = {"repo_id": MODEL_ID, "revision": MODEL_REVISION}
    if local_files_only:
        kwargs["local_files_only"] = True
    snapshot = Path(downloader(**kwargs)).resolve()
    validate_snapshot(snapshot)
    return DownloadReport(path=snapshot, bytes=directory_size(snapshot))


def main() -> None:
    import argparse

    parser = argparse.ArgumentParser()
    parser.add_argument("--local-only", action="store_true")
    args = parser.parse_args()
    report = download_exact_model(local_files_only=args.local_only)
    print(f"Model: {MODEL_ID}")
    print(f"Revision: {MODEL_REVISION}")
    print(f"Cache: {report.path}")
    print(f"Bytes: {report.bytes}")


if __name__ == "__main__":
    main()
