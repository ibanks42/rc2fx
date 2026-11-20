#!/usr/bin/env bash
# fetch-velopack.sh [GOOS-GOARCH...]
#
# Fetch velopack_libc at the version/SHA-256 pinned in config/velopack.json
# and stage it under desktop/third_party/velopack (gitignored):
#   include/Velopack.h
#   <linux-amd64|linux-arm64|darwin-arm64|darwin-amd64>/libvelopack_libc.a
#   windows-amd64/velopack_libc.dll, .def, and libvelopack_libc.dll.a (when dlltool is available)
#
# Windows links the MSVC DLL from MinGW via its C exports only so the two C
# runtimes stay isolated. macOS links per-arch static libs instead of the
# universal dylib (whose install name points to Velopack's build machine).
# Defaults to host GOOS-GOARCH, or darwin-universal on macOS.
set -euo pipefail

here=$(cd "$(dirname "$0")" && pwd)
velopack_config=$here/../../config/velopack.json
json_field() {
    sed -n "s/^[[:space:]]*\"$1\"[[:space:]]*:[[:space:]]*\"\([^\"]*\)\".*/\1/p" "$velopack_config" | head -n 1
}
version=$(json_field version)
sha=$(json_field libcSha256)
[[ -n $version && -n $sha ]] || { echo "could not read version/libcSha256 from $velopack_config" >&2; exit 1; }
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
            # Rename to match the LIBRARY name in the zip's import library.
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
