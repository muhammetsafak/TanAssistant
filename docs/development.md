# Development

## Prerequisites

- Rust (stable) with `rustfmt` and `clippy`
- Node.js 22+ and npm
- Windows: Visual Studio Build Tools 2022 (C++ workload) and WebView2
- macOS: Xcode command line tools
- Linux: `libwebkit2gtk-4.1-dev libayatana-appindicator3-dev librsvg2-dev libxdo-dev libssl-dev libgtk-3-dev patchelf`
- Linux, audio and screen capture (build): `libasound2-dev libclang-dev libxcb1-dev libxrandr-dev`
  `libdbus-1-dev libpipewire-0.3-dev libspa-0.2-dev libwayland-dev pkg-config` (the full list is in
  `.github/workflows/ci.yml`).
  - X11 sessions capture the screen directly. Wayland sessions use the `xdg-desktop-portal`
    ScreenCast portal and PipeWire, so at run time they need `xdg-desktop-portal` with the backend
    of your desktop (`xdg-desktop-portal-gnome`, `-kde` or `-wlr`) and PipeWire. The first recording
    shows the system's "share your screen" dialog once; the choice is remembered (a restore token
    in the data directory), so later recordings start without a dialog. Without a portal, or if you
    decline, only audio is recorded.
- Local transcription builds whisper.cpp (`whisper-rs`): CMake and libclang (LLVM) are needed on
  every OS, and `cmake` must be on `PATH` of the shell that runs `cargo` (a shell opened before the
  installation does not see it).
  - Windows: `winget install Kitware.CMake` and `winget install --id LLVM.LLVM -e`. Use the MSVC
    toolchain. libclang is found in `C:\Program Files\LLVM\bin` without further setup; if LLVM
    lives elsewhere, set `LIBCLANG_PATH` to its `bin` folder.
  - macOS: `brew install cmake` (libclang comes with the Xcode command line tools).
  - Linux: `sudo apt-get install cmake libclang-dev build-essential`.
- GPU backends of local whisper are optional features (see [Transcription](#transcription)):
  - Vulkan (Windows, Linux): the Vulkan SDK (`winget install KhronosGroup.VulkanSDK`; Linux:
    `sudo apt-get install libvulkan-dev glslc spirv-headers`). Needs `VULKAN_SDK` and `glslc` on
    `PATH`. On Linux `glslc` must be a recent shaderc (2026.1 is known to work). ggml's shader
    generator skips a shader that `glslc` failed to compile and the build still succeeds, so such
    a failure shows up only at link time as `undefined symbol: matmul_id_subgroup_*_cm1_data`;
    build with `cargo build -vv ... 2>&1 | grep 'cannot compile'` to see why.
  - Metal (macOS): the Xcode command line tools are enough.

## Project Layout

| Path | Purpose |
|---|---|
| `crates/domain` | Entities and rules. No dependencies. |
| `crates/application` | Use cases and ports (traits). Depends on `domain` only. |
| `crates/infra-*` | Adapters: storage, secrets, AI, capture, transcription. |
| `src-tauri` | Tauri composition root, IPC commands and tray. No business logic. |
| `ui` | React + TypeScript + Vite front end. |
| `assets/brand` | Source SVGs of the app icon and tray glyphs. |

Dependency direction: `src-tauri -> infra-* -> application -> domain` (enforced by
`scripts/check-deps.sh`).

## Commands

Run everything from the repository root.

```bash
npm --prefix ui ci                                # install UI dependencies (once)
./ui/node_modules/.bin/tauri dev                  # run the app
./ui/node_modules/.bin/tauri build --debug --no-bundle
```

Checks (the same as CI):

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
bash scripts/check-deps.sh
npm --prefix ui run lint
npm --prefix ui test -- --run
npm --prefix ui run build
```

`scripts/ci-local.sh` runs the CI jobs on your machine: the macOS job directly, the Linux, Vulkan
and `cargo audit` jobs in a Docker container. The Windows job has no local stand-in.

```bash
scripts/ci-local.sh all    # or: macos, linux, vulkan, audit
```

## App Icon

The icon set in `src-tauri/icons/` and the tray glyphs are generated from the SVGs in
`assets/brand/`. After changing an SVG, regenerate them and commit the result:

```bash
bash scripts/gen-icons.sh
```

## Transcription

The default transcription engine is local whisper. Model files are not bundled: download one in
the app (the recommended default for Turkish is `large-v3-turbo-q5_0`, about 574 MB). Models live
in `<data>/models/` and are verified against a SHA-256 checksum after download. Silence is skipped
before whisper sees the audio, and the app forecasts the running time of a session from a short
calibration run.

Build settings live in `.cargo/config.toml` (`[env]`): ggml is built for a portable CPU baseline
(AVX2, FMA and F16C on x86_64, so CPUs from before 2013 are not supported) instead of the CPU of
the build machine, and with optimisation on every platform. A variable you already set in the
shell wins over these.

GPU backends are cargo features of `tan-app`; a build with one uses the GPU when the machine has
one and falls back to the CPU otherwise. Distribution builds enable `vulkan` on Windows and Linux
and `metal` on macOS:

```bash
cargo build -p tan-app --features vulkan    # or: metal, cuda
```

On Windows the Vulkan build nests CMake projects deeper than `MAX_PATH` (260 characters) allows,
so build into a short target directory:

```bash
CARGO_TARGET_DIR='C:\t' cargo build -p tan-app --features vulkan
```

The Linux Vulkan build needs `sudo sysctl -w vm.overcommit_memory=1` first: ggml's shader
generator forks `glslc` from an ever larger process, and under the kernel's default heuristic
overcommit the late forks fail with "Failed to fork process".

A Vulkan build on a machine without a usable Vulkan driver uses the CPU (the Vulkan loader library
itself must exist). A test covers this by hiding the driver from the loader:

```bash
cargo test -p tan-infra-transcription --features vulkan --test vulkan_fallback
```

Tests that run real speech through a model need a ggml model file:

```bash
export TAN_WHISPER_MODEL=/path/to/ggml-small-q5_1.bin
cargo test -p tan-infra-transcription --features model-tests
cargo test -p tan-app --test transcription_e2e
```

## IPC Bindings

TypeScript bindings for the Rust commands are generated by `tauri-specta` into
`ui/src/ipc/bindings.ts`; never edit that file by hand. After adding or changing a command,
regenerate it:

```bash
TAN_UPDATE_BINDINGS=1 cargo test -p tan-app --test ipc
```

`cargo test` fails if the committed file is stale.
