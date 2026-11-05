#!/bin/sh
# Run only in a disposable Debian/Ubuntu container, with the DEB and expected
# version passed by test-linux-packages.sh.
#
# This is a real package transaction: apt resolves and installs the package's
# dependencies, then reinstall and purge exercise the maintainer scripts. No
# display, USB device, FUSE mount or host setting is used.
set -eu
test -f /.dockerenv || test -f /run/.containerenv
test "$#" -eq 2
DEB=$1
EXPECTED=$2
test -f "$DEB"
test -n "$EXPECTED"
case "$DEB" in
    */rc2fx-"$EXPECTED"-linux-x64.deb) ;;
    *) echo "DEB path does not name version $EXPECTED: $DEB" >&2; exit 1 ;;
esac
# shellcheck source=.github/scripts/test-embedded-envelope.sh
. "$(dirname -- "$0")/test-embedded-envelope.sh"
export DEBIAN_FRONTEND=noninteractive

say() { printf '\n== %s ==\n' "$1"; }

say "installing dependencies and $DEB"
apt-get update -qq
# Install the package alone first: its declared dependencies, not the check
# tools, must provide the runtime libraries.
apt-get install -y --no-install-recommends "$DEB"

say "package metadata"
test "$(dpkg-query -W -f='${Status}' rc2fx)" = 'install ok installed'
test "$(dpkg-query -W -f='${Version}' rc2fx)" = "$EXPECTED"
test "$(dpkg-query -W -f='${Architecture}' rc2fx)" = "$(dpkg --print-architecture)"
test -z "$(dpkg --verify rc2fx)"
test -z "$(dpkg --audit)"

say "installed files and permissions"
check_file() {
    path=$1
    mode=$2
    test -f "$path" || { echo "missing $path" >&2; exit 1; }
    test "$(stat -c '%a' "$path")" = "$mode" || {
        echo "$path has mode $(stat -c '%a' "$path"), expected $mode" >&2
        exit 1
    }
    test "$(stat -c '%U:%G' "$path")" = 'root:root' || {
        echo "$path is not owned by root:root" >&2
        exit 1
    }
}
check_file /usr/bin/rc2fx 755
check_file /usr/share/applications/rc2fx.desktop 644
check_file /usr/share/icons/hicolor/scalable/apps/rc2fx.svg 644
check_file /usr/lib/udev/rules.d/70-rc2fx.rules 644
check_file /usr/share/licenses/rc2fx/LICENSES.html 644
test -x /usr/bin/rc2fx
test -s /usr/share/licenses/rc2fx/LICENSES.html
grep -qi 'MIT' /usr/share/licenses/rc2fx/LICENSES.html

# Application integration metadata, not just its presence.
udevadm verify /usr/lib/udev/rules.d/70-rc2fx.rules
command -v pkexec
command -v udevadm

# The distributed packages ship only integration files, never the mixer
# payload or repository documentation.
if dpkg-query -L rc2fx | grep -q '^/opt/'; then
    echo 'package contains a mixer payload outside its integration set' >&2
    exit 1
fi

say "dynamic libraries and the embedded mixer envelope"
# ldd exits 0 even when a dependency is unresolved or a symbol version is
# missing. -r also reports undefined symbols.
ldd -r /usr/bin/rc2fx > /tmp/rc2fx-ldd.txt 2>&1
cat /tmp/rc2fx-ldd.txt
if grep -E 'not found|undefined symbol|version .* not found' /tmp/rc2fx-ldd.txt; then
    echo 'unresolved library or symbol' >&2
    exit 1
fi
# libvulkan and Wayland are loaded with dlopen and cannot appear in ldd.
ldconfig -p | grep -q 'libvulkan\.so\.1'
ldconfig -p | grep -q 'libwayland-client\.so\.0'
expect_installer_helper_exit_2 /usr/bin/rc2fx --installer-helper
expect_embedded_envelope /usr/bin/rc2fx

say "desktop entry"
apt-get install -y --no-install-recommends desktop-file-utils
desktop-file-validate /usr/share/applications/rc2fx.desktop

say "reinstalling"
apt-get install -y --no-install-recommends --reinstall "$DEB"
test "$(dpkg-query -W -f='${Status}' rc2fx)" = 'install ok installed'
test -x /usr/bin/rc2fx

say "purging"
apt-get purge -y rc2fx
for path in \
    /usr/bin/rc2fx \
    /usr/share/applications/rc2fx.desktop \
    /usr/share/icons/hicolor/scalable/apps/rc2fx.svg \
    /usr/lib/udev/rules.d/70-rc2fx.rules \
    /usr/share/licenses/rc2fx/LICENSES.html
do
    test ! -e "$path" || { echo "$path survived purge" >&2; exit 1; }
done
if dpkg-query -W rc2fx >/dev/null 2>&1; then
    echo 'rc2fx is still registered after purge' >&2
    exit 1
fi

echo 'DEB installation, metadata, shared libraries, reinstallation and purge passed.'
