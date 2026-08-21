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
        try:
            from huggingface_hub import snapshot_download
            from tqdm.auto import tqdm
        except ImportError as error:
            # Loquara ships without the Python engine, so this is the normal
            # state of a fresh machine rather than a broken install.
            raise ModelAccessError("engine", spec.id, str(error)) from error

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
    try:
        snapshot = Path(downloader(**kwargs)).resolve()
    except Exception as error:  # noqa: BLE001 - re-raised unless recognised
        access_error = classify_access_error(error, spec.id)
        if access_error is None:
            raise
        raise access_error from error
    validate_snapshot(snapshot)
    if spec.kind == "cohere":
        patch_cohere_modeling(snapshot)
    return DownloadReport(path=snapshot, bytes=directory_size(snapshot))


class ModelAccessError(Exception):
    """A download that failed for a reason the user can actually act on.

    ``kind`` is one of:

    ``gated``
        The repository exists but requires accepting its licence with a
        Hugging Face account. A token alone is not enough.
    ``unauthorized``
        No token was supplied, or the one supplied is invalid or expired.
    ``engine``
        The Python dependencies this script needs are not installed, so no
        download could even be attempted.
    """

    def __init__(self, kind: str, repo: str, detail: str = "") -> None:
        super().__init__(detail or kind)
        self.kind = kind
        self.repo = repo
        self.detail = detail


def _status_code(error: BaseException) -> int | None:
    response = getattr(error, "response", None)
    return getattr(response, "status_code", None)


def classify_access_error(error: BaseException, repo: str) -> ModelAccessError | None:
    """Maps a huggingface_hub exception onto something explainable.

    Returns ``None`` for anything that is not an access problem, so genuine
    faults (no disk space, no network) keep their original message.
    """
    name = type(error).__name__
    if name == "GatedRepoError":
        return ModelAccessError("gated", repo, str(error))
    status = _status_code(error)
    if name == "RepositoryNotFoundError" or status == 404:
        # A private or gated repo is indistinguishable from a missing one
        # when the caller is not authorised, and the actionable reading is
        # that the user needs access.
        return ModelAccessError("unauthorized", repo, str(error))
    if status in (401, 403):
        return ModelAccessError("gated" if status == 403 else "unauthorized", repo, str(error))
    return None


def verify_token() -> str:
    """Returns the account name for the token in the environment.

    Raises ``ModelAccessError('unauthorized', ...)`` when the token is missing
    or rejected, so the caller can tell "wrong token" from "no network", and
    ``ModelAccessError('engine', ...)`` when the dependencies are absent.
    """
    try:
        from huggingface_hub import HfApi
        from huggingface_hub.utils import HfHubHTTPError
    except ImportError as error:
        raise ModelAccessError("engine", "", str(error)) from error

    try:
        identity = HfApi().whoami()
    except HfHubHTTPError as error:
        if _status_code(error) in (401, 403):
            raise ModelAccessError("unauthorized", "", str(error)) from error
        raise
    return identity.get("name") or identity.get("fullname") or ""


def main() -> None:
    import argparse

    import sys

    parser = argparse.ArgumentParser()
    parser.add_argument("--local-only", action="store_true")
    parser.add_argument("--model", default=DEFAULT_MODEL_KEY)
    parser.add_argument("--local-dir", type=Path, default=None)
    parser.add_argument(
        "--verify-token",
        action="store_true",
        help="Check the HF_TOKEN in the environment and print the account name.",
    )
    args = parser.parse_args()

    def emit_access_error(error: ModelAccessError) -> None:
        # On stdout, alongside progress, so the caller parses one stream.
        print(
            json.dumps(
                {"event": "error", "kind": error.kind, "repo": error.repo},
                separators=(",", ":"),
            ),
            flush=True,
        )
        print(error.detail, file=sys.stderr)

    if args.verify_token:
        try:
            name = verify_token()
        except ModelAccessError as error:
            emit_access_error(error)
            raise SystemExit(1)
        print(json.dumps({"event": "whoami", "name": name}, separators=(",", ":")), flush=True)
        return

    spec = model_spec(args.model)
    try:
        report = download_exact_model(
            local_files_only=args.local_only,
            model_key=args.model,
            local_dir=args.local_dir,
        )
    except ModelAccessError as error:
        emit_access_error(error)
        raise SystemExit(1)
    print(f"Model: {spec.id}")
    print(f"Revision: {spec.revision}")
    print(f"Cache: {report.path}")
    print(f"Bytes: {report.bytes}")


if __name__ == "__main__":
    main()
