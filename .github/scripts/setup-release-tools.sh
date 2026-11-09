#!/usr/bin/env bash
# Verified Linux build tools; the workflow caches only this tool directory.
# The AppImage itself is made by vpk (setup-vpk.sh), which brings its own
# AppImage runtime and needs mksquashfs (squashfs-tools) on the PATH.
set -euo pipefail
[[ $(uname -s) == Linux && $(uname -m) == x86_64 ]]
base=${RC2FX_TOOLS_DIR:-$HOME/.cache/rc2fx-release-tools}
mkdir -p "$base"
temporary=$(mktemp -d)
trap 'rm -rf "$temporary"' EXIT
fetch() {
    local url=$1 sha=$2 output=$3
    curl --fail --location --retry 3 --proto '=https' --proto-redir '=https' "$url" -o "$output"
    printf '%s  %s\n' "$sha" "$output" | sha256sum -c -
}
case ${1:-} in
    zig)
        destination=$base/zig-0.16.0
        if [[ ! -x $destination/zig ]]; then
            fetch https://ziglang.org/download/0.16.0/zig-x86_64-linux-0.16.0.tar.xz \
                70e49664a74374b48b51e6f3fdfbf437f6395d42509050588bd49abe52ba3d00 "$temporary/zig.tar.xz"
            mkdir "$temporary/zig"
            tar -xJf "$temporary/zig.tar.xz" --strip-components=1 -C "$temporary/zig"
            rm -rf "$destination"
            mv "$temporary/zig" "$destination"
        fi
        [[ $("$destination/zig" version) == 0.16.0 ]]
        if [[ -n ${GITHUB_PATH:-} ]]; then printf '%s\n' "$destination" >> "$GITHUB_PATH"; fi
        printf 'Zig ready: %s\n' "$destination"
        ;;
    appimage)
        destination=$base/appimage
        mkdir -p "$destination"
        extract_tool() {
            local name=$1 url=$2 sha=$3
            if [[ ! -x $destination/$name/AppRun ]]; then
                fetch "$url" "$sha" "$temporary/$name.AppImage"
                chmod +x "$temporary/$name.AppImage"
                mkdir "$temporary/$name"
                (cd "$temporary/$name" && "$temporary/$name.AppImage" --appimage-extract >/dev/null)
                rm -rf "$destination/$name"
                mv "$temporary/$name/squashfs-root" "$destination/$name"
            fi
        }
        extract_tool linuxdeploy \
            https://github.com/linuxdeploy/linuxdeploy/releases/download/1-alpha-20251107-1/linuxdeploy-x86_64.AppImage \
            c20cd71e3a4e3b80c3483cef793cda3f4e990aca14014d23c544ca3ce1270b4d
        patcher=$destination/patchelf-0.19.3
        if [[ ! -x $patcher/bin/patchelf ]]; then
            fetch https://github.com/NixOS/patchelf/releases/download/0.19.3/patchelf-0.19.3-x86_64.tar.gz \
                38861b171079e440b25ac86483284f690d0ee7921dba98b0c8fe04a9b0a11667 "$temporary/patchelf.tar.gz"
            mkdir -p "$patcher"
            tar -xzf "$temporary/patchelf.tar.gz" -C "$patcher"
        fi
        if [[ -n ${GITHUB_ENV:-} ]]; then
            printf 'RC2FX_LINUXDEPLOY=%s/linuxdeploy/AppRun\n' "$destination" >> "$GITHUB_ENV"
            printf 'RC2FX_PATCHELF=%s/bin/patchelf\n' "$patcher" >> "$GITHUB_ENV"
        fi
        printf 'AppImage tools ready: %s\n' "$destination"
        ;;
    *) echo 'usage: setup-release-tools.sh zig|appimage' >&2; exit 2 ;;
esac
