import json
import os
import shutil
import struct
import subprocess
import sys
import tempfile
import unittest
import wave
from pathlib import Path

import numpy as np

from engine.protocol import (
    MODEL_ID,
    EngineInfo,
    handle_line,
    read_wav_mono_16khz,
)


class FakeEngine:
    def __init__(self):
        self.load_calls = 0
        self.transcribe_calls = []

    def load(self):
        self.load_calls += 1
        return EngineInfo(model="fake/parakeet", device="cpu")

    def transcribe(self, audio, sample_rate, language):
        self.transcribe_calls.append((audio, sample_rate, language))
        return "Zażółć gęślą jaźń."


def request(command, request_id="req-1", **arguments):
    return json.dumps(
        {"request_id": request_id, "command": command, **arguments},
        ensure_ascii=False,
    )


def write_wav(path, sample_width, channels, sample_rate, frames):
    with wave.open(str(path), "wb") as wav_file:
        wav_file.setnchannels(channels)
        wav_file.setsampwidth(sample_width)
        wav_file.setframerate(sample_rate)
        wav_file.writeframes(frames)


def pack_pcm24(values):
    output = bytearray()
    for value in values:
        unsigned = value if value >= 0 else value + (1 << 24)
        output.extend(
            (unsigned & 0xFF, (unsigned >> 8) & 0xFF, (unsigned >> 16) & 0xFF)
        )
    return bytes(output)


class ProtocolTests(unittest.TestCase):
    def setUp(self):
        self.engine = FakeEngine()

    def test_ping_is_ready_without_loading_the_model(self):
        response = handle_line(request("ping"), self.engine)

        self.assertEqual(
            response,
            {
                "request_id": "req-1",
                "ok": True,
                "result": {"status": "ready"},
            },
        )
        self.assertEqual(self.engine.load_calls, 0)

    def test_load_returns_model_and_device_from_adapter(self):
        response = handle_line(request("load"), self.engine)

        self.assertEqual(
            response["result"],
            {"model": "fake/parakeet", "device": "cpu"},
        )
        self.assertEqual(self.engine.load_calls, 1)

    def test_transcribe_reads_wav_and_passes_optional_language(self):
        with tempfile.TemporaryDirectory() as directory:
            wav_path = Path(directory) / "speech.wav"
            write_wav(
                wav_path,
                sample_width=2,
                channels=1,
                sample_rate=16_000,
                frames=struct.pack("<4h", -32768, -16384, 0, 32767),
            )

            response = handle_line(
                request("transcribe", audio_path=str(wav_path), language="pl"),
                self.engine,
            )

        self.assertTrue(response["ok"])
        self.assertEqual(
            response["result"],
            {
                "text": "Zażółć gęślą jaźń.",
                "model": "fake/parakeet",
                "language": "pl",
                "duration_ms": 0,
            },
        )
        audio, sample_rate, language = self.engine.transcribe_calls[0]
        np.testing.assert_allclose(audio, [-1.0, -0.5, 0.0, 32767 / 32768])
        self.assertEqual(sample_rate, 16_000)
        self.assertEqual(language, "pl")

    def test_transcribe_omits_language_when_not_requested(self):
        with tempfile.TemporaryDirectory() as directory:
            wav_path = Path(directory) / "speech.wav"
            write_wav(wav_path, 2, 1, 16_000, struct.pack("<h", 0))

            response = handle_line(
                request("transcribe", audio_path=str(wav_path)),
                self.engine,
            )

        self.assertNotIn("language", response["result"])

    def test_invalid_json_returns_error_and_does_not_raise(self):
        response = handle_line("{broken", self.engine)

        self.assertEqual(response["request_id"], None)
        self.assertEqual(
            response["error"],
            {
                "code": "invalid_json",
                "message": "request is not valid JSON",
                "retryable": False,
            },
        )

    def test_unknown_command_returns_error_with_request_id(self):
        response = handle_line(request("explode"), self.engine)

        self.assertEqual(response["request_id"], "req-1")
        self.assertFalse(response["ok"])
        self.assertEqual(response["error"]["code"], "unknown_command")
        self.assertFalse(response["error"]["retryable"])

    def test_missing_request_id_is_invalid_request(self):
        response = handle_line(json.dumps({"command": "ping"}), self.engine)

        self.assertEqual(response["request_id"], None)
        self.assertEqual(response["error"]["code"], "invalid_request")

    def test_missing_wav_is_reported_before_loading_model(self):
        response = handle_line(
            request("transcribe", audio_path="does-not-exist.wav"),
            self.engine,
        )

        self.assertEqual(response["error"]["code"], "audio_not_found")
        self.assertFalse(response["error"]["retryable"])
        self.assertEqual(self.engine.load_calls, 0)
        self.assertEqual(self.engine.transcribe_calls, [])

    def test_model_identity_constant_is_the_requested_nvidia_model(self):
        self.assertEqual(MODEL_ID, "nvidia/parakeet-tdt-0.6b-v3")


