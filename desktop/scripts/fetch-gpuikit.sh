#!/usr/bin/env bash
# Usage: fetch-gpuikit.sh [GOOS-GOARCH | darwin-universal]
#
# Downloads the pinned gpuikit bindings release, verifies its SHA-256, and
# unpacks it into desktop/third_party/gpuikit (gitignored), where go.mod and
# `./xtask package` expect:
#   lib/libgpuikit_static.a   static library linked by package gk (plus
#                             libgpuikit_sys.so linker script on Linux)
#   go/                       Go bindings (gpuikit.dev)
#   THIRD_PARTY.md, licences/ bundled license notices
#
# Defaults to `go env GOOS`-`go env GOARCH`. macOS uses a single universal
# archive (arm64 + x86_64, macOS 11.0+) for darwin-universal, darwin-arm64,
# and darwin-amd64. The offscreen diagnostic library (RC2FX_GPUIKIT_DIAG) is
# not included here and must be built from the bindings repo.
#
# If the bindings repo is private, set GPUIKIT_TOKEN (or GH_TOKEN/GITHUB_TOKEN)
# to fetch via the GitHub API instead of the public release URL.
#
# sha_* vars are referenced dynamically via ${!sha_name}.
# shellcheck disable=SC2034,SC2020
set -euo pipefail

version=0.0.2
tag=v$version
repo=ibanks42/gpui-kit-bindings
# Pinned SHA-256 per platform asset; an empty value aborts the script.
sha_linux_x86_64=5eac9dc1e38d4570f8bc87eefb726e89175156aa05aa80182460dd4c01d8d4db
sha_macos_universal=b32b18832069d7350f02c7ddd4a856d93d739a69989fa99b19de1e6da58601e1
sha_windows_x86_64=959bc528e620e61a2ece5ef4ce745135b3d92218a9c82726c07a58924def0b33

[[ $# -le 1 ]] || { echo 'usage: fetch-gpuikit.sh [GOOS-GOARCH | darwin-universal]' >&2; exit 2; }
target=${1:-$(go env GOOS)-$(go env GOARCH)}
library=libgpuikit_static.a
case $target in
    linux-amd64) platform=linux-x86_64 ext=tar.zst ;;
    darwin-universal | darwin-arm64 | darwin-amd64) platform=macos-universal ext=tar.zst ;;
    windows-amd64) platform=windows-x86_64 ext=zip ;;
    *) echo "no gpuikit download for $target" >&2; exit 1 ;;
esac

here=$(cd "$(dirname "$0")" && pwd)
third_party=$(dirname "$here")/third_party
token=${GPUIKIT_TOKEN:-${GH_TOKEN:-${GITHUB_TOKEN:-}}}

# Stage next to the destination on disk; archives are several hundred MB and /tmp is often tmpfs.
mkdir -p "$third_party"
tmp=$(mktemp -d "$third_party/.fetch-gpuikit.XXXXXX")
trap 'rm -rf "$tmp"' EXIT

curl_https() { curl --fail --silent --show-error --location --retry 3 --proto '=https' --proto-redir '=https' "$@"; }
# Pass the auth header via stdin so the token never shows up in argv; curl strips it on cross-host redirects.
curl_auth() { printf 'header = "Authorization: Bearer %s"\n' "$token" | curl_https --config - "$@"; }

release_json=
download() {
    local name=$1 file=$2 id
    if [[ -z $token ]]; then
        curl_https "https://github.com/$repo/releases/download/$tag/$name" -o "$file"
        return
    fi
    if [[ -z $release_json ]]; then
        release_json=$tmp/release.json
        curl_auth -H 'Accept: application/vnd.github+json' \
            "https://api.github.com/repos/$repo/releases/tags/$tag" -o "$release_json"
    fi
    # Asset API URLs (.../releases/assets/ID) precede their "name" field in the release JSON.
    id=$(tr ',{}' '\n\n\n' < "$release_json" | awk -v name="$name" '
        match($0, /"url"[[:space:]]*:[[:space:]]*"https:\/\/api\.github\.com\/repos\/[^"]*\/releases\/assets\/[0-9]+"/) {
            id = $0; sub(/"[[:space:]]*$/, "", id); sub(/.*\//, "", id); next
        }
        $0 ~ /"name"[[:space:]]*:/ {
            value = $0; sub(/^[^:]*:[[:space:]]*"/, "", value); sub(/"[[:space:]]*$/, "", value)
            if (value == name && id != "") { print id; exit }
        }')
    [[ -n $id ]] || { echo "release $tag of $repo has no asset $name" >&2; exit 1; }
    curl_auth -H 'Accept: application/octet-stream' \
        "https://api.github.com/repos/$repo/releases/assets/$id" -o "$file"
}

fetch() {
    local dest=$1 name sha_name sha archive unpacked
    name=gpuikit-$version-$platform
    sha_name=sha_${platform//-/_}
    sha=${!sha_name}
    if [[ -z $sha ]]; then
        echo "fetch-gpuikit.sh: $sha_name is empty. Fill in the SHA-256 of $name.$ext from" >&2
        echo "https://github.com/$repo/releases/tag/$tag (checked against the bindings' build)" >&2
        echo "in desktop/scripts/fetch-gpuikit.sh, then run this again." >&2
        exit 1
    fi
    archive=$tmp/$name.$ext
    download "$name.$ext" "$archive"
    if command -v sha256sum > /dev/null; then
        printf '%s  %s\n' "$sha" "$archive" | sha256sum -c -
    else
        printf '%s  %s\n' "$sha" "$archive" | shasum -a 256 -c -
    fi
    mkdir "$tmp/x"
    case $ext in
        zip)
            if command -v unzip > /dev/null; then
                unzip -q "$archive" -d "$tmp/x"
            else
                tar -xf "$archive" -C "$tmp/x"
            fi
            ;;
        tar.zst)
            if command -v zstd > /dev/null; then
                zstd -dcq "$archive" | tar -xf - -C "$tmp/x"
            else
                tar -xf "$archive" -C "$tmp/x"
            fi
            ;;
    esac
    unpacked=$tmp/x/$name
    [[ -f $unpacked/lib/$library ]] || { echo "$name.$ext has no $name/lib/$library" >&2; exit 1; }
    [[ -f $unpacked/go/go.mod ]] || { echo "$name.$ext has no $name/go/go.mod" >&2; exit 1; }
    # Remove any existing directory or symlink before moving the new one into place.
    rm -rf "$dest"
    mv "$unpacked" "$dest"
    printf 'gpuikit %s ready in %s for %s\n' "$version" "$dest" "$target"
}

fetch "$third_party/gpuikit"
# Clean up the old diag download directory from earlier releases.
rm -rf "$third_party/gpuikit-diag"
