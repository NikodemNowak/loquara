import tempfile
import unittest
from pathlib import Path
from unittest import mock

from engine.download_model import (
    ModelAccessError,
    classify_access_error,
    directory_size,
    download_exact_model,
    patch_cohere_modeling,
    validate_snapshot,
)
from engine.protocol import MODEL_ID, MODEL_REVISION


class DownloadModelTests(unittest.TestCase):
    def test_download_uses_engine_model_and_exact_revision(self):
        with tempfile.TemporaryDirectory() as directory:
            snapshot = Path(directory)
            for name in (
                "config.json",
                "preprocessor_config.json",
                "tokenizer.json",
                "model.safetensors",
            ):
                (snapshot / name).write_bytes(b"x")
            calls = []

            def downloader(**kwargs):
                calls.append(kwargs)
                return str(snapshot)

            report = download_exact_model(downloader=downloader)

        self.assertEqual(
            calls,
            [
                {
                    "repo_id": MODEL_ID,
                    "revision": MODEL_REVISION,
                }
            ],
        )
        # Compare against the canonical path: on Windows CI the temp dir is
        # reported with a short (8.3) name while resolve() expands it.
        self.assertEqual(report.path, snapshot.resolve())
        self.assertEqual(report.bytes, 4)

    def test_directory_size_counts_nested_files(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / "one.bin").write_bytes(b"12")
            (root / "nested").mkdir()
            (root / "nested" / "two.bin").write_bytes(b"345")

            self.assertEqual(directory_size(root), 5)

    def test_local_dir_is_passed_as_a_regular_model_destination(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            snapshot = root / "parakeet" / MODEL_REVISION
            snapshot.mkdir(parents=True)
            for name in (
                "config.json",
                "preprocessor_config.json",
                "tokenizer.json",
                "model.safetensors",
            ):
                (snapshot / name).write_bytes(b"x")
            calls = []

            def downloader(**kwargs):
                calls.append(kwargs)
                return str(snapshot)

            download_exact_model(
                downloader=downloader,
                local_dir=root,
                model_key="parakeet",
            )

        self.assertEqual(calls[0]["local_dir"], str(root / "parakeet" / MODEL_REVISION))

    def test_snapshot_validation_requires_runtime_artifacts(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            for name in (
                "config.json",
                "preprocessor_config.json",
                "tokenizer.json",
                "model.safetensors",
            ):
                (root / name).write_text("{}", encoding="utf-8")

            validate_snapshot(root)
            (root / "config.json").unlink()

            with self.assertRaisesRegex(RuntimeError, "config.json"):
                validate_snapshot(root)

    def test_real_downloader_skips_unused_nemo_weights(self):
        with tempfile.TemporaryDirectory() as directory:
            snapshot = Path(directory)
            for name in (
                "config.json",
                "preprocessor_config.json",
                "tokenizer.json",
                "model.safetensors",
            ):
                (snapshot / name).write_bytes(b"x")
            calls: list = []

            def fake_snapshot_download(**kwargs):
                calls.append(kwargs)
                return str(snapshot)

            with mock.patch(
                "huggingface_hub.snapshot_download",
                side_effect=fake_snapshot_download,
            ):
                report = download_exact_model()

            kwargs = calls[0]
            self.assertIn("*.nemo", kwargs["ignore_patterns"])
            self.assertIn("*.md", kwargs["ignore_patterns"])
            self.assertEqual(report.bytes, 4)

    def test_patch_cohere_modeling_makes_fp16_fills_dtype_aware_and_idempotent(self):
        with tempfile.TemporaryDirectory() as directory:
            modeling = Path(directory) / "modeling_cohere_asr.py"
            modeling.write_text(
                "scores = scores.masked_fill(expanded_mask, -1e9)\n"
                "key_padding = (1.0 - effective_decoder_mask[:, None, None, :]"
                ".to(dtype=dtype)) * -1e9\n",
                encoding="utf-8",
            )

            self.assertTrue(patch_cohere_modeling(Path(directory)))
            text = modeling.read_text(encoding="utf-8")
            self.assertNotIn("-1e9", text)
            self.assertIn("torch.finfo(scores.dtype).min", text)
            self.assertIn("torch.finfo(dtype).min", text)
            self.assertFalse(patch_cohere_modeling(Path(directory)))
            self.assertFalse(patch_cohere_modeling(Path(directory) / "missing"))

    def test_patch_cohere_modeling_ignores_other_models(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / "config.json").write_text("{}", encoding="utf-8")
            self.assertFalse(patch_cohere_modeling(root))


class _Response:
    def __init__(self, status_code):
        self.status_code = status_code


class _HttpError(Exception):
    def __init__(self, status_code, message="denied"):
        super().__init__(message)
        self.response = _Response(status_code)


class GatedRepoError(Exception):
    """Stands in for the huggingface_hub exception, which is matched by name."""


class RepositoryNotFoundError(Exception):
    pass


class AccessErrorTests(unittest.TestCase):
    def test_gated_repository_is_reported_as_needing_a_licence(self):
        error = classify_access_error(GatedRepoError("access denied"), "acme/model")

        self.assertIsNotNone(error)
        self.assertEqual(error.kind, "gated")
        self.assertEqual(error.repo, "acme/model")

    def test_forbidden_means_the_licence_was_never_accepted(self):
        error = classify_access_error(_HttpError(403), "acme/model")

        self.assertEqual(error.kind, "gated")

    def test_unauthorized_means_the_token_is_missing_or_wrong(self):
        error = classify_access_error(_HttpError(401), "acme/model")

        self.assertEqual(error.kind, "unauthorized")

    def test_a_repository_that_looks_missing_is_treated_as_no_access(self):
        # Hugging Face returns 404 for private repos to callers without a
        # token, so "not found" must not be reported as a broken model id.
        error = classify_access_error(RepositoryNotFoundError("nope"), "acme/model")

        self.assertEqual(error.kind, "unauthorized")

    def test_unrelated_failures_keep_their_own_message(self):
        self.assertIsNone(classify_access_error(OSError("no space left"), "acme/model"))
        self.assertIsNone(classify_access_error(_HttpError(500), "acme/model"))

    def test_download_surfaces_a_gated_repository_instead_of_the_raw_exception(self):
        def refuse(**_kwargs):
            raise GatedRepoError("you must accept the licence")

        with self.assertRaises(ModelAccessError) as raised:
            download_exact_model(downloader=refuse, model_key="cohere")

        self.assertEqual(raised.exception.kind, "gated")

    def test_download_does_not_swallow_ordinary_failures(self):
        def explode(**_kwargs):
            raise OSError("no space left on device")

        with self.assertRaises(OSError):
            download_exact_model(downloader=explode, model_key="cohere")


if __name__ == "__main__":
    unittest.main()