class WavConversionTests(unittest.TestCase):
    def read_samples(
        self,
        sample_width,
        frames,
        *,
        channels=1,
        sample_rate=16_000,
    ):
        with tempfile.TemporaryDirectory() as directory:
            wav_path = Path(directory) / "audio.wav"
            write_wav(
                wav_path,
                sample_width,
                channels,
                sample_rate,
                frames,
            )
            return read_wav_mono_16khz(wav_path)

    def test_normalizes_unsigned_8_bit_pcm(self):
        audio = self.read_samples(1, bytes([0, 128, 255]))

        np.testing.assert_allclose(audio, [-1.0, 0.0, 127 / 128])

    def test_normalizes_signed_16_bit_pcm(self):
        audio = self.read_samples(
            2,
            struct.pack("<4h", -32768, -16384, 0, 32767),
        )

        np.testing.assert_allclose(audio, [-1.0, -0.5, 0.0, 32767 / 32768])

    def test_normalizes_signed_24_bit_pcm(self):
        audio = self.read_samples(
            3,
            pack_pcm24([-8388608, -4194304, 0, 8388607]),
        )

        np.testing.assert_allclose(
            audio,
            [-1.0, -0.5, 0.0, 8388607 / 8388608],
        )

    def test_normalizes_signed_32_bit_pcm(self):
        audio = self.read_samples(
            4,
            struct.pack("<4i", -2147483648, -1073741824, 0, 2147483647),
        )

        np.testing.assert_allclose(
            audio,
            [-1.0, -0.5, 0.0, 2147483647 / 2147483648],
        )

    def test_mixes_all_channels_to_mono(self):
        audio = self.read_samples(
            2,
            struct.pack("<4h", 32767, -32768, 16384, 16384),
            channels=2,
        )

        np.testing.assert_allclose(
            audio,
            [(-1 / 32768) / 2, 0.5],
        )

    def test_resamples_to_16khz_deterministically(self):
        source = struct.pack("<4h", 0, 32767, 0, -32768)

        first = self.read_samples(2, source, sample_rate=8_000)
        second = self.read_samples(2, source, sample_rate=8_000)

        self.assertEqual(len(first), 8)
        np.testing.assert_array_equal(first, second)
        np.testing.assert_allclose(
            first,
            [0.0, 0.49998474, 0.9999695, 0.49998474, 0.0, -0.5, -1.0, -1.0],
            rtol=1e-6,
            atol=1e-6,
        )


class WorkerProcessTests(unittest.TestCase):
    def test_worker_ping_stays_alive_without_importing_model_dependencies(self):
        repository = Path(__file__).resolve().parent.parent
        worker = repository / "engine" / "parakeet_worker.py"
        with tempfile.TemporaryDirectory() as directory:
            trap_directory = Path(directory)
            trap = "raise RuntimeError('heavy dependency imported during ping')\n"
            (trap_directory / "torch.py").write_text(trap, encoding="utf-8")
            (trap_directory / "transformers.py").write_text(
                trap,
                encoding="utf-8",
            )
            environment = os.environ.copy()
            environment["PYTHONPATH"] = os.pathsep.join(
                [str(trap_directory), str(repository)]
            )

            process = subprocess.run(
                [sys.executable, "-u", str(worker)],
                input="\n".join(
                    [
                        request("ping", request_id="ping-1"),
                        "{broken",
                        request("explode", request_id="bad-1"),
                        "",
                    ]
                ),
                text=True,
                capture_output=True,
                cwd=repository,
                env=environment,
                timeout=10,
                check=False,
            )

        self.assertEqual(process.returncode, 0, process.stderr)
        responses = [
            json.loads(line) for line in process.stdout.splitlines() if line
        ]
        self.assertEqual(len(responses), 3)
        self.assertEqual(responses[0]["result"], {"status": "ready"})
        self.assertEqual(responses[1]["error"]["code"], "invalid_json")
        self.assertEqual(responses[2]["error"]["code"], "unknown_command")
        self.assertEqual(process.stderr, "")

    def test_setup_ping_works_in_available_powershell_hosts(self):
        repository = Path(__file__).resolve().parent.parent
        script = repository / "scripts" / "setup-engine.ps1"
        hosts = list(
            dict.fromkeys(
                host
                for executable in ("pwsh", "powershell.exe")
                if (host := shutil.which(executable)) is not None
            )
        )
        self.assertTrue(hosts, "PowerShell is required to test engine setup")

        for host in hosts:
            with self.subTest(host=host):
                process = subprocess.run(
                    [
                        host,
                        "-NoProfile",
                        "-ExecutionPolicy",
                        "Bypass",
                        "-File",
                        str(script),
                        "-SkipInstall",
                    ],
                    text=True,
                    capture_output=True,
                    cwd=repository,
                    timeout=30,
                    check=False,
                )

                self.assertEqual(process.returncode, 0, process.stderr)
                self.assertIn("Parakeet worker ping: ready", process.stdout)


if __name__ == "__main__":
    unittest.main()
