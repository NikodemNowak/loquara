import tempfile
import unittest
from pathlib import Path

from engine.download_model import (
    directory_size,
    download_exact_model,
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
        self.assertEqual(report.path, snapshot)
        self.assertEqual(report.bytes, 4)

    def test_directory_size_counts_nested_files(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / "one.bin").write_bytes(b"12")
            (root / "nested").mkdir()
            (root / "nested" / "two.bin").write_bytes(b"345")

            self.assertEqual(directory_size(root), 5)

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


if __name__ == "__main__":
    unittest.main()
