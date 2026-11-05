#!/bin/sh
# Run only in a disposable Fedora container, with the RPM and expected version
# passed by test-linux-packages.sh.
#
# A real dnf transaction: dependency resolution, install, reinstall and remove.
# The package is unsigned because it is built locally; --nodeps and --assumeno
# are deliberately not used.
set -eu
test -f /.dockerenv || test -f /run/.containerenv
test "$#" -eq 2
RPM=$1
EXPECTED=$2
test -f "$RPM"
test -n "$EXPECTED"
case "$RPM" in
    */rc2fx-"$EXPECTED"-linux-x64.rpm) ;;
    *) echo "RPM path does not name version $EXPECTED: $RPM" >&2; exit 1 ;;
esac
# shellcheck source=.github/scripts/test-embedded-envelope.sh
. "$(dirname -- "$0")/test-embedded-envelope.sh"

say() { printf '\n== %s ==\n' "$1"; }

say "installing dependencies and $RPM"
# Install the package alone first: its declared dependencies, not the check
# tools, must provide the runtime libraries.
dnf install -y "$RPM"

say "package metadata"
test "$(rpm -q --qf '%{VERSION}' rc2fx)" = "$EXPECTED"
test "$(rpm -q --qf '%{ARCH}' rc2fx)" = "$(uname -m)"
test -z "$(rpm -V rc2fx)"
rpm -qi rc2fx

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

udevadm verify /usr/lib/udev/rules.d/70-rc2fx.rules
command -v pkexec
command -v udevadm
if rpm -ql rc2fx | grep -q '^/opt/'; then
    echo 'package contains a mixer payload outside its integration set' >&2
    exit 1
fi

say "dynamic libraries and the embedded mixer envelope"
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
dnf install -y desktop-file-utils
desktop-file-validate /usr/share/applications/rc2fx.desktop

say "reinstalling"
if ! dnf reinstall -y "$RPM"; then
    echo 'dnf reinstall of the local RPM failed' >&2
    exit 1
fi
test "$(rpm -q --qf '%{VERSION}' rc2fx)" = "$EXPECTED"
test -x /usr/bin/rc2fx

say "removing"
dnf remove -y rc2fx
if rpm -q rc2fx >/dev/null 2>&1; then
    echo 'rc2fx is still registered after removal' >&2
    exit 1
fi
for path in \
    /usr/bin/rc2fx \
    /usr/share/applications/rc2fx.desktop \
    /usr/share/icons/hicolor/scalable/apps/rc2fx.svg \
    /usr/lib/udev/rules.d/70-rc2fx.rules \
    /usr/share/licenses/rc2fx/LICENSES.html
do
    test ! -e "$path" || { echo "$path survived removal" >&2; exit 1; }
done

echo 'RPM installation, metadata, shared libraries, reinstallation and removal passed.'
