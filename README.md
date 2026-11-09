# Tan

Desktop app that records meetings and turns their audio and screen into structured notes with AI.

[![CI](https://github.com/muhammetsafak/TanAssistant/actions/workflows/ci.yml/badge.svg)](https://github.com/muhammetsafak/TanAssistant/actions/workflows/ci.yml)
[![License](https://img.shields.io/github/license/muhammetsafak/TanAssistant)](LICENSE)
[![Designed & Maintained with Tan](https://muhammetsafak.com/badges/designed-maintained-with-tan.svg)](https://muhammetsafak.com/tan/)

Tan runs on your computer during a meeting in any conferencing tool. It records your microphone
and the other participants' audio on separate tracks and keeps only the screen frames that
change. When the meeting ends it produces a transcript and a note with a summary, decisions,
action items and open questions. Recordings, frames and notes stay on your machine; cloud AI
providers are used only for the tasks you assign to them.

## Features

- Records the microphone and the system audio on separate tracks, plus screen frames that
  visibly change, on Windows, macOS and Linux (X11 and Wayland).
- Pauses and resumes a recording; the paused time is left out of the recording.
- Transcribes locally with whisper (Vulkan or Metal GPU, CPU fallback) or with OpenAI or Gemini.
- Writes a note in the language you choose: summary, decisions, checkable action items and open
  questions, with the screen frames that were shown.
- Works with OpenAI, Anthropic, Gemini, DeepSeek or a local Ollama; API keys are kept in the OS
  keychain.
- Searches across notes and transcripts and exports a note with its frames as Markdown.

## Quick Start

Prerequisites: Rust (stable), Node.js 22+, CMake and the platform packages listed in
[Development](docs/development.md#prerequisites).

```bash
git clone https://github.com/muhammetsafak/TanAssistant.git
cd TanAssistant
npm --prefix ui ci
./ui/node_modules/.bin/tauri dev
```

The first build compiles whisper.cpp and Tauri and takes several minutes. The app window opens
with a one-time recording notice. A development build transcribes on the CPU; add
`--features metal` (macOS) or `--features vulkan` (Windows, Linux) to use the GPU.

## Installation

There is no published release yet. Until the first release, build the app from source:

```bash
cfg='{"bundle":{"createUpdaterArtifacts":false}}'  # no updater signing key on this machine
./ui/node_modules/.bin/tauri build --features metal --bundles app,dmg --config "$cfg"        # macOS
./ui/node_modules/.bin/tauri build --features vulkan --bundles nsis --config "$cfg"          # Windows
./ui/node_modules/.bin/tauri build --features vulkan --bundles deb,appimage --config "$cfg"  # Linux
```

The `--config` switches the update files off; without it `tauri build` fails without the
project's signing key (see [Releasing](docs/releasing.md#building-the-bundles-locally)). The
commands are bash: on Windows run them in Git Bash, as PowerShell does not take the `cfg=...`
assignment or the single-quoted JSON the same way.

Requirements:

- Windows 10/11 (x64) with WebView2 and a GPU driver that provides the Vulkan loader.
- macOS 13 or newer (Apple Silicon).
- Linux (x64) with WebKitGTK 4.1, PipeWire or PulseAudio, and a Secret Service provider such as
  GNOME Keyring or KWallet. Wayland sessions also need `xdg-desktop-portal` with the backend of
  your desktop.

Build details, GPU backends and platform packages are in [Development](docs/development.md); the
release workflow is described in [Releasing](docs/releasing.md).

## Usage

1. **Notice.** On first launch, accept the one-time reminder that the other participants must
   be told about the recording.
2. **Permissions.**
   - Windows: allow microphone access for desktop apps in *Settings > Privacy & security >
     Microphone*. System audio and the screen need no permission.
   - macOS: the first recording asks for *Microphone* and *Screen Recording* (which also covers
     the system audio). Allow both, then restart the app once; macOS applies a new Screen
     Recording permission only to a new process.
   - Linux: PipeWire and PulseAudio need no prompt. On Wayland the first recording shows the
     system's screen sharing dialog once; without a portal, or if you decline, only audio is
     recorded.
3. **Whisper model.** In *Settings > Local whisper models*, download a model and select it. The
   recommended default for Turkish is `large-v3-turbo-q5_0` (about 574 MB). Skip this if you
   transcribe with a cloud provider only.
4. **AI provider.** In *AI Providers*, add a profile for OpenAI, Anthropic, Gemini, DeepSeek or
   Ollama and assign it to the tasks it should do: note writing, image analysis and, optionally,
   transcription. API keys are entered in *Credentials*.
5. **Record.** Choose the screen on the *Library* page and start the recording. Pause and resume
   as needed; the note is produced when you stop.

Without a Secret Service on Linux, or when the OS keychain is unavailable, secrets fall back to a
password-encrypted file.

## Documentation

- [Development](docs/development.md): prerequisites, project layout, commands, transcription
  build settings and IPC bindings.
- [Releasing](docs/releasing.md): versioning, the release workflow and macOS signing.

## Project Status

**Alpha.** Version 0.1.0 has not been released yet; behaviour, data layout and settings may change
between versions without a migration path.

## Contributing

Contributions are welcome. Read the [contributing guide](CONTRIBUTING.md) before opening an issue
or pull request.

## Security

Do not report security vulnerabilities through public issues; follow the
[security policy](SECURITY.md) instead.

## License

The code is licensed under the [GNU General Public License v3.0 only](LICENSE) (`GPL-3.0-only`).
The Tan name, the Tan mascot and the app icon are not covered by this license; see
[assets/brand](assets/brand/README.md).
