# Tan

Desktop app that captures meeting audio and screen frames and turns them into structured
notes with AI. Tauri 2 + Rust core, React + TypeScript UI. Windows, macOS and Linux from one
code base. Decisions live in `.ssot/ADR.md`.

## Install

Download the installer for your system from the
[releases page](../../releases) (every file is listed in `SHA256SUMS.txt`):

| System | File | Notes |
|---|---|---|
| Windows 10/11 (x64) | `Tan_<version>_x64-setup.exe` | Per-user install, no admin rights. WebView2 is downloaded by the installer if missing. The Visual C++ runtime is linked into the app (`crt-static`), nothing else to install. A GPU driver that provides the Vulkan loader (`vulkan-1.dll`) is needed (every common graphics driver does). |
| macOS 13 or newer (Apple Silicon) | `Tan_<version>_aarch64.dmg` | Drag *Tan* to *Applications*. Signed and notarized when the release was built with the Apple secrets (below); otherwise open it via right-click > *Open*. |
| Linux (x64) | `tan_<version>_amd64.deb` or `Tan_<version>_amd64.AppImage` | Built on the `ubuntu-latest` runner image, so it needs a glibc at least as new as that image's (the `.deb` declares it as `libc6 (>= x.y)`, so `apt` refuses it on an older system; the AppImage does not check). `sudo apt install ./tan_<version>_amd64.deb`; the AppImage only needs `chmod +x` and a run. |

On Linux the `.deb` depends on `libwebkit2gtk-4.1-0`, GTK 3, ALSA, `libvulkan1`, PipeWire and the
usual X11 client libraries, and recommends `xdg-desktop-portal` (with the backend of your desktop:
`-gnome`, `-kde` or `-wlr`), `pipewire-pulse` (system audio is read from the PulseAudio/PipeWire
monitor source) and a Secret Service provider such as GNOME Keyring or KWallet. On **Wayland**
screen frames come from the ScreenCast portal: the first recording shows the system's "share your
screen" dialog once. Without a portal, or if you decline, only audio is recorded. X11 sessions need
neither. Without a Secret Service the app falls back to a password-encrypted file for secrets.

### First run

1. **Notice.** A one-time reminder that the other participants must be told about the recording
   (KVKK); accept it to continue.
2. **Permissions.**
   - Windows: allow microphone access for desktop apps (*Settings > Privacy & security >
     Microphone*). System audio and the screen need no permission.
   - macOS: the first recording asks for the *Microphone* and for *Screen Recording* (this also
     covers the system audio). Allow both in *System Settings > Privacy & Security*, then restart
     the app once, because macOS applies a new Screen Recording permission only to a new process.
   - Linux: PipeWire/PulseAudio needs no prompt; on Wayland see the portal dialog above.
3. **Whisper model.** *Settings > Local whisper models*: download a model (the recommended default for
   Turkish is `large-v3-turbo-q5_0`, about 574 MB) and select it. Models are not bundled; they live
   in the data directory under `models/`. If you transcribe with a cloud provider only, you can
   skip this.
4. **AI provider.** *AI Providers*: add a profile for OpenAI, Anthropic, Gemini,
   DeepSeek or Ollama with its API key (kept in the OS keychain) and assign it under *Task assignments*
   to the jobs it should do (summary, image analysis and, optionally, transcription).
5. Start a recording from the *Recordings* page; the notes are produced when the recording ends.

## Releasing

Version: change `version` in `[workspace.package]` of the root `Cargo.toml` and in
`ui/package.json` (same number), add the entry to `CHANGELOG.md`. `src-tauri/tauri.conf.json`
has no version of its own; it follows the Cargo one.

Push a tag `v<version>` (or `v<version>-rc.1` for a pre-release):

```sh
git tag v0.1.0 && git push origin v0.1.0
```

`.github/workflows/release.yml` builds the Windows NSIS installer, the macOS `.app` / `.dmg`
(Apple Silicon) and the Linux `.deb` / AppImage on their own runners, and opens a **draft**
GitHub release with the files and `SHA256SUMS.txt`; you publish it by hand. *Actions > Release >
Run workflow* builds the same bundles without a tag and without a release (artifacts only).

**macOS signing secrets** (repository *Settings > Secrets and variables > Actions*). The values
are yours to enter; they never go into the repository or the logs:

| Secret | Content |
|---|---|
| `APPLE_CERTIFICATE` | The *Developer ID Application* certificate with its private key exported as `.p12`, base64-encoded (`base64 -i cert.p12 \| pbcopy`) |
| `APPLE_CERTIFICATE_PASSWORD` | The password chosen when exporting the `.p12` |
| `APPLE_SIGNING_IDENTITY` | The certificate name, e.g. `Developer ID Application: Your Name (TEAMID)` |
| `APPLE_ID` | The Apple ID e-mail used for notarization |
| `APPLE_PASSWORD` | An app-specific password of that Apple ID (appleid.apple.com), not the account password |
| `APPLE_TEAM_ID` | The 10-character team id |

Without the first three the workflow still builds the macOS bundle, **unsigned**, and prints a
warning in the log; without the last three it signs but does not notarize (also with a warning).
Windows and Linux bundles are not code-signed.

Release bundles use the GPU backends of ADR-011 (Vulkan on Windows and Linux, Metal on macOS)
and the portable CPU baseline of `.cargo/config.toml`. Building them yourself:

```sh
./ui/node_modules/.bin/tauri build --features vulkan --bundles nsis    # Windows (CARGO_TARGET_DIR='C:\t')
./ui/node_modules/.bin/tauri build --features metal --bundles app,dmg  # macOS
./ui/node_modules/.bin/tauri build --features vulkan --bundles deb,appimage  # Linux
```

The Linux Vulkan build also needs `sudo sysctl -w vm.overcommit_memory=1` before it: ggml's shader
generator forks `glslc` from an ever larger process, and under the kernel's default heuristic
overcommit the late forks fail with "Failed to fork process" (CI and the release workflow set it).

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
    on `PATH`; the SDK installer sets both for new shells. On Linux `glslc` must be a recent
    shaderc (2026.1 is known to work; ggml builds its cooperative-matrix shaders when `glslc`
    knows `GL_KHR_cooperative_matrix`). ggml's shader generator skips a shader that `glslc`
    failed to compile and the build still succeeds, so such a failure shows up only at link
    time as `undefined symbol: matmul_id_subgroup_*_cm1_data`; build with
    `cargo build -vv ... 2>&1 | grep 'cannot compile'` to see why. Keep a few GB of disk free
    (the CI job for this backend runs on its own runner for that reason).
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
