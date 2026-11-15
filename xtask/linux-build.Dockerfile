# Build environment for PlanCraft's Linux packages: Ubuntu 22.04 (glibc 2.35), so the binaries
# run on Ubuntu 22.04 and every newer release (and other distributions of that age or newer).
# `cargo xtask package linux` builds this image once and runs the build in it with the host's
# Rust toolchain mounted read-only (no Rust download here). Packages only; local, no cloud.
#
# It builds for x86_64 natively and cross-compiles for arm64 (aarch64). The arm64 C compiler is
# used only as the linker (PlanCraft compiles no C code; see deny.toml), against Ubuntu 22.04's
# arm64 C library.
#
# The runtime libraries listed last are not linked; they are copied into the packages
# (`lib/plancraft/`) because the app loads them at startup and some systems lack them
# (see `BUNDLED_LIBS` in `xtask/src/linux.rs`). Their arm64 copies come from Ubuntu's arm64
# archive (ports.ubuntu.com), so the existing sources are limited to amd64 first.
FROM ubuntu:22.04
RUN sed -i 's/^deb /deb [arch=amd64] /' /etc/apt/sources.list \
 && printf 'deb [arch=arm64] http://ports.ubuntu.com/ubuntu-ports %s main universe\n' jammy jammy-updates jammy-security \
    > /etc/apt/sources.list.d/arm64.list \
 && dpkg --add-architecture arm64 \
 && apt-get update \
 && DEBIAN_FRONTEND=noninteractive apt-get install -y --no-install-recommends gcc libc6-dev squashfs-tools \
    gcc-aarch64-linux-gnu libc6-dev-arm64-cross \
    libxkbcommon0 libxkbcommon-x11-0 libxcb-xkb1 libxcursor1 libxi6 libxrender1 libwayland-egl1 \
    libxkbcommon0:arm64 libxkbcommon-x11-0:arm64 libxcb-xkb1:arm64 libxcursor1:arm64 libxi6:arm64 libxrender1:arm64 libwayland-egl1:arm64 \
 && rm -rf /var/lib/apt/lists/*
