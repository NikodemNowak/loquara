# Loquara

Loquara is a local, open-source dictation app for Windows (Linux builds run in CI; macOS is not yet supported). Hold a global shortcut, speak, and get a transcription from a local Whisper, Parakeet, or Cohere model running on your GPU — optionally pasted straight into the active app.

## Highlights

- global shortcut `Ctrl+Space` starts and stops recording, `Esc` cancels;
- a minimal floating pill shows a live level meter while dictating — nothing else;
- distinct sound cues mark recording start, stop, and transcript-ready;
- system tray: start recording, paste the last transcript, open the main window;
- history keeps audio and text; a custom vocabulary and modes refine results;
- shortcut capture: click the shortcut field and press the new combo to set it;
- a dark interface, tuned for long dictation sessions;
- Polish and English UI — follows the OS language (Polish on Polish systems, English otherwise), switchable anytime in settings;
- **everything runs locally** — audio never leaves the machine.

## Privacy and architecture

The React UI runs in Tauri. Rust captures audio, stores SQLite, and handles shortcuts, tray, and clipboard. A long-lived Python worker loads `nvidia/parakeet-tdt-0.6b-v3` at the pinned revision `7c35754d166cca382ad1e53e68b01e7c575f3a1d` via Transformers/PyTorch CUDA. After a one-time model download, transcription sends nothing to the cloud.

## Requirements

- Windows 10/11 x64 (primary) or Linux x64 (via CI);
- Node.js, `pnpm`, and a Rust toolchain matching `rust-version` (for building);
- Python 3.10+ (tested with 3.13);
- PyTorch with CUDA installed separately, matching your NVIDIA driver;
- at least 6 GB of free space for the model in the Hugging Face cache.

`engine/requirements.txt` deliberately omits PyTorch so it never replaces your optimized CUDA build.

## Engine and model setup

In PowerShell (or bash on macOS/Linux):

```powershell
.\scripts\setup-engine.ps1
.\scripts\download-model.ps1
```

`setup-engine.ps1` installs the pinned lightweight dependencies, checks CUDA, the GPU name, and pings the worker. `download-model.ps1` downloads the pinned revision into the standard Hugging Face cache; it is idempotent and reports the path and byte count. If Python is not on `PATH`, set `MOW_PYTHON` to the full path of `python.exe`.

## Run and build

```powershell
pnpm install
.\scripts\run-dev.ps1
pnpm tauri build
```

`run-dev.ps1` options: `-SetupEngine`, `-DownloadModel`, and `-Built`. The release binary lands in `src-tauri\target\release\loquara.exe`, the NSIS installer in `src-tauri\target\release\bundle\nsis`.

Full runtime verification:

```powershell
.\tests\smoke.ps1 -ExePath .\src-tauri\target\release\loquara.exe
```

## Language

The interface ships in Polish and English. It follows the OS language — Polish on Polish systems, English everywhere else — and can be switched anytime in **Settings → General**.

## Local data

- settings, history, and recordings: `%APPDATA%\io.loquara.desktop`;
- model: `%USERPROFILE%\.cache\huggingface\hub` or paths from `HF_HOME` / `HF_HUB_CACHE`;
- model data and recordings are never committed to the repository.

## License

MIT — see [LICENSE](LICENSE).