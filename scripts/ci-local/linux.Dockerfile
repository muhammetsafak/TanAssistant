# Local stand-in for the ubuntu-latest jobs of .github/workflows/ci.yml (see scripts/ci-local.sh).
# Ubuntu 26.04 matches the runner image ubuntu-latest migrates to, and ships the glslc (2026.1)
# that knows GL_KHR_cooperative_matrix, which the Vulkan job relies on.
FROM ubuntu:26.04

ENV DEBIAN_FRONTEND=noninteractive

# Same package list as the "Install Linux system packages" step, plus what the runner image
# provides out of the box (curl, git, rsync, a C toolchain, Node).
RUN apt-get update && apt-get install -y --no-install-recommends \
      ca-certificates curl git rsync build-essential file \
      libwebkit2gtk-4.1-dev \
      libayatana-appindicator3-dev \
      librsvg2-dev \
      libxdo-dev \
      libssl-dev \
      libgtk-3-dev \
      patchelf \
      pkg-config \
      libasound2-dev \
      libclang-dev \
      cmake \
      libxcb1-dev \
      libxrandr-dev \
      libdbus-1-dev \
      libpipewire-0.3-dev \
      libspa-0.2-dev \
      libwayland-dev \
      libegl-dev \
      libgbm-dev \
      libdrm-dev \
      libvulkan-dev \
      glslc \
      spirv-headers \
      nodejs npm \
    && rm -rf /var/lib/apt/lists/*

# The workflow uses the stable toolchain with rustfmt and clippy.
ENV RUSTUP_HOME=/opt/rustup CARGO_HOME=/opt/cargo PATH=/opt/cargo/bin:$PATH
RUN curl -sSf https://sh.rustup.rs | sh -s -- -y --profile minimal \
      --default-toolchain stable --component rustfmt,clippy \
    && chmod -R a+w /opt/rustup /opt/cargo

WORKDIR /work
