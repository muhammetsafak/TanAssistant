#!/usr/bin/env bash
# Run the checks of .github/workflows/ci.yml on a developer machine, for when GitHub Actions is
# unavailable. Mirrors the workflow job by job; keep the two in step when either changes.
#
#   scripts/ci-local.sh [macos|linux|vulkan|audit|all]...    (default: all)
#
#   macos   the macos-latest job, on this Mac (needs Xcode CLT, cmake, Node, Rust)
#   linux   the ubuntu-latest job, in a Docker container
#   vulkan  the "whisper Vulkan (ubuntu-latest)" job, in a Docker container
#   audit   the "cargo audit" job, in a Docker container
#   all     macos (when on macOS) + linux + vulkan + audit
#
# The Windows job has no local stand-in: run `cargo test --workspace` and the Vulkan steps of
# ci.yml on a Windows machine.
#
# Environment:
#   CI_LOCAL_PLATFORM  Docker platform, default: the host's (linux/arm64 on Apple Silicon).
#                      linux/amd64 matches the runners but runs under emulation and is slow.
#
# Containers never write into the working tree: the sources are copied into the container
# (without target/ and node_modules/), and build output lives in named Docker volumes.
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$root"

image="tan-ci-local:ubuntu-26.04"
platform="${CI_LOCAL_PLATFORM:-}"
results=()

export CARGO_TERM_COLOR=always
export CARGO_PROFILE_DEV_DEBUG=line-tables-only
export CARGO_PROFILE_TEST_DEBUG=line-tables-only

step() { printf '\n\033[1;36m==> %s\033[0m\n' "$*"; }

# The "Bundle configuration" step of ci.yml.
bundle_config() {
  local cargo_version ui_version
  cargo_version="$(sed -n '/^\[workspace.package\]/,/^\[/{s/^version = "\(.*\)"/\1/p;}' Cargo.toml)"
  ui_version="$(node -p "require('./ui/package.json').version")"
  echo "workspace version: $cargo_version, ui version: $ui_version"
  test -n "$cargo_version"
  if [ "$cargo_version" != "$ui_version" ]; then
    echo "error: Cargo.toml ($cargo_version) and ui/package.json ($ui_version) disagree" >&2
    return 1
  fi
  if node -e "process.exit('version' in require('./src-tauri/tauri.conf.json') ? 0 : 1)"; then
    echo 'error: tauri.conf.json must not set "version"' >&2
    return 1
  fi
  if [ "$(uname -s)" = Darwin ]; then
    plutil -lint src-tauri/Info.plist src-tauri/Entitlements.plist
  fi
}

# Steps shared by the macOS and Linux matrix entries of the `ci` job, in workflow order.
common_head() {
  step "Rust format";              cargo fmt --all -- --check
  step "Rust clippy";              cargo clippy --workspace --all-targets -- -D warnings
  step "Rust tests";               cargo test --workspace
  step "Capture spike examples";   cargo build -p tan-infra-capture --examples
}

common_tail() {
  step "Dependency direction";     bash scripts/check-deps.sh
  step "UI install";               npm --prefix ui ci
  step "UI lint";                  npm --prefix ui run lint
  step "UI test";                  npm --prefix ui test -- --run
  step "UI build";                 npm --prefix ui run build
  step "Bundle configuration";     bundle_config
  step "Tauri build (debug)";      ./ui/node_modules/.bin/tauri build --debug --no-bundle
}

job_macos() {
  if [ "$(uname -s)" != Darwin ]; then
    echo "macos: skipped (not on macOS)"; return 0
  fi
  command -v cmake >/dev/null || { echo "error: cmake missing (brew install cmake)" >&2; return 1; }
  common_head
  step "Whisper GPU backend, Metal"
  MACOSX_DEPLOYMENT_TARGET=13.0 cargo build -p tan-app --features metal
  common_tail
}

job_linux() {
  common_head
  common_tail
}

