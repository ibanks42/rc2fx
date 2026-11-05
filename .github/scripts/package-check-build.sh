#!/usr/bin/env bash
# Runs inside the package-check container, from a snapshot of the source. Same
# steps as the release workflow, but the mixer runtime is built here from that
# snapshot, so the packages cannot embed anything older.
set -euo pipefail
cd /work/src

scratch=$(mktemp -d)
trap 'rm -rf "$scratch"' EXIT
export GITHUB_ENV=$scratch/env GITHUB_PATH=$scratch/path
bash .github/scripts/setup-release-tools.sh zig
bash .github/scripts/setup-release-tools.sh appimage
PATH=$(cat "$GITHUB_PATH"):$PATH
set -a
# shellcheck disable=SC1090
. "$GITHUB_ENV"
set +a
unset GITHUB_ENV GITHUB_PATH

rm -rf target/package target/*-appimage-work
cargo xtask mixer-build
python3 .github/scripts/test_prepare_appimage_notices.py
python3 .github/scripts/prepare-appimage-notices.py --cache /work/cache/notices
RC2FX_RELEASE_BUILD=1 cargo xtask package
