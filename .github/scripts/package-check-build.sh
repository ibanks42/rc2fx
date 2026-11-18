#!/usr/bin/env bash
# Runs inside the package-check container, from a snapshot of the source, for
# releases and `just package-check` alike. The AppImage doesn't carry the
# mixer runtime (the app downloads it), so nothing here builds the mixer.
set -euo pipefail
cd /work/src

# The gpuikit download and Velopack's C library are gitignored, so the
# snapshot never has them: fetch them first, checked against their pins.
bash desktop/scripts/fetch-gpuikit.sh linux-amd64
bash desktop/scripts/fetch-velopack.sh linux-amd64
# The token that reads the gpuikit bindings is for that fetch alone: nothing
# after it (tool setup, NuGet, build scripts, module downloads, packaging
# tools) gets to see it.
unset GPUIKIT_TOKEN GH_TOKEN GITHUB_TOKEN

scratch=$(mktemp -d)
trap 'rm -rf "$scratch"' EXIT
export GITHUB_ENV=$scratch/env
bash .github/scripts/setup-release-tools.sh appimage
# setup-vpk.sh downloads into RUNNER_TEMP, as on GitHub.
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
