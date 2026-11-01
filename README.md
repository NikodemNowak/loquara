# Loquara

Loquara is a fast, private speech-to-text dictation application for **Windows, macOS and Linux**. Press a global shortcut anywhere, speak, and have your speech transcribed locally on your GPU or CPU using the **NVIDIA Parakeet TDT 0.6B v3** model — and pasted directly into your active window, code editor, browser, or terminal.

**Offline by default:** audio and transcripts never leave your computer unless you switch to a cloud engine. No Python, PyTorch, or cloud accounts required.

**Optional cloud engines:** bring your own API key and dictate through **xAI Grok Voice Transcribe**, OpenAI, Groq, Mistral, ElevenLabs, Deepgram, or any OpenAI-compatible endpoint. Keys are encrypted with Windows DPAPI on Windows; on macOS and Linux they live in the app's SQLite file, protected by your user account's file permissions. The connection can be verified from Settings before you dictate.

---

## ✨ Features

- **Global shortcut:** `Ctrl+Space` starts and stops dictation anywhere (`Ctrl+Shift+Space` on macOS, where `Ctrl+Space` switches input sources). Configurable in Settings.
- **Optional push-to-talk:** choose **Hold to talk** in Settings → Dictation to record while holding the shortcut and transcribe when you release it. Toggle mode remains the default.
- **Minimal floating overlay:** a compact pill with a noise-adaptive meter during recording. Sound cues signal start, stop, and transcript readiness.
- **Universal paste:**
  - Standard apps: `Ctrl + V` (`Command + V` on macOS)
  - Windows terminals / SSH: automatic `Ctrl + Shift + V` for Termius, Windows Terminal, WSL, Git Bash, Alacritty, WezTerm, Ghostty, PuTTY, etc.
  - Linux terminals: automatic `Ctrl + Shift + V` when `xdotool` can identify the focused window.
  - Custom paste mode selection available in Settings.
- **Any hardware:** DirectML hardware acceleration on Windows covers NVIDIA, AMD, Intel, and CPU fallback with no driver toolkits to install. macOS and Linux use the publisher's prebuilt CPU engine.
- **Built-in model manager:** downloads the 670 MB Parakeet model in-app with 4 concurrent streams and live progress.
- **Live preview:** local Parakeet and xAI can build a draft in the app and clipboard while you speak. The destination stays untouched until you stop; then the complete result is pasted once.
- **Custom vocabulary & modes:** teach Loquara specialized jargon, brand names, and phonetic replacements. Transform output with custom mode rules.
- **Audio history & retention:** review past recordings and transcripts. Set automatic audio cleanup (1 day, 7 days, 30 days, or forever).
- **System tray integration:** start dictation, paste the last transcription, or launch minimized to the tray at login.
- **Bilingual interface:** Polish and English UI matching the system language, with a manual override.

---

## 🚀 Getting Started

### Windows
1. Download `Loquara_x.x.x_x64-setup.exe` or the `.msi` from [Releases](https://github.com/NikodemNowak/loquara/releases).
2. Run the installer (per-user, no administrator rights needed) and launch Loquara.
3. On first start, click **Download** to fetch the offline speech engine model.
4. Press `Ctrl+Space` anywhere to start dictating.

### macOS
1. Download the `.dmg` from [Releases](https://github.com/NikodemNowak/loquara/releases) and drag Loquara to **Applications**.
2. The build is ad-hoc signed, not notarized by Apple. On first launch, **right-click → Open**, or run:
   ```bash
   xattr -dr com.apple.quarantine /Applications/Loquara.app
   ```
3. Allow **Microphone** access when prompted.
4. Open **System Settings → Privacy & Security → Accessibility** and enable Loquara. It needs this to paste the transcript into the focused app. If the keystroke is refused, the transcript is still in the clipboard.
5. Press `Ctrl+Shift+Space` (or the shortcut you choose) to dictate.

### Linux
1. Install the `.deb`, or make the `.AppImage` executable and run it:
   ```bash
   chmod +x Loquara_x.x.x_amd64.AppImage
   ./Loquara_x.x.x_amd64.AppImage
   ```
2. Install `xdotool` for automatic paste into other windows:
   ```bash
   sudo apt install xdotool        # Debian / Ubuntu
   sudo dnf install xdotool        # Fedora
   sudo pacman -S xdotool          # Arch
   ```
3. Global shortcuts and automatic paste use **X11**. On Wayland, run Loquara through **XWayland**; a pure Wayland session does not expose global shortcut registration or `xdotool` window access.
4. The first start offers the same one-click model download.

---

## 🛠️ Architecture & Tech Stack

Loquara is a lightweight native desktop application:

- **Frontend:** React 19, TypeScript, Vite, Fluent UI icons, Radix UI.
- **Backend / Platform:** Tauri 2 (Rust), `cpal` (audio capture), `rusqlite` (SQLite storage). Windows adds Win32 window management and `SendInput`; macOS uses System Events for paste; Linux uses `xdotool` on X11.
- **Speech Engine:** native `sherpa-onnx` ONNX Runtime bindings. Windows builds sherpa-onnx with **DirectML**; macOS and Linux use the upstream prebuilt CPU libraries, bundled with the app. The model is `nvidia/parakeet-tdt-0.6b-v3`.
- **Cloud:** optional HTTPS/WebSocket transcription; no key is sent anywhere else.

---

## 💻 Development & Building

### Prerequisites
- [Node.js 22+](https://nodejs.org/) and [pnpm](https://pnpm.io/)
- [Rust](https://rustup.rs/) (edition 2024 / stable toolchain)
- **Windows:** Visual Studio C++ build tools and CMake (sherpa-onnx is compiled with DirectML)
- **macOS:** Xcode command line tools
- **Linux:** `libwebkit2gtk-4.1-dev`, `libappindicator3-dev`, `libayatana-appindicator3-dev`, `librsvg2-dev`, `patchelf`, `libasound2-dev`, `libxdo-dev`, `libx11-dev`, `libdbus-1-dev`, `libssl-dev`, and `cmake`

### Running locally
```bash
pnpm install
pnpm tauri dev
```

### Running tests
```bash
# Frontend
pnpm test

# Rust unit & platform tests
cargo test --manifest-path src-tauri/Cargo.toml --lib
```

Build the speech engine first when running Rust tests on a clean checkout; the app's build script stages its shared libraries, and Cargo does not order that after `sherpa-rs-sys`:

```bash
cargo build --manifest-path src-tauri/Cargo.toml -p sherpa-rs-sys
cargo test --manifest-path src-tauri/Cargo.toml --lib
```

### Building installers
```bash
pnpm tauri build
```

The bundles land in `src-tauri/target/release/bundle/` (or `src-tauri/target/universal-apple-darwin/release/bundle/` for a universal macOS build). The GitHub **release** workflow builds Windows, macOS (Apple silicon + Intel) and Linux on matching runners and uploads the artifacts.

### Landing page
The marketing site lives in `site/`. It is **not deployed by this repository**.

```bash
pnpm site:dev      # http://localhost:1421
pnpm site:build    # static output in site/dist
```

---

## 📁 Local Data Paths

- **Windows:** `%APPDATA%\io.loquara.desktop\`
- **macOS:** `~/Library/Application Support/io.loquara.desktop/`
- **Linux:** `~/.local/share/io.loquara.desktop/`

Engine models live in the `models/` subdirectory of that location.

---

## 📄 License

MIT — see [LICENSE](LICENSE).
