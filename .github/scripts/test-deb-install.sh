#!/bin/sh
# Run only in a disposable Debian/Ubuntu container, with the DEB mounted read-only.
# No display, USB devices or host settings are needed for this package check.
set -eu
test -f /.dockerenv || test -f /run/.containerenv
test "$#" -eq 1
test -f "$1"
export DEBIAN_FRONTEND=noninteractive

apt-get update -qq
apt-get install -y --no-install-recommends "$1"
test "$(dpkg-query -W -f='${Status}' rc2fx)" = 'install ok installed'
test -z "$(dpkg --verify rc2fx)"
test -x /usr/bin/rc2fx
test -s /usr/share/applications/rc2fx.desktop
test -s /usr/share/icons/hicolor/scalable/apps/rc2fx.svg
test -s /usr/lib/udev/rules.d/70-rc2fx.rules
test -s /usr/share/licenses/rc2fx/LICENSES.html
command -v pkexec
command -v udevadm

# ldd can exit successfully even with an unresolved dependency or symbol version.
ldd /usr/bin/rc2fx > /tmp/rc2fx-ldd.txt 2>&1
cat /tmp/rc2fx-ldd.txt
if grep -E 'not found|version .* not found' /tmp/rc2fx-ldd.txt; then
    exit 1
fi

# Exercise maintainer scripts for installation, reinstallation and removal.
apt-get install -y --no-install-recommends --reinstall "$1"
apt-get purge -y rc2fx
test ! -e /usr/bin/rc2fx
test ! -e /usr/lib/udev/rules.d/70-rc2fx.rules
echo 'DEB installation, shared libraries, reinstallation and removal passed.'
