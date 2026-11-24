#!/bin/sh
# shellcheck disable=SC2174 # -m applies to the last directory, as intended
set -eu
# Override root for tests (see mixer/scripts/tests/install.py).
root=${RC2FX_INSTALL_ROOT:-}
up=$root/run/rc2fx-upload
opt=$root/opt
home=$opt/rc2fx
shim=$root/usr/sbin/rc_audio_mixer
gui_shim=$root/usr/sbin/rc_gui_juce
previous=$opt/.rc2fx-previous
pending=$opt/.rc2fx-install-pending
# Keep in sync with PayloadNames in desktop/installer/embed/format.go
# (lists_test.go checks both this list and the chmod 700 list below).
files="librc2fx_native.so rc2fxd rc2fx-supervisor rc2fx-firmware firmware-profiles.json librc2fx_gui.so
 librc2fx_jackerr.so rc2fx_jackerr_snapshot mixer-release.json
 SHA256SUMS rc2fx.sh rc2fx-shim.sh rc2fx-gui-shim.sh install.sh"
# Keep only .rc2fx-previous for rollback. Older installers archived every
# previous copy under .rc2fx-retained.* and eventually filled rootfs; prune
# those, sparing the oldest pre-GUI-shim install and anything with a data/ dir.
pre_gui() { ! grep -qF '  gui-launch) ' "$1/rc2fx.sh" 2>/dev/null; }
installed_at() { if test -f "$1/rc2fx.sh"; then echo "$1/rc2fx.sh"; else echo "$1"; fi; }
original() {
	keep=
	for dir in "$opt"/.rc2fx-retained.*; do
		# shellcheck disable=SC2015 # skip the entry when any test or pre_gui fails
		test -d "$dir" && ! test -L "$dir" && pre_gui "$dir" || continue
		if test -z "$keep" || test "$(installed_at "$dir")" -ot "$(installed_at "$keep")"; then keep=$dir; fi
	done
	echo "$keep"
}
prune_retained() {
	kept=$(original)
	for dir in "$opt"/.rc2fx-retained.*; do
		test -e "$dir" || test -L "$dir" || continue
		test "$dir" != "$kept" || continue
		if test -L "$dir" || ! test -d "$dir"; then
			echo "Left alone, not a directory: $dir" >&2
			continue
		fi
		if test -e "$dir/data"; then
			echo "Kept, it holds saved settings: $dir" >&2
			continue
		fi
		rm -rf "$dir"
	done
}
# Once the pending marker is cleared, nothing launches .rc2fx-previous. Drop it
# unless it holds saved settings or is the only pre-GUI-shim copy we have.
retire_previous() {
	test -d "$previous" && ! test -L "$previous" || return 0
	if test -e "$previous/data" || { pre_gui "$previous" && test -z "$(original)"; }; then
		test ! -e "$previous/data" || echo "Kept, it holds saved settings: $previous" >&2
		retained=$(mktemp -d "$opt/.rc2fx-retained.XXXXXX")
		rmdir "$retained"
		mv "$previous" "$retained"
	else
		rm -rf "$previous"
	fi
}
rollback() {
	test -f "$pending" || return 0
	/bin/bash "$home/rc2fx.sh" stop || return 1
	if test -d "$previous"; then
		for name in data logs; do test ! -e "$home/$name" || mv "$home/$name" "$previous/$name"; done
		failed=$(mktemp -d "$opt/.rc2fx-failed.XXXXXX")
		rmdir "$failed"
		mv "$home" "$failed"
		mv "$previous" "$home"
		if test -f "$home/.installer-autostart" && test -f "$home/rc2fx-shim.sh"; then
			cp "$home/rc2fx-shim.sh" "$shim"
			chmod 755 "$shim"
			if test -f "$home/rc2fx-gui-shim.sh"; then
				cp "$home/rc2fx-gui-shim.sh" "$gui_shim"
				chmod 755 "$gui_shim"
			else
				rm -f "$gui_shim"
			fi
		else
			rm -f "$shim" "$gui_shim"
		fi
		rm -f "$pending"
		if test -f "$home/rc2fx.sh" && test -x "$shim"; then /bin/bash "$home/rc2fx.sh" start --wait 240; fi
	else
		rm -f "$shim" "$gui_shim" "$pending"
	fi
}
case "${1:-install}" in
rollback)
	rollback
	exit
	;;
commit)
	test -f "$pending" || exit 0
	rm -f "$pending"
	retire_previous
	exit
	;;
install) ;;
*) exit 1 ;;
esac
if test -e "${RC2FX_FIRST_BOOT:-$root/usr/bin/first-boot}" || test -e "${RC2FX_PRODUCTION_BOOT:-$root/usr/bin/production-boot}"; then
	echo 'Refusing install: mixer is in its first boot after a firmware update' >&2
	exit 4