job_vulkan() {
  step "Build the Vulkan backend and check the shader generator"
  # Needs a privileged container; see the workflow for why the shader generator's forks need it.
  sysctl -w vm.overcommit_memory=1 \
    || echo "warning: could not set vm.overcommit_memory=1; shader forks may fail" >&2
  local log=/tmp/vulkan-build.log
  set -o pipefail
  cargo build -vv -p tan-infra-transcription --features vulkan 2>&1 | tee "$log" >/dev/null
  if grep -q 'cannot compile \|Error executing command for ' "$log"; then
    echo 'error: ggml could not compile some Vulkan shaders' >&2
    grep -n -A12 'cannot compile \|Error executing command for ' "$log" | head -100
    return 1
  fi
  step "Vulkan fallback test"
  cargo test -p tan-infra-transcription --features vulkan --test vulkan_fallback
}

job_audit() {
  step "Install cargo-audit"
  command -v cargo-audit >/dev/null || cargo install cargo-audit --locked --version 0.22.2
  step "Audit Cargo.lock";         cargo audit
}

# Run one job inside the Linux container. Each job gets its own target volume, as the runners
# give each job its own machine; the cargo registry is shared.
in_container() {
  local job="$1"
  command -v docker >/dev/null || { echo "error: docker not found" >&2; return 1; }
  docker info >/dev/null 2>&1 || { echo "error: the Docker daemon is not running" >&2; return 1; }

  local plat_args=()
  [ -n "$platform" ] && plat_args=(--platform "$platform")
  local tag="${platform//\//-}"; tag="${tag:-native}"

  step "Docker image $image ($tag)"
  docker build ${plat_args[@]+"${plat_args[@]}"} -q -t "$image-$tag" -f scripts/ci-local/linux.Dockerfile scripts/ci-local >/dev/null

  local priv=()
  [ "$job" = vulkan ] && priv=(--privileged)

  docker run --rm ${plat_args[@]+"${plat_args[@]}"} ${priv[@]+"${priv[@]}"} \
    -v "$root":/src:ro \
    -v "tan-ci-local-$tag-$job-target":/work/target \
    -v "tan-ci-local-$tag-registry":/opt/cargo/registry \
    -v "tan-ci-local-$tag-npm":/root/.npm \
    -e CARGO_TERM_COLOR -e CARGO_PROFILE_DEV_DEBUG -e CARGO_PROFILE_TEST_DEBUG \
    -e CI_LOCAL_IN_CONTAINER=1 \
    "$image-$tag" bash -c '
      set -euo pipefail
      rsync -a --delete --exclude /target --exclude node_modules --exclude /.git /src/ /work/
      cd /work
      exec bash scripts/ci-local.sh --inside '"$job"'
    '
}

run_job() {
  local job="$1"
  if [ "${CI_LOCAL_IN_CONTAINER:-}" = 1 ] || [ "$job" = macos ]; then
    "job_$job"
  else
    in_container "$job"
  fi
}

if [ "${1:-}" = --inside ]; then
  run_job "$2"
  exit
fi

jobs=("$@")
[ ${#jobs[@]} -eq 0 ] && jobs=(all)
if [ "${jobs[*]}" = all ]; then
  jobs=(linux vulkan audit)
  [ "$(uname -s)" = Darwin ] && jobs=(macos "${jobs[@]}")
fi

status=0
for job in "${jobs[@]}"; do
  case "$job" in
    macos|linux|vulkan|audit) ;;
    *) echo "unknown job: $job (macos|linux|vulkan|audit|all)" >&2; exit 2 ;;
  esac
  start=$SECONDS
  if ( run_job "$job" ); then
    results+=("$job: passed ($((SECONDS - start))s)")
  else
    results+=("$job: FAILED ($((SECONDS - start))s)"); status=1
  fi
done

printf '\n\033[1m%s\033[0m\n' "Summary"
printf '  %s\n' "${results[@]}"
exit $status
