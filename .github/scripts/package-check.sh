#!/usr/bin/env bash
# `just package-check`, and the release's Linux build: build the AppImage from
# the working tree in the AlmaLinux 8 container (glibc 2.28), then check it in
# clean containers of several distributions. Everything stays under
# target/package-check. Needs no device, display or GitHub run.
set -euo pipefail
[[ $(uname -s) == Linux && $(uname -m) == x86_64 ]] || {
    echo 'package-check needs x86-64 Linux' >&2
    exit 1
}
root=$(git -C "$(dirname "${BASH_SOURCE[0]}")" rev-parse --show-toplevel)
cd "$root"

engine=${CONTAINER_ENGINE:-}
if [[ -z $engine ]]; then
    if command -v podman >/dev/null; then engine=podman
    elif command -v docker >/dev/null; then engine=docker
    else
        echo 'package-check needs podman or docker' >&2
        exit 1
    fi
fi
# Build files on the mount must belong to the calling user. Rootless engines
# map container root to that user, except podman, which keep-id handles.
case $engine in
    podman) user=(--userns=keep-id) ;;
    docker)
        if [[ $(docker info --format '{{.SecurityOptions}}') == *rootless* ]]; then
            user=()
        else
            user=(--user "$(id -u):$(id -g)")
        fi
        ;;
    *) echo "unsupported CONTAINER_ENGINE $engine" >&2; exit 1 ;;
esac

scripts=.github/scripts
# CI passes the published build image (build-image.yml) for this tag; a local
# run builds it, refreshing its packages.
tag=$(sh "$scripts/build-image-tag.sh")
if [[ -n ${RC2FX_BUILD_IMAGE:-} ]]; then
    [[ $RC2FX_BUILD_IMAGE == *:"$tag" ]] || {
        echo "RC2FX_BUILD_IMAGE $RC2FX_BUILD_IMAGE is not the build image for tag $tag" >&2
        exit 1
    }
    image=$RC2FX_BUILD_IMAGE
    "$engine" pull "$image"
else
    rust=$(sed -n 's/^channel = "\(.*\)"$/\1/p' rust-toolchain.toml)
    image=localhost/rc2fx-package-check:$tag
    "$engine" build --build-arg "RUST_VERSION=$rust" --build-arg "PACKAGE_REFRESH=$(date +%s)" -t "$image" \
        -f "$scripts/package-check.Containerfile" "$scripts"
fi

check=$root/target/package-check
mkdir -p "$check/target" "$check/cache"
# Tracked and new files as they are now, minus deleted and ignored ones.
# Modification times are kept so the cached build only redoes what changed.
snapshot() {
    git ls-files -z --cached --others --exclude-standard |
        while IFS= read -r -d '' file; do
            if [[ -e $file || -L $file ]]; then printf '%s\0' "$file"; fi
        done |
        sort --zero-terminated |
        tar --create --null --no-recursion --files-from=- "$@"
}
# File names, contents and modes only: touching a file changes no result.
digest() {
    snapshot --mtime=@0 --owner=0 --group=0 --numeric-owner | sha256sum | cut -c1-16
}

# Setup skips tools it already has, so a changed pin needs its own directory.
tools=/work/cache/tools/$(sha256sum "$scripts/setup-release-tools.sh" | cut -c1-12)
source=$(digest)
head=$(git rev-parse --short HEAD)
changes=$(git status --short)
echo "Packaging $head, source digest $source, with these local changes:"
echo "${changes:-(none)}"

# shellcheck disable=SC2016 # $HOME belongs to the container.
snapshot |
    "$engine" run --rm -i "${user[@]}" --security-opt label=disable \
        -v "$check/target:/work/src/target" -v "$check/cache:/work/cache" \
        -e CARGO_HOME=/work/cache/cargo -e HOME=/work/cache/home \
        -e "RC2FX_TOOLS_DIR=$tools" -w /work/src \
        "$image" sh -ec 'mkdir -p "$HOME"
            tar --extract --no-same-owner --directory=/work/src
            exec bash .github/scripts/package-check-build.sh'

bash "$scripts/test-linux-packages.sh" --artifacts "$check/target/package" --runtime "$engine"
# The tests read their scripts from the live tree, and a build takes a while.
if [[ $(digest) != "$source" ]]; then
    echo "Package check is void: the source changed during the run (was digest $source)" >&2
    exit 1
fi
echo "Package check passed for $head, source digest $source${changes:+, with local changes}"
echo "Packages: $check/target/package"
