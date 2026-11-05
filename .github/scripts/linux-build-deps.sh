#!/bin/sh
# Ubuntu packages the Linux desktop build needs, here and in the package-check container.
set -eu
sudo=
if [ "$(id -u)" -ne 0 ]; then sudo=sudo; fi
$sudo apt-get update
$sudo env DEBIAN_FRONTEND=noninteractive apt-get install -y --no-install-recommends \
    build-essential pkg-config cmake clang \
    libfontconfig1-dev libwayland-dev libxkbcommon-dev libxkbcommon-x11-dev \
    libx11-dev libxcb1-dev libvulkan1 libasound2-dev \
    libssl-dev libudev-dev libdbus-1-dev
