#!/bin/sh
# Runs inside a disposable container via test-linux-packages.sh to inspect an
# extracted AppImage (no FUSE mount, GUI launch, or device access).
set -eu
test -f /.dockerenv || test -f /run/.containerenv
test "$#" -eq 2
APPIMAGE=$1
EXPECTED=$2
test -f "$APPIMAGE"
test -n "$EXPECTED"
case "$APPIMAGE" in
    */rc2fx-linux-x64.AppImage) ;;
    *) echo "unexpected AppImage name: $APPIMAGE" >&2; exit 1 ;;
esac
# shellcheck source=.github/scripts/test-embedded-envelope.sh
. "$(dirname -- "$0")/test-embedded-envelope.sh"

say() { printf '\n== %s ==\n' "$1"; }

say "installing inspection tools"
if command -v apt-get >/dev/null 2>&1; then
    export DEBIAN_FRONTEND=noninteractive
    apt-get update -qq
    apt-get install -y --no-install-recommends \
        binutils file desktop-file-utils \
        libxcb1 libfontconfig1 libfreetype6 libwayland-client0 libvulkan1
elif command -v dnf >/dev/null 2>&1; then
    dnf install -y binutils file desktop-file-utils \
        libxcb fontconfig freetype libwayland-client vulkan-loader
else
    echo 'no apt-get or dnf in the AppImage check container' >&2
    exit 1
fi

work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
app="$work/rc2fx.AppImage"
cp "$APPIMAGE" "$app"
chmod 755 "$app"

say "AppImage header and extraction"
test "$(head -c 4 "$app" | od -An -tx1 | tr -d ' \n')" = '7f454c46'
test "$(dd if="$app" bs=1 skip=8 count=3 2>/dev/null | od -An -tx1 | tr -d ' \n')" = '414902'
(cd "$work" && ./rc2fx.AppImage --appimage-extract > /tmp/rc2fx-appimage-extract.log 2>&1)
root="$work/squashfs-root"
test -d "$root"

say "AppRun and application payload"
test -f "$root/AppRun"
test ! -L "$root/AppRun"
test -x "$root/AppRun"
sh -n "$root/AppRun"
# shellcheck disable=SC2016  # $APPDIR is literal in AppRun.
grep -q 'exec "$APPDIR/usr/bin/rc2fx"' "$root/AppRun"
test -x "$root/usr/bin/rc2fx"
file "$root/usr/bin/rc2fx" | grep -q 'ELF 64-bit'
test -f "$root/usr/share/applications/rc2fx.desktop"
test -f "$root/usr/share/icons/hicolor/scalable/apps/rc2fx.svg"
desktop-file-validate "$root/usr/share/applications/rc2fx.desktop"

say "Velopack updater and version"
# The AppImage filename is unversioned, so verify the version in vpk's sq.version manifest.
grep -q "<id>io.github.ibanks42.rc2fx</id>" "$root/usr/bin/sq.version"
grep -q "<version>$EXPECTED</version>" "$root/usr/bin/sq.version"
grep -q '<channel>linux</channel>' "$root/usr/bin/sq.version"
test -x "$root/usr/bin/UpdateNix"
file "$root/usr/bin/UpdateNix" | grep -q 'ELF 64-bit'

say "licenses and bundled libraries"
test -s "$root/usr/share/licenses/rc2fx/LICENSES.html"
grep -qi 'MIT' "$root/usr/share/licenses/rc2fx/LICENSES.html"
test -d "$root/usr/lib"
find "$root/usr/lib" -maxdepth 1 \( -type f -o -type l \) -name '*.so*' | grep -q .
# Every RUNPATH/RPATH entry must be relative to $ORIGIN (reject absolute build paths).
runpath=$(readelf -d "$root/usr/bin/rc2fx" \
    | sed -n 's/.*(\(RUNPATH\|RPATH\)).*\[\(.*\)\]/\2/p')
if [ -n "$runpath" ]; then
    old_ifs=$IFS
    IFS=:
    for entry in $runpath; do
        # shellcheck disable=SC2016  # $ORIGIN is a literal runpath token.
        case "$entry" in
            '$ORIGIN'*) ;;
            *) echo "AppImage executable has a non-relative RPATH/RUNPATH entry: $entry" >&2; exit 1 ;;
        esac
    done
    IFS=$old_ifs
fi

say "dynamic libraries and the embedded mixer envelope"
# Allow only core host libraries (not xkb, which must be bundled).
# libresolv comes from Go's cgo net package; libutil comes from velopack_libc.
# libfreetype.so.6 is linked by goui-kit; linuxdeploy does not bundle it because
# every desktop Linux system provides it, so the AppImage uses the host's copy.
baseline='libc.so.6 libm.so.6 libgcc_s.so.1 libxcb.so.1 libfontconfig.so.1 libfreetype.so.6 libwayland-client.so.0 libvulkan.so.1 libdl.so.2 libpthread.so.0 librt.so.1 libresolv.so.2 libutil.so.1 ld-linux-x86-64.so.2'
needed=$(readelf -d "$root/usr/bin/rc2fx" | sed -n 's/.*(NEEDED).*\[\(.*\)\]/\1/p')
for library in $needed; do
    if [ -e "$root/usr/lib/$library" ] || [ -L "$root/usr/lib/$library" ]; then
        continue
    fi
    case " $baseline " in
        *" $library "*) ;;
        *) echo "AppImage needs $library, but does not bundle it and it is not in the host baseline" >&2; exit 1 ;;
    esac
done
# Test AppRun's own library wiring before setting LD_LIBRARY_PATH.
# --installer-helper with no args exits 2 without touching devices, files, or GUI.
unset LD_LIBRARY_PATH
APPIMAGE_EXTRACT_AND_RUN=1 expect_installer_helper_exit_2 "$app" --installer-helper
export LD_LIBRARY_PATH="$root/usr/lib${LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}"
ldd -r "$root/usr/bin/rc2fx" > /tmp/rc2fx-ldd.txt 2>&1
cat /tmp/rc2fx-ldd.txt
if grep -E 'not found|undefined symbol|version .* not found' /tmp/rc2fx-ldd.txt; then
    echo 'unresolved library or symbol' >&2
    exit 1
fi
expect_embedded_envelope "$root/usr/bin/rc2fx"

# Mixer binaries belong inside the embedded payload, never as loose files in the AppImage.
if find "$root" -name 'librc2fx_native.so' -o -name 'rc2fxd' -o -name 'mixer-release.json' | grep -q .; then
    echo 'AppImage contains mixer payload sidecars' >&2
    exit 1
fi

echo 'AppImage extraction, AppRun, libraries, notices and envelope passed.'
