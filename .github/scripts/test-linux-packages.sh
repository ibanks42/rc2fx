#!/bin/sh
# Check the Linux release artifacts in disposable containers.
#
# This is the single entry point for local and CI package checks. It selects
# exactly the current version's AppImage (with its update package and feed),
# verifies their checksums and the feed on the host, then streams a
# read-only copy into throwaway containers. No
# artifact is modified, no host bind mount is used, and no device, display or
# FUSE access is needed.
#
#   test-linux-packages.sh [DIR] [--artifacts DIR] [--version VERSION]
#                          [--runtime auto|podman|docker]
#
# Defaults: target/package, [workspace.package] version from Cargo.toml, and
# podman when available, otherwise docker. CONTAINER_ENGINE=podman|docker
# selects the engine from the environment, matching the package-check wrapper.
# The checks need network access for apt/dnf to install inspection tools.
set -eu

ROOT=$(CDPATH='' cd -- "$(dirname -- "$0")/../.." && pwd)
ARTIFACTS=
VERSION=
RUNTIME=${CONTAINER_ENGINE:-${RC2FX_CONTAINER_RUNTIME:-auto}}
# AlmaLinux 8 has the oldest glibc the AppImage supports (2.28); the others
# span what people run now.
APPIMAGE_IMAGES=${RC2FX_APPIMAGE_IMAGES:-"docker.io/library/almalinux:8 docker.io/library/ubuntu:22.04 docker.io/library/debian:12-slim docker.io/library/ubuntu:24.04 docker.io/library/debian:13-slim docker.io/library/fedora:44"}

usage() {
    sed -n '2,16p' "$0" | sed 's/^# \{0,1\}//'
    exit "${1:-0}"
}

fail() {
    echo "package check failed: $*" >&2
    exit 1
}

while [ "$#" -gt 0 ]; do
    case "$1" in
        --artifacts)
            [ "$#" -ge 2 ] || usage 2
            ARTIFACTS=$2
            shift 2
            ;;
        --version)
            [ "$#" -ge 2 ] || usage 2
            VERSION=$2
            shift 2
            ;;
        --runtime)
            [ "$#" -ge 2 ] || usage 2
            RUNTIME=$2
            shift 2
            ;;
        -h|--help)
            usage
            ;;
        -*)
            echo "unknown argument: $1" >&2
            usage 2
            ;;
        *)
            [ -z "$ARTIFACTS" ] || { echo "artifact directory given twice" >&2; usage 2; }
            ARTIFACTS=$1
            shift
            ;;
    esac
done

