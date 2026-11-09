#!/bin/sh
# The build image's tag: a digest of what it is built from, so the image is
# rebuilt exactly when its Containerfile or the pinned Rust changes. Shared by
# package-check.sh and the build-image workflow.
set -eu
root=$(git -C "$(dirname "$0")" rev-parse --show-toplevel)
cat "$root/.github/scripts/package-check.Containerfile" "$root/rust-toolchain.toml" |
    sha256sum | cut -c1-12
