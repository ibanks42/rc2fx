#!/usr/bin/env bash
# Builds the Linux AppImage from the current working tree inside the AlmaLinux 8
# (glibc 2.28) container and tests it across distro containers. Output goes under
# target/package-check; requires no hardware, display, or GitHub Actions.
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
# Keep mounted build outputs owned by the host user.
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
# CI passes the prebuilt image from build-image.yml; local runs build it on demand.
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

# Under `docker --user`, an unmapped UID causes .NET (vpk) to fail creating named
# mutexes ("NuGet-Migrations"). Synthesize /etc/passwd and /etc/group entries for it.
if [[ $engine == docker && ${#user[@]} -gt 0 ]]; then
    uid=$(id -u) gid=$(id -g)
    identity=$check/identity
    mkdir -p "$identity"
    "$engine" run --rm "$image" cat /etc/passwd > "$identity/passwd"
    "$engine" run --rm "$image" cat /etc/group > "$identity/group"
    awk -F: -v id="$uid" '$3 == id { found = 1 } END { exit !found }' "$identity/passwd" ||
        printf 'rc2fx:x:%s:%s::/work/cache/home:/bin/sh\n' "$uid" "$gid" >> "$identity/passwd"
    awk -F: -v id="$gid" '$3 == id { found = 1 } END { exit !found }' "$identity/group" ||
        printf 'rc2fx:x:%s:\n' "$gid" >> "$identity/group"
    chmod 644 "$identity/passwd" "$identity/group"
    user+=(-v "$identity/passwd:/etc/passwd:ro" -v "$identity/group:/etc/group:ro")
fi
# Archive tracked and untracked non-ignored files, preserving mtimes for incremental builds.
snapshot() {
    git ls-files -z --cached --others --exclude-standard |
        while IFS= read -r -d '' file; do
            if [[ -e $file || -L $file ]]; then printf '%s\0' "$file"; fi
        done |
        sort --zero-terminated |
        tar --create --null --no-recursion --files-from=- "$@"
}
# Hash names, contents, and modes only (ignore mtimes/ownership).
digest() {
    snapshot --mtime=@0 --owner=0 --group=0 --numeric-owner | sha256sum | cut -c1-16
}

# Scope cached tools by script hash so updated pins don't reuse old binaries.
tools=/work/cache/tools/$(sha256sum "$scripts/setup-release-tools.sh" | cut -c1-12)
source=$(digest)
head=$(git rev-parse --short HEAD)
changes=$(git status --short)
echo "Packaging $head, source digest $source, with these local changes:"
echo "${changes:-(none)}"

# Use -modcacherw so the host user can clean up the mounted Go module cache.
# shellcheck disable=SC2016 # $HOME belongs to the container.
snapshot |
    "$engine" run --rm -i "${user[@]}" --security-opt label=disable \
        -v "$check/target:/work/src/target" -v "$check/cache:/work/cache" \
        -e CARGO_HOME=/work/cache/cargo -e HOME=/work/cache/home \
        -e GOCACHE=/work/cache/go-build -e GOMODCACHE=/work/cache/gomod \
        -e GOFLAGS=-modcacherw -e GOTOOLCHAIN=local \
        -e "RC2FX_TOOLS_DIR=$tools" -w /work/src \
        "$image" sh -ec 'mkdir -p "$HOME"
            tar --extract --no-same-owner --directory=/work/src
            exec bash .github/scripts/package-check-build.sh'

bash "$scripts/test-linux-packages.sh" --artifacts "$check/target/package" --runtime "$engine"
# Fail if the working tree changed mid-run, since the test scripts read from the live tree.
if [[ $(digest) != "$source" ]]; then
    echo "Package check is void: the source changed during the run (was digest $source)" >&2
    exit 1
fi
echo "Package check passed for $head, source digest $source${changes:+, with local changes}"
echo "Packages: $check/target/package"
