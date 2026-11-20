#!/bin/sh
# Compute the build image tag from package-check.Containerfile and rust-toolchain.toml
# so both package-check.sh and build-image.yml rebuild whenever Go, Rust, or base packages change.
set -eu
root=$(git -C "$(dirname "$0")" rev-parse --show-toplevel)
cat "$root/.github/scripts/package-check.Containerfile" "$root/rust-toolchain.toml" |
    sha256sum | cut -c1-12