[ -n "$ARTIFACTS" ] || ARTIFACTS=$ROOT/target/package
case "$ARTIFACTS" in
    /*) ;;
    *) ARTIFACTS=$PWD/$ARTIFACTS ;;
esac
[ -d "$ARTIFACTS" ] || fail "artifact directory does not exist: $ARTIFACTS"

if [ -z "$VERSION" ]; then
    VERSION=$(awk '
        /^\[workspace\.package\]/ { section = 1; next }
        /^\[/ { section = 0 }
        section && /^version[[:space:]]*=/ {
            sub(/^version[[:space:]]*=[[:space:]]*"/, "")
            sub(/".*$/, "")
            print
            exit
        }
    ' "$ROOT/Cargo.toml")
fi
printf '%s\n' "$VERSION" | grep -Eq '^(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)$' \
    || fail "invalid version: $VERSION"

# Published names have no version; the update package and feed name it.
APPIMAGE=$ARTIFACTS/rc2fx-linux-x64.AppImage
NUPKG=$ARTIFACTS/io.github.ibanks42.rc2fx-$VERSION-linux-full.nupkg
FEED=$ARTIFACTS/releases.linux.json
SOURCES=$ARTIFACTS/rc2fx-$VERSION-appimage-runtime-sources.tar.gz
SUMS=$ARTIFACTS/SHA256SUMS

check_artifact() {
    path=$1
    [ -f "$path" ] || fail "missing exact artifact: $path"
    [ ! -L "$path" ] || fail "artifact is a symlink: $path"
    [ -s "$path" ] || fail "artifact is empty: $path"
    [ -r "$path" ] || fail "artifact is not readable: $path"
}

check_artifact "$APPIMAGE"
check_artifact "$NUPKG"
check_artifact "$FEED"

# A stale, differently versioned file must never be picked up by a glob.
for path in "$ARTIFACTS"/rc2fx*.AppImage "$ARTIFACTS"/io.github.ibanks42.rc2fx-*-linux-full.nupkg; do
    [ -e "$path" ] || continue
    [ "$path" = "$APPIMAGE" ] || [ "$path" = "$NUPKG" ] \
        || fail "stale artifact in $ARTIFACTS: $(basename "$path")"
done

magic() {
    od -An -tx1 -N"$2" "$1" | tr -d ' \n'
}
[ "$(magic "$APPIMAGE" 4)" = '7f454c46' ] || fail "AppImage does not start with an ELF header"
[ "$(magic "$APPIMAGE" 11 | cut -c17-22)" = '414902' ] \
    || fail "AppImage is not type 2"
[ -f "$SUMS" ] || fail "missing SHA256SUMS beside the artifacts"

verify_sum() {
    name=$1
    path=$2
    lines=$(awk -v name="$name" '$2 == name { print $1 }' "$SUMS")
    [ -n "$lines" ] || fail "SHA256SUMS has no entry for $name"
    [ "$(printf '%s\n' "$lines" | grep -c .)" -eq 1 ] \
        || fail "SHA256SUMS has more than one entry for $name"
    actual=$(sha256sum "$path" | awk '{ print $1 }')
    [ "$actual" = "$lines" ] || fail "SHA256SUMS does not match $name"
}
verify_sum "$(basename "$APPIMAGE")" "$APPIMAGE"
verify_sum "$(basename "$NUPKG")" "$NUPKG"
verify_sum "$(basename "$FEED")" "$FEED"
# The app downloads the package the feed names, checks its digest, and puts
# the AppImage inside it in place of the running one: it must be this one.
python3 - "$FEED" "$NUPKG" "$APPIMAGE" "$VERSION" <<'PY' || fail "the update feed or package does not match the AppImage"
import hashlib, json, sys, zipfile
from pathlib import Path
feed, nupkg, appimage, version = sys.argv[1:]
assets = json.loads(Path(feed).read_text())["Assets"]
digest = hashlib.sha256(Path(nupkg).read_bytes()).hexdigest()
assert [(a["PackageId"], a["Version"], a["Type"], a["FileName"], a["SHA256"].lower()) for a in assets] == [
    ("io.github.ibanks42.rc2fx", version, "Full", Path(nupkg).name, digest)], assets
with zipfile.ZipFile(nupkg) as package:
    assert package.read("lib/app/io.github.ibanks42.rc2fx.AppImage") == Path(appimage).read_bytes()
PY
check_artifact "$SOURCES"
verify_sum "$(basename "$SOURCES")" "$SOURCES"
tar -tzf "$SOURCES" | grep -Eq '(^|/)SOURCES\.json$' \
    || fail "AppImage runtime sources have no SOURCES.json"

case "$RUNTIME" in
    auto)
        if command -v podman >/dev/null 2>&1; then
            RUNTIME=podman
        elif command -v docker >/dev/null 2>&1; then
            RUNTIME=docker
        else
            fail 'podman or docker is required to check Linux packages'
        fi
        ;;
    podman|docker) ;;
    *) fail "unsupported container runtime: $RUNTIME" ;;
esac
command -v "$RUNTIME" >/dev/null 2>&1 || fail "$RUNTIME is not installed"
"$RUNTIME" info >/dev/null 2>&1 || fail "$RUNTIME is not usable by this user"

STAGE=$(mktemp -d)
trap 'rm -rf "$STAGE"' EXIT HUP INT TERM
mkdir -p "$STAGE/artifacts" "$STAGE/scripts"
cp "$APPIMAGE" "$STAGE/artifacts/"
for script in test-appimage.sh test-embedded-envelope.sh; do
    cp "$ROOT/.github/scripts/$script" "$STAGE/scripts/"
done

run_check() {
    image=$1
    script=$2
    artifact=$3
    printf '\n=== %s: %s (%s) ===\n' "$image" "$script" "$artifact"
    # SC2016: the container command is deliberately single-quoted; $work,
    # $CHECK_SCRIPT and friends expand inside the container, not on the host.
    # shellcheck disable=SC2016
    if ! tar -cf - -C "$STAGE" . | "$RUNTIME" run --rm -i \
        -e CHECK_SCRIPT="$script" \
        -e CHECK_ARTIFACT="$artifact" \
        -e CHECK_VERSION="$VERSION" \
        "$image" sh -ec '
            set -eu
            work=$(mktemp -d)
            trap "rm -rf \"$work\"" EXIT
            tar -xf - -C "$work"
            exec sh "$work/scripts/$CHECK_SCRIPT" \
                "$work/artifacts/$CHECK_ARTIFACT" "$CHECK_VERSION"
        '
    then
        fail "$image failed for $artifact"
    fi
}

echo "Checking $VERSION artifacts in $ARTIFACTS with $RUNTIME"
for image in $APPIMAGE_IMAGES; do
    run_check "$image" test-appimage.sh "$(basename "$APPIMAGE")"
done

echo
echo "All Linux package checks passed for $VERSION."
