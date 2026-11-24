#!/usr/bin/env bash
# Runs inside the package-check container against a source snapshot for both
# releases and `just package-check`. Only builds the desktop AppImage (the app
# downloads the mixer runtime separately).
set -euo pipefail
cd /work/src

# Prebuilt goui-kit and velopack_libc are gitignored and absent from the snapshot.
./xtask fetch-goui-kit linux-amd64
./xtask fetch-velopack linux-amd64

scratch=$(mktemp -d)
trap 'rm -rf "$scratch"' EXIT
export GITHUB_ENV=$scratch/env
bash .github/scripts/setup-release-tools.sh appimage
# setup-vpk.sh expects RUNNER_TEMP just like on GitHub Actions.
export RUNNER_TEMP=/work/cache/runner DOTNET_CLI_HOME=/work/cache/home
mkdir -p "$RUNNER_TEMP"
bash .github/scripts/setup-vpk.sh
set -a
# shellcheck disable=SC1090
. "$GITHUB_ENV"
set +a
unset GITHUB_ENV


rm -rf target/package target/*-appimage-work
python3 .github/scripts/test_prepare_appimage_notices.py
python3 .github/scripts/prepare-appimage-notices.py --cache /work/cache/notices \
    --vpk-package "$RUNNER_TEMP"/rc2fx-vpk/source/vpk.*.nupkg
RC2FX_RELEASE_BUILD=1 ./xtask package
