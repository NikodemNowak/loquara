# Loquara

Loquara is a fast, private, and offline speech-to-text dictation application for Windows. Press a global shortcut anywhere, speak, and have your speech transcribed locally on your GPU or CPU using the **NVIDIA Parakeet TDT 0.6B v3** model — and pasted directly into your active window, code editor, browser, or terminal.

**100% offline and private:** audio and transcripts never leave your computer. No Python, PyTorch, or cloud accounts required.

---

## ✨ Features

- **Global shortcut:** `Ctrl+Space` starts and stops dictation anywhere. Configurable in settings.
- **Minimal floating overlay:** Compact pill with a noise-adaptive meter during recording. Sound cues signal start, stop, and transcript readiness.
- **Universal paste:**
  - Standard apps: `Ctrl + V`
  - Linux SSH / Terminals: automatic `Ctrl + Shift + V` for Termius, Windows Terminal, WSL, Git Bash, Alacritty, WezTerm, Ghostty, etc.
  - PuTTY / KiTTY: automatic `Shift + Insert`
  - Custom paste mode selection available in Settings.
- **Runs on any GPU or CPU:** DirectML hardware acceleration covers NVIDIA, AMD, Intel, and CPU fallback out of the box with zero driver toolkits to install.
- **Built-in model manager:** Downloads the lightweight 670 MB Parakeet model in-app with 4 concurrent streams and live progress.
- **Custom vocabulary & modes:** Teach Loquara specialized jargon, brand names, and phonetic replacements. Transform output with custom mode rules.
- **Audio history & retention:** Review past recordings and transcripts. Set automatic audio cleanup (1 day, 7 days, 30 days, or forever).
- **System tray integration:** Start dictation, paste last transcription, or launch minimized to the tray at login.
- **Bilingual interface:** Polish and English UI matching system language with manual override.

---

## 🚀 Getting Started

### Installation
1. Download the latest installer (`Loquara_x.x.x_x64-setup.exe` or `.msi`) from [Releases](https://github.com/NikodemNowak/loquara/releases).
2. Run the installer and launch Loquara.
3. On first start, click **Download** to fetch the offline speech engine model (670 MB).
4. Press `Ctrl+Space` anywhere in Windows to start dictating!

---

## 🛠️ Architecture & Tech Stack

Loquara is built as a lightweight, native Windows desktop application:

- **Frontend:** React 19, TypeScript, Vite, Fluent UI icons, Radix UI.
- **Backend / Platform:** Tauri 2 (Rust), Win32 API (window management, thread-attached input injection, scan-code mapping), `cpal` (audio capture), `rusqlite` (SQLite storage).
- **Speech Engine:** Native `sherpa-onnx` ONNX Runtime bindings with DirectML / CPU acceleration running `nvidia/parakeet-tdt-0.6b-v3`.

---

## 💻 Development & Building

### Prerequisites
- Windows 10/11 x64
- [Node.js 22+](https://nodejs.org/) & [pnpm](https://pnpm.io/)
- [Rust](https://rustup.rs/) (edition 2024 / stable toolchain)
- CMake 3.x (for compiling native ONNX runtime bindings)

### Running locally
```powershell
# Install frontend dependencies
pnpm install

# Run the app in development mode (with hot reload)
pnpm tauri dev
```

### Running tests
```powershell
# Run frontend tests
pnpm test

# Run Rust unit & platform tests
cargo test --manifest-path src-tauri/Cargo.toml --lib
```

### Building the installer
```powershell
pnpm tauri build
```
The resulting NSIS installer and MSI packages are created in `src-tauri/target/release/bundle/`.

---

## 📁 Local Data Paths

- **Settings, SQLite database, and recordings:** `%APPDATA%\io.loquara.desktop\`
- **Engine models:** `%APPDATA%\io.loquara.desktop\models\`

---

## 📄 License

MIT — see [LICENSE](LICENSE).