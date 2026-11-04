#!/usr/bin/env bash
# Verified Linux build tools; the workflow caches only this tool directory.
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
        extract_tool appimagetool \
            https://github.com/AppImage/appimagetool/releases/download/1.9.1/appimagetool-x86_64.AppImage \
            ed4ce84f0d9caff66f50bcca6ff6f35aae54ce8135408b3fa33abfc3cb384eb0
        patcher=$destination/patchelf-0.19.3
        if [[ ! -x $patcher/bin/patchelf ]]; then
            fetch https://github.com/NixOS/patchelf/releases/download/0.19.3/patchelf-0.19.3-x86_64.tar.gz \
                38861b171079e440b25ac86483284f690d0ee7921dba98b0c8fe04a9b0a11667 "$temporary/patchelf.tar.gz"
            mkdir -p "$patcher"
            tar -xzf "$temporary/patchelf.tar.gz" -C "$patcher"
        fi
        nfpm=$destination/nfpm-2.48.0
        if [[ ! -x $nfpm/nfpm ]]; then
            fetch https://github.com/goreleaser/nfpm/releases/download/v2.48.0/nfpm_2.48.0_Linux_x86_64.tar.gz \
                6d7f9a28b2f397934403950fcbba83f598b08d02edbe79a0815b3b4897341750 "$temporary/nfpm.tar.gz"
            mkdir -p "$nfpm"
            tar -xzf "$temporary/nfpm.tar.gz" -C "$nfpm"
        fi
        runtime=$destination/runtime-x86_64
        runtime_sha=2fca8b443c92510f1483a883f60061ad09b46b978b2631c807cd873a47ec260d
        if ! printf '%s  %s\n' "$runtime_sha" "$runtime" | sha256sum -c - >/dev/null 2>&1; then
            fetch https://github.com/AppImage/type2-runtime/releases/download/20251108/runtime-x86_64 \
                "$runtime_sha" "$temporary/runtime"
            mv "$temporary/runtime" "$runtime"
        fi
        if [[ -n ${GITHUB_ENV:-} ]]; then
            printf 'RC2FX_LINUXDEPLOY=%s/linuxdeploy/AppRun\nRC2FX_APPIMAGETOOL=%s/appimagetool/AppRun\nRC2FX_APPIMAGE_RUNTIME=%s\n' \
                "$destination" "$destination" "$runtime" >> "$GITHUB_ENV"
            printf 'RC2FX_PATCHELF=%s/bin/patchelf\nRC2FX_NFPM=%s/nfpm\n' "$patcher" "$nfpm" >> "$GITHUB_ENV"
        fi
        printf 'AppImage tools ready: %s\n' "$destination"
        ;;
    *) echo 'usage: setup-release-tools.sh zig|appimage' >&2; exit 2 ;;
esac
