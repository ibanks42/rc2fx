#!/bin/bash
# Installed as /usr/sbin/rc_audio_mixer so audio-start.sh hits us before
# /usr/bin/rc_audio_mixer. Hands off to rc2fx.sh launch (preferring a RAM dev
# build in /run/rc2fx-dev over /opt/rc2fx), or falls straight through to the
# stock mixer.
#
# If an in-place update left /opt/.rc2fx-update-pending behind and its driver
# is no longer running (e.g. after a power cut), boot from /opt/.rc2fx-previous.
previous=/opt/.rc2fx-previous
driver_alive() {
	local p s
	read -r p s </run/rc2fx-update/driver 2>/dev/null || return 1
	[[ $p =~ ^[0-9]+$ ]] || return 1
	grep -qa 'update-commit' "/proc/$p/cmdline" 2>/dev/null
}
if test -f /opt/.rc2fx-update-pending && test -f "$previous/rc2fx.sh" &&
	grep -q '^  launch) ' "$previous/rc2fx.sh" && ! driver_alive; then
	exec /bin/bash "$previous/rc2fx.sh" launch "$@"
fi
for s in /run/rc2fx-dev/rc2fx.sh /opt/rc2fx/rc2fx.sh; do
	if test -f "$s" && grep -q '^  launch) ' "$s"; then exec /bin/bash "$s" launch "$@"; fi
done
exec /usr/bin/rc_audio_mixer "$@"
