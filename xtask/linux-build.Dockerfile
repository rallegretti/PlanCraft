# Build environment for PlanCraft's Linux packages: Ubuntu 22.04 (glibc 2.35), so the binaries
# run on Ubuntu 22.04 and every newer release (and other distributions of that age or newer).
# `cargo xtask package linux` builds this image once and runs the build in it with the host's
# Rust toolchain mounted read-only (no Rust download here). Packages only; local, no cloud.
#
# The runtime libraries listed last are not linked; they are copied into the packages
# (`lib/plancraft/`) because the app loads them at startup and some systems lack them
# (see `BUNDLED_LIBS` in `xtask/src/linux.rs`).
FROM ubuntu:22.04
RUN apt-get update \
 && DEBIAN_FRONTEND=noninteractive apt-get install -y --no-install-recommends gcc libc6-dev squashfs-tools \
    libxkbcommon0 libxkbcommon-x11-0 libxcb-xkb1 libxcursor1 libxi6 libxrender1 libwayland-egl1 \
 && rm -rf /var/lib/apt/lists/*
