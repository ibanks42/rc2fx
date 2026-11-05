#!/bin/sh
# Check the Linux release artifacts in disposable containers.
#
# This is the single entry point for local and CI package checks. It selects
# exactly the current version's DEB, RPM and AppImage, verifies their checksums
# on the host, then streams a read-only copy into throwaway containers. No
# artifact is modified, no host bind mount is used, and no device, display or
# FUSE access is needed.
#
#   test-linux-packages.sh [DIR] [--artifacts DIR] [--version VERSION]
#                          [--runtime auto|podman|docker]
#
# Defaults: target/package, [workspace.package] version from Cargo.toml, and
# podman when available, otherwise docker. CONTAINER_ENGINE=podman|docker
# selects the engine from the environment, matching the package-check wrapper.
# The checks need network access for apt/dnf to resolve real dependencies.
set -eu

ROOT=$(CDPATH='' cd -- "$(dirname -- "$0")/../.." && pwd)
ARTIFACTS=
VERSION=
RUNTIME=${CONTAINER_ENGINE:-${RC2FX_CONTAINER_RUNTIME:-auto}}
DEB_IMAGES=${RC2FX_DEB_IMAGES:-"docker.io/library/ubuntu:24.04 docker.io/library/debian:13-slim"}
RPM_IMAGE=${RC2FX_RPM_IMAGE:-docker.io/library/fedora:44}
APPIMAGE_IMAGES=${RC2FX_APPIMAGE_IMAGES:-"docker.io/library/ubuntu:24.04 docker.io/library/debian:13-slim docker.io/library/fedora:44"}

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

DEB=$ARTIFACTS/rc2fx-$VERSION-linux-x64.deb
RPM=$ARTIFACTS/rc2fx-$VERSION-linux-x64.rpm
APPIMAGE=$ARTIFACTS/rc2fx-$VERSION-linux-x64.AppImage
SOURCES=$ARTIFACTS/rc2fx-$VERSION-appimage-runtime-sources.tar.gz
SUMS=$ARTIFACTS/SHA256SUMS

check_artifact() {
    path=$1
    [ -f "$path" ] || fail "missing exact artifact: $path"
    [ ! -L "$path" ] || fail "artifact is a symlink: $path"
    [ -s "$path" ] || fail "artifact is empty: $path"
    [ -r "$path" ] || fail "artifact is not readable: $path"
}

check_artifact "$DEB"
check_artifact "$RPM"
check_artifact "$APPIMAGE"

# A stale, differently versioned file must never be picked up by a glob.
for extension in deb rpm AppImage; do
    for path in "$ARTIFACTS"/rc2fx*-linux-x64."$extension"; do
        [ -e "$path" ] || continue
        [ "$path" = "$ARTIFACTS/rc2fx-$VERSION-linux-x64.$extension" ] \
            || fail "stale artifact in $ARTIFACTS: $(basename "$path")"
    done
done

magic() {
    od -An -tx1 -N"$2" "$1" | tr -d ' \n'
}
[ "$(magic "$DEB" 8)" = '213c617263683e0a' ] || fail "DEB does not start with !<arch>"
[ "$(magic "$RPM" 4)" = 'edabeedb' ] || fail "RPM does not start with its ar archive magic"
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
verify_sum "$(basename "$DEB")" "$DEB"
verify_sum "$(basename "$RPM")" "$RPM"
verify_sum "$(basename "$APPIMAGE")" "$APPIMAGE"
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
for path in "$DEB" "$RPM" "$APPIMAGE"; do
    cp "$path" "$STAGE/artifacts/"
done
for script in test-deb-install.sh test-rpm-install.sh test-appimage.sh test-embedded-envelope.sh; do
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
for image in $DEB_IMAGES; do
    run_check "$image" test-deb-install.sh "$(basename "$DEB")"
done
run_check "$RPM_IMAGE" test-rpm-install.sh "$(basename "$RPM")"
for image in $APPIMAGE_IMAGES; do
    run_check "$image" test-appimage.sh "$(basename "$APPIMAGE")"
done

echo
echo "All Linux package checks passed for $VERSION."
