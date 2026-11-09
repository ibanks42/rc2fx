#!/usr/bin/env bash
# Runs inside the package-check container, from a snapshot of the source, for
# releases and `just package-check` alike. The mixer runtime is built here from
# that snapshot too, so the AppImage cannot embed anything older.
set -euo pipefail
cd /work/src

scratch=$(mktemp -d)
trap 'rm -rf "$scratch"' EXIT
export GITHUB_ENV=$scratch/env GITHUB_PATH=$scratch/path
bash .github/scripts/setup-release-tools.sh zig
bash .github/scripts/setup-release-tools.sh appimage
# setup-vpk.sh downloads into RUNNER_TEMP, as on GitHub.
export RUNNER_TEMP=/work/cache/runner DOTNET_CLI_HOME=/work/cache/home
mkdir -p "$RUNNER_TEMP"
bash .github/scripts/setup-vpk.sh
PATH=$(cat "$GITHUB_PATH"):$PATH
set -a
# shellcheck disable=SC1090
. "$GITHUB_ENV"
set +a
unset GITHUB_ENV GITHUB_PATH

rm -rf target/package target/*-appimage-work
cargo xtask mixer-build
python3 .github/scripts/test_prepare_appimage_notices.py
python3 .github/scripts/prepare-appimage-notices.py --cache /work/cache/notices \
    --vpk-package "$RUNNER_TEMP"/rc2fx-vpk/source/vpk.*.nupkg
RC2FX_RELEASE_BUILD=1 cargo xtask package
