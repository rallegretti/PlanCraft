# Build environment for PlanCraft's Linux packages: Ubuntu 22.04 (glibc 2.35), so the binaries
# run on Ubuntu 22.04 and every newer release (and other distributions of that age or newer).
# `cargo xtask package linux` builds this image once and runs the build in it with the host's
# Rust toolchain mounted read-only (no Rust download here). Packages only; local, no cloud.
FROM ubuntu:22.04
RUN apt-get update \
 && DEBIAN_FRONTEND=noninteractive apt-get install -y --no-install-recommends gcc libc6-dev squashfs-tools \
 && rm -rf /var/lib/apt/lists/*
