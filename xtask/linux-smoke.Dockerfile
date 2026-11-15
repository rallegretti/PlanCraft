# A clean Linux desktop for testing the Linux packages before release: Ubuntu 22.04 (the oldest
# system the packages support) with only a display server (Xvfb) and Mesa graphics, the way a
# fresh desktop install has them, and none of the extra libraries PlanCraft bundles.
# `cargo xtask package linux` starts the AppImage and the tarball's binary in it and requires
# each to open its window and save a screenshot. Local only, no cloud.
#
# Made once per architecture (`docker build --platform linux/amd64` or `linux/arm64`); the arm64
# one runs under QEMU emulation on an x86_64 machine.
FROM ubuntu:22.04
RUN apt-get update \
 && DEBIAN_FRONTEND=noninteractive apt-get install -y --no-install-recommends xvfb xauth libegl1 libgl1-mesa-dri \
 && rm -rf /var/lib/apt/lists/*
