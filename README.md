# Tan

Desktop app that captures meeting audio and screen frames and turns them into structured
notes with AI. Tauri 2 + Rust core, React + TypeScript UI. Windows, macOS and Linux from one
code base. Decisions live in `.ssot/ADR.md`.

## Layout

| Path | Purpose |
|---|---|
| `crates/domain` | Entities and rules. No dependencies. |
| `crates/application` | Use cases and ports (traits). Depends on `domain` only. |
| `crates/infra-*` | Adapters: storage, secrets, AI, capture, transcription. |
| `src-tauri` | Tauri composition root and IPC commands. No business logic. |
| `ui` | React + TypeScript + Vite front end. |

Dependency direction: `src-tauri -> infra-* -> application -> domain`
(enforced by `scripts/check-deps.sh`).

## Prerequisites

- Rust (stable) with `rustfmt` and `clippy`
- Node.js 22+ and npm
- Windows: Visual Studio Build Tools 2022 (C++ workload) and WebView2
- macOS: Xcode command line tools
- Linux: `libwebkit2gtk-4.1-dev libayatana-appindicator3-dev librsvg2-dev libxdo-dev libssl-dev libgtk-3-dev patchelf`
- Linux, audio and screen capture (build): `libasound2-dev libclang-dev libxcb1-dev libxrandr-dev`
  `libdbus-1-dev libpipewire-0.3-dev libspa-0.2-dev libwayland-dev pkg-config` (the full list is in
  `.github/workflows/ci.yml`).
  - X11 sessions capture the screen directly. **Wayland sessions** use the `xdg-desktop-portal`
    ScreenCast portal and PipeWire, so at run time they need `xdg-desktop-portal` with the backend
    of your desktop (`xdg-desktop-portal-gnome`, `-kde` or `-wlr`) and PipeWire. The first recording
    shows the system's "share your screen" dialog once; the choice is remembered (a restore token
    in the data directory), so later recordings start without a dialog. Without a portal, or if you
    decline, only audio is recorded.
- Local transcription builds whisper.cpp (`whisper-rs`): **CMake** and **libclang** (LLVM) are
  needed on every OS, and `cmake` must be on `PATH` of the shell that runs `cargo` (a shell
  opened before the installation does not see it).
  - Windows: `winget install Kitware.CMake` and `winget install --id LLVM.LLVM -e`. Use the
    MSVC toolchain. libclang is found in `C:\Program Files\LLVM\bin` without further setup; if
    LLVM lives elsewhere, set `LIBCLANG_PATH` to its `bin` folder. If a MinGW `cmake` (WinLibs)
    comes first on `PATH` it works too, because the build selects the Visual Studio generator.
  - macOS: `brew install cmake` (libclang comes with the Xcode command line tools).
  - Linux: `sudo apt-get install cmake libclang-dev build-essential`.
- GPU backends of local whisper are optional features (see *Transcription* below):
  - Vulkan (Windows, Linux): the Vulkan SDK (`winget install KhronosGroup.VulkanSDK`; Linux:
    `sudo apt-get install libvulkan-dev glslc spirv-headers`). Needs `VULKAN_SDK` and `glslc`
    on `PATH`; the SDK installer sets both for new shells.
  - Metal (macOS): the Xcode command line tools are enough.

## Commands

Run everything from the repository root.

```sh
npm --prefix ui ci                       # install UI dependencies (once)

# Run the app (Tauri CLI: either `cargo install tauri-cli --locked` or the local one)
cargo tauri dev                          # or: ./ui/node_modules/.bin/tauri dev
cargo tauri build --debug --no-bundle    # or: ./ui/node_modules/.bin/tauri build --debug --no-bundle

# Checks (same as CI)
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
bash scripts/check-deps.sh
npm --prefix ui run lint
npm --prefix ui test -- --run
npm --prefix ui run build
```

## Transcription (whisper)

The default transcription engine is local whisper (ADR-003, ADR-011). Model files are not
bundled: download one in the app (`list_whisper_models` lists them; the recommended default for
Turkish is `large-v3-turbo-q5_0`, about 574 MB; `select_whisper_model` chooses which installed
model is used). Models live in `<data>/models/` and are verified against a SHA-256 checksum
after download. Silence is skipped before whisper sees the audio, and
`estimate_transcription` forecasts the running time of a session from a short calibration run.

**Build settings** live in `.cargo/config.toml` (`[env]`): ggml is built for a portable CPU
baseline (AVX2, FMA and F16C on x86_64, so CPUs from before 2013 are not supported) instead of
the CPU of the build machine, and with optimisation on every platform. A variable you already
set in the shell wins over these.

**GPU backends** are cargo features of `tan-app`; a build with one uses the GPU when the
machine has one and falls back to the CPU otherwise. Distribution builds enable `vulkan` on
Windows and Linux and `metal` on macOS:

```sh
cargo build -p tan-app --features vulkan    # or: metal, cuda
```

On Windows the Vulkan build nests CMake projects deeper than `MAX_PATH` (260 characters)
allows, so build into a short target directory:

```sh
CARGO_TARGET_DIR='C:\t' cargo build -p tan-app --features vulkan
```

A Vulkan build on a machine without a usable Vulkan driver uses the CPU (the Vulkan loader
library itself must exist). This is covered by a test that hides the driver from the loader:

```sh
cargo test -p tan-infra-transcription --features vulkan --test vulkan_fallback
```

Tests that run real speech through a model need a ggml model file:

```sh
export TAN_WHISPER_MODEL=/path/to/ggml-small-q5_1.bin
cargo test -p tan-infra-transcription --features model-tests
cargo test -p tan-app --test transcription_e2e
# speed on this machine (release build; add `vulkan` to the features to measure the GPU):
cargo test --release -p tan-infra-transcription --features model-tests \
  --test whisper measure_speed -- --ignored --nocapture
```

## IPC types

TypeScript bindings for Rust commands are generated by `tauri-specta` into
`ui/src/ipc/bindings.ts` (never edit by hand). The app itself never writes it; after
adding or changing a command, regenerate it with:

```sh
TAN_UPDATE_BINDINGS=1 cargo test -p tan-app --test ipc
```

`cargo test` fails if the committed file is stale.
