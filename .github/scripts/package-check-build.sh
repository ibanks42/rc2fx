#!/usr/bin/env bash
# Runs inside the package-check container against a source snapshot for both
# releases and `just package-check`. Only builds the desktop AppImage (the app
# downloads the mixer runtime separately).
set -euo pipefail
cd /work/src

# Prebuilt gpuikit and velopack_libc are gitignored and absent from the snapshot.
bash desktop/scripts/fetch-gpuikit.sh linux-amd64
bash desktop/scripts/fetch-velopack.sh linux-amd64
# Drop auth tokens right after fetching gpuikit so later build/packaging steps can't read them.
unset GPUIKIT_TOKEN GH_TOKEN GITHUB_TOKEN

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
