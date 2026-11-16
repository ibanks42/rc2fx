#!/usr/bin/env bash
# fetch-velopack.sh [GOOS-GOARCH...]: Velopack's C library for builds with the
# `velopack` tag (desktop/app/selfupdate/vpkc). Fetches velopack_libc at
# exactly the version vpk packs with (.github/scripts/setup-vpk.sh), checks
# the zip against a pinned SHA-256 and lays it out in
# desktop/third_party/velopack (gitignored):
#
#   include/Velopack.h
#   linux-amd64/libvelopack_libc.a     (and linux-arm64)
#   darwin-arm64/libvelopack_libc.a    and darwin-amd64/libvelopack_libc.a:
#                                      target darwin-universal fetches both,
#                                      one per slice of the universal app
#   windows-amd64/velopack_libc.dll    ship beside rc2fx.exe
#   windows-amd64/velopack_libc.def    its exports, from Velopack.h
#   windows-amd64/libvelopack_libc.dll.a  MinGW import library, when dlltool
#                                      or llvm-dlltool is found (otherwise
#                                      GNU ld links the DLL itself)
#
# The Windows DLL is MSVC-built; cgo links it from MinGW through its plain C
# exports only, so the two C runtimes never meet. On macOS each slice links
# its architecture's static library; the zip's universal
# velopack_libc_osx.dylib is not used, since its install name is a path on
# Velopack's build machine. Targets default to this machine's `go env GOOS
# GOARCH`, except that macOS always gets darwin-universal.
set -euo pipefail

version=1.3.39-g8d2844f
sha=73c727362cc615d0423f434f949431818f7dbd85acadc7c301a58e5cdc2a3881

here=$(cd "$(dirname "$0")" && pwd)
out=$(dirname "$here")/third_party/velopack
targets=("$@")
if [[ ${#targets[@]} -eq 0 ]]; then
    targets=("$(go env GOOS)-$(go env GOARCH)")
    if [[ $(go env GOOS) == darwin ]]; then targets=(darwin-universal); fi
fi
# darwin-universal is both macOS architectures.
expanded=()
for target in "${targets[@]}"; do
    if [[ $target == darwin-universal ]]; then
        expanded+=(darwin-arm64 darwin-amd64)
    else
        expanded+=("$target")
    fi
done

tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
zip=$tmp/velopack_libc.zip
curl --fail --location --retry 3 --proto '=https' --proto-redir '=https' \
    "https://github.com/velopack/velopack/releases/download/$version/velopack_libc_$version.zip" -o "$zip"
if command -v sha256sum > /dev/null; then
    printf '%s  %s\n' "$sha" "$zip" | sha256sum -c -
else
    printf '%s  %s\n' "$sha" "$zip" | shasum -a 256 -c -
fi
mkdir "$tmp/x"
if command -v unzip > /dev/null; then
    unzip -q "$zip" -d "$tmp/x"
else
    tar -xf "$zip" -C "$tmp/x"
fi

rm -rf "$out"
mkdir -p "$out/include"
cp "$tmp/x/include/Velopack.h" "$out/include/"
for target in "${expanded[@]}"; do
    case $target in
        linux-amd64) src=lib-static/velopack_libc_linux_x64_gnu.a ;;
        linux-arm64) src=lib-static/velopack_libc_linux_arm64_gnu.a ;;
        darwin-arm64) src=lib-static/velopack_libc_osx_arm64_gnu.a ;;
        darwin-amd64) src=lib-static/velopack_libc_osx_x64_gnu.a ;;
        windows-amd64) src=lib/velopack_libc_win_x64_msvc.dll ;;
        *) echo "no velopack_libc prebuilt for $target" >&2; exit 1 ;;
    esac
    mkdir -p "$out/$target"
    case $target in
        windows-*)
            # The import library the zip carries names velopack_libc.dll.
            cp "$tmp/x/$src" "$out/$target/velopack_libc.dll"
            {
                printf 'LIBRARY velopack_libc.dll\nEXPORTS\n'
                grep -oE '\bvpkc_[a-z_]+\(' "$out/include/Velopack.h" | tr -d '(' | sort -u | sed 's/^/    /'
            } > "$out/$target/velopack_libc.def"
            tool=$(command -v x86_64-w64-mingw32-dlltool || command -v dlltool || command -v llvm-dlltool || true)
            if [[ -n $tool ]]; then
                "$tool" -m i386:x86-64 -d "$out/$target/velopack_libc.def" -l "$out/$target/libvelopack_libc.dll.a"
            fi
            ;;
        *) cp "$tmp/x/$src" "$out/$target/libvelopack_libc.a" ;;
    esac
done
printf '%s\n' "$version" > "$out/VERSION"
printf 'velopack_libc %s ready in %s for %s\n' "$version" "$out" "${expanded[*]}"