fi
for path in "$opt" "$home" "$home/data" "$home/logs" "$previous" "$pending" "$shim" "$gui_shim"; do
	if test -L "$path"; then
		echo "Linked install path refused: $path" >&2
		exit 1
	fi
done
mkdir -p "$opt"
if test -f "$pending"; then
	echo 'Earlier runtime transaction needs readiness checks or rollback' >&2
	exit 1
fi
prune_retained
bytes=0
for name in $files; do
	test -f "$up/$name" && ! test -L "$up/$name"
	bytes=$((bytes + $(wc -c <"$up/$name")))
done
# Check space for the staged bundle plus 16 MiB headroom (the existing runtime
# is renamed to .rc2fx-previous, not copied).
bundle=$(((bytes + 1023) / 1024))
headroom=16384
free=$(df -Pk "$opt" | awk 'NR == 2 {print $4}')
if test "$free" -lt $((bundle + headroom)); then
	echo "Not enough free space to install: $free KiB free on the filesystem of $opt, $((bundle + headroom)) KiB needed (the $bundle KiB bundle and $headroom KiB to spare)" >&2
	exit 1
fi
stage=$(mktemp -d "$opt/.rc2fx-stage.XXXXXX")
trap 'rm -rf "$stage"' EXIT
for name in $files; do
	cp "$up/$name" "$stage/$name"
done
if test -f "$up/bundle.sha256"; then cp "$up/bundle.sha256" "$stage/bundle.sha256"; fi
chmod 700 "$stage/rc2fxd" "$stage/rc2fx-supervisor" "$stage/rc2fx-firmware" "$stage/rc2fx_jackerr_snapshot" "$stage/rc2fx.sh" \
	"$stage/rc2fx-shim.sh" "$stage/rc2fx-gui-shim.sh" "$stage/install.sh"
(cd "$stage" && sha256sum -c SHA256SUMS >/dev/null)
"$stage/rc2fx-firmware" check-device "$stage/firmware-profiles.json" "$root/usr/bin/rc_audio_mixer" "$root/usr/bin/audio-start.sh" >/dev/null
# Remember whether the current install had autostart enabled. A running RC2FX
# stays alive out of the moved directory until we restart it below.
if test -d "$home" && test -x "$shim"; then touch "$home/.installer-autostart"; fi
# Stage is verified, so we can safely retire any leftover .rc2fx-previous.
retire_previous
if test -d "$home"; then mv "$home" "$previous"; fi
mv "$stage" "$home"
touch "$pending"
# Free the uploaded bundle from /run (tmpfs) now that the new copy and rollback
# are on disk.
rm -rf "$up"
# shellcheck disable=SC2154 # code is set inside the trap itself
trap 'code=$?; if test "$code" -ne 0; then rollback || echo "Rollback needs attention" >&2; fi; exit "$code"' EXIT
for name in data logs; do test ! -d "$previous/$name" || mv "$previous/$name" "$home/$name"; done
mkdir -p -m 700 "$home/logs"
# Both audio-start.sh and display-start.sh invoke binaries by bare name with
# /usr/sbin ahead of /usr/bin. The GUI shim won't take effect until the next
# boot because the running display-start.sh shell (bash) already hashed
# /usr/bin/rc_gui_juce.
cp "$home/rc2fx-shim.sh" "$shim"
chmod 755 "$shim"
cp "$home/rc2fx-gui-shim.sh" "$gui_shim"
chmod 755 "$gui_shim"
# Clean up init scripts from older RC2FX releases.
rm -f "$root/etc/rc5.d/S99rc2fx" "$root/etc/init.d/rc2fx"
# Clear any RAM dev build so it doesn't shadow the newly installed bundle.
rm -rf "$root/run/rc2fx-dev"
sync
status=0
/bin/bash "$home/rc2fx.sh" restart --wait 240 || status=$?
if test "$status" -eq 3; then
	# Existing audio-start.sh already hashed /usr/bin/rc_audio_mixer and won't
	# pick up the new shim until a reboot; commit the install as-is.
	rm -f "$pending"
	echo 'Installed; RC2FX starts with the mixer at the next power-on'
	exit 0
fi
test "$status" -eq 0
n=0
until test -S "$root/run/rc2fx/control.sock" && pgrep -x rc2fxd >/dev/null; do
	n=$((n + 1))
	if test "$n" -ge 90; then
		echo 'New runtime startup failed; rolling back' >&2
		exit 1
	fi
	sleep 1
done
echo 'Runtime started; readiness verification must commit the transaction'
