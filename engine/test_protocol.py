import json
import os
import shutil
import struct
import subprocess
import sys
import tempfile
import time
import unittest
import wave
from pathlib import Path
from types import SimpleNamespace
from unittest.mock import patch

import numpy as np

from engine.protocol import (
    MODEL_ID,
    EngineInfo,
    handle_line,
    model_spec,
    read_wav_mono_16khz,
)


class ModelSpecTests(unittest.TestCase):
    def test_cohere_model_points_at_the_ungated_mirror(self):
        cohere = model_spec("cohere")

        self.assertEqual(cohere.kind, "cohere")
        self.assertEqual(cohere.id, "AEmotionStudio/cohere-transcribe-03-2026-models")
        self.assertEqual(cohere.revision, "d114f701a80b2150943f5dbae71458f4d1fcb37b")
        self.assertNotIn("cohere-8bit", model_spec("parakeet").key)

    def test_default_model_is_parakeet(self):
        self.assertEqual(model_spec(None).key, "parakeet")


class FakeEngine:
    def __init__(self):
        self.load_calls = 0
        self.transcribe_calls = []
        self.set_model_calls = []
        self.kind_value = "pipeline"

    def set_model(self, spec):
        self.set_model_calls.append(spec.key)

    def kind(self):
        return self.kind_value

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

    def test_transcribe_forwards_an_explicit_language_hint(self):
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
        self.assertEqual(self.engine.transcribe_calls[-1][2], "pl")

    def test_transcribe_omits_language_when_not_requested(self):
        with tempfile.TemporaryDirectory() as directory:
            wav_path = Path(directory) / "speech.wav"
            write_wav(wav_path, 2, 1, 16_000, struct.pack("<h", 0))

            response = handle_line(
                request("transcribe", audio_path=str(wav_path)),
                self.engine,
            )

        self.assertNotIn("language", response["result"])

    def test_transcribe_does_not_ask_asr_to_invent_text_for_silence(self):
        with tempfile.TemporaryDirectory() as directory:
            wav_path = Path(directory) / "silence.wav"
            write_wav(wav_path, 2, 1, 16_000, struct.pack("<16000h", *([0] * 16000)))

            response = handle_line(
                request("transcribe", audio_path=str(wav_path)),
                self.engine,
            )

        self.assertTrue(response["ok"])
        self.assertEqual(response["result"]["text"], "")
        self.assertEqual(self.engine.transcribe_calls, [])

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

    def test_default_loader_passes_exact_model_revision_without_downloading(self):
        from engine.parakeet_worker import load_pipeline_runtime
        from engine.protocol import model_spec

        calls = []

        def fake_pipeline(task, **options):
            calls.append((task, options))
            return lambda *_args, **_kwargs: {"text": ""}

        fake_torch = SimpleNamespace(
            cuda=SimpleNamespace(is_available=lambda: False),
            float16=object(),
            float32=object(),
        )
        fake_transformers = SimpleNamespace(pipeline=fake_pipeline)
        with patch.dict(
            sys.modules,
            {"torch": fake_torch, "transformers": fake_transformers},
        ):
            runtime = load_pipeline_runtime(model_spec("parakeet"))

        self.assertEqual(runtime.model, MODEL_ID)
        self.assertEqual(
            calls[0][1]["revision"],
            "7c35754d166cca382ad1e53e68b01e7c575f3a1d",
        )

    def test_requirements_use_exact_reproducible_versions_without_torch(self):
        requirements = (
            Path(__file__).with_name("requirements.txt").read_text(
                encoding="utf-8"
            )
        )
        packages = {
            line
            for line in requirements.splitlines()
            if line and not line.startswith("#")
        }

        self.assertEqual(
            packages,
            {
                "numpy==2.4.4",
                "transformers==5.14.1",
                "safetensors==0.8.0",
                "huggingface_hub==1.24.0",
                "soxr==1.1.0",
                "librosa==0.11.0",
                "accelerate==1.14.0",
                "sentencepiece==0.2.2",
                "bitsandbytes==0.50.0",
            },
        )
        self.assertFalse(
            any(package.lower().startswith("torch") for package in packages)
        )

    def test_missing_soxr_returns_structured_dependency_error(self):
        with tempfile.TemporaryDirectory() as directory:
            wav_path = Path(directory) / "high-rate.wav"
            write_wav(
                wav_path,
                sample_width=2,
                channels=1,
                sample_rate=48_000,
                frames=struct.pack("<480h", *([0] * 480)),
            )

            with patch.dict(sys.modules, {"soxr": None}):
                response = handle_line(
                    request("transcribe", audio_path=str(wav_path)),
                    self.engine,
                )

        self.assertEqual(
            response["error"],
            {
                "code": "missing_dependency",
                "message": (
                    "Python package 'soxr' is required for audio resampling"
                ),
                "retryable": False,
            },
        )
        self.assertEqual(self.engine.load_calls, 0)


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
        self.assertTrue(np.isfinite(first).all())

    def test_downsampling_strongly_suppresses_above_nyquist_tone(self):
        sample_rate = 48_000
        positions = np.arange(sample_rate, dtype=np.float64)
        tone = 0.8 * np.sin(2 * np.pi * 12_000 * positions / sample_rate)
        frames = np.round(tone * 32767).astype("<i2").tobytes()

        audio = self.read_samples(
            2,
            frames,
            sample_rate=sample_rate,
        )

        steady_state = audio[256:-256]
        rms = float(np.sqrt(np.mean(np.square(steady_state))))
        self.assertLess(rms, 0.02)

    def test_downsampling_preserves_in_band_tone(self):
        sample_rate = 48_000
        positions = np.arange(sample_rate, dtype=np.float64)
        tone = 0.8 * np.sin(2 * np.pi * 1_000 * positions / sample_rate)
        frames = np.round(tone * 32767).astype("<i2").tobytes()

        audio = self.read_samples(
            2,
            frames,
            sample_rate=sample_rate,
        )

        steady_state = audio[256:-256]
        rms = float(np.sqrt(np.mean(np.square(steady_state))))
        self.assertGreater(rms, 0.5)

    def test_high_rate_downsampling_preserves_passband_and_rejects_stopband(
        self,
    ):
        reference_rms = 0.8 / np.sqrt(2)

        for sample_rate in (48_000, 96_000, 192_000):
            measurements = {}
            positions = np.arange(sample_rate, dtype=np.float64)
            for frequency in (7_500, 8_500):
                tone = 0.8 * np.sin(
                    2 * np.pi * frequency * positions / sample_rate
                )
                frames = np.round(tone * 32767).astype("<i2").tobytes()
                audio = self.read_samples(
                    2,
                    frames,
                    sample_rate=sample_rate,
                )
                steady_state = audio[512:-512]
                measurements[frequency] = float(
                    np.sqrt(np.mean(np.square(steady_state)))
                )

            passband_ratio = measurements[7_500] / reference_rms
            stopband_ratio = measurements[8_500] / reference_rms
            with self.subTest(sample_rate=sample_rate, band="passband"):
                self.assertGreaterEqual(
                    passband_ratio,
                    0.80,
                    f"7.5 kHz RMS ratio was {passband_ratio:.6f}",
                )
            with self.subTest(sample_rate=sample_rate, band="stopband"):
                self.assertLessEqual(
                    stopband_ratio,
                    0.05,
                    f"8.5 kHz RMS ratio was {stopband_ratio:.6f}",
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

    def test_worker_forces_utf8_for_polish_jsonl_and_path(self):
        repository = Path(__file__).resolve().parent.parent
        worker = repository / "engine" / "parakeet_worker.py"
        with tempfile.TemporaryDirectory() as directory:
            invalid_wav = Path(directory) / "dźwięk-ąęłóśźż.wav"
            invalid_wav.write_bytes(b"not a wav")
            payload = request(
                "transcribe",
                request_id="żądanie-ąęłóśźż",
                audio_path=str(invalid_wav),
            )
            environment = os.environ.copy()
            environment["PYTHONIOENCODING"] = "cp1250"

            process = subprocess.run(
                [sys.executable, "-u", str(worker)],
                input=(payload + "\n").encode("utf-8"),
                capture_output=True,
                cwd=repository,
                env=environment,
                timeout=10,
                check=False,
            )

        self.assertEqual(process.returncode, 0, process.stderr)
        self.assertIn("żądanie-ąęłóśźż".encode(), process.stdout)
        response = json.loads(process.stdout.decode("utf-8"))
        self.assertEqual(response["request_id"], "żądanie-ąęłóśźż")
        self.assertEqual(response["error"]["code"], "invalid_audio")

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

    def test_setup_times_out_and_kills_hanging_worker_in_all_hosts(self):
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

        with tempfile.TemporaryDirectory() as directory:
            for index, host in enumerate(hosts):
                worker = Path(directory) / f"hanging-worker-{index}.py"
                pid_path = worker.with_suffix(".pid")
                worker.write_text(
                    "import os, pathlib, sys, time\n"
                    "pathlib.Path(__file__).with_suffix('.pid').write_text("
                    "str(os.getpid()), encoding='ascii')\n"
                    "sys.stdin.buffer.read(1)\n"
                    "time.sleep(60)\n",
                    encoding="utf-8",
                )
                started = time.monotonic()

                process = subprocess.run(
                    [
                        host,
                        "-NoProfile",
                        "-ExecutionPolicy",
                        "Bypass",
                        "-File",
                        str(script),
                        "-SkipInstall",
                        "-WorkerPath",
                        str(worker),
                        "-PingTimeoutSeconds",
                        "1",
                    ],
                    text=True,
                    capture_output=True,
                    cwd=repository,
                    timeout=15,
                    check=False,
                )
                elapsed = time.monotonic() - started

                self.assertNotEqual(process.returncode, 0)
                self.assertLess(elapsed, 8)
                self.assertIn(
                    "timed out",
                    (process.stdout + process.stderr).lower(),
                )
                self.assertTrue(pid_path.is_file())
                worker_pid = int(pid_path.read_text(encoding="ascii"))
                with self.assertRaises(OSError):
                    os.kill(worker_pid, 0)

    def test_setup_uses_mow_python_when_path_has_no_python(self):
        repository = Path(__file__).resolve().parent.parent
        script = repository / "scripts" / "setup-engine.ps1"
        hosts = list(
            dict.fromkeys(
                host
                for executable in ("pwsh", "powershell.exe")
                if (host := shutil.which(executable)) is not None
            )
        )
        self.assertTrue(hosts)
        environment = os.environ.copy()
        environment["PATH"] = ""
        environment["MOW_PYTHON"] = sys.executable

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
                    env=environment,
                    timeout=30,
                    check=False,
                )

                self.assertEqual(process.returncode, 0, process.stderr)
                self.assertIn("Parakeet worker ping: ready", process.stdout)


if __name__ == "__main__":
    unittest.main()
