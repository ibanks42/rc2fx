#!/bin/bash
# Installed by RC2FX as /usr/sbin/rc_audio_mixer. RØDE's audio launcher
# (audio-start.sh) starts the mixer by name with /usr/sbin ahead of /usr/bin,
# so this runs in its place, in the launcher's own environment, and ends in an
# exec of the stock executable: with RC2FX injected when a bundle is present
# and passes its checks, otherwise unchanged. A build run from RAM wins over
# the installed one until the next boot.
#
# A signed in-place update that has not committed leaves
# /opt/.rc2fx-install-pending (docs/developer/mixer-releases.md). Unless its
# detached driver is still working, the last known-good bundle in
# /opt/.rc2fx-previous is what this boot starts.
previous=/opt/.rc2fx-previous
driver_alive() {
  local p s
  read -r p s < /run/rc2fx-update/driver 2>/dev/null || return 1
  [[ $p =~ ^[0-9]+$ ]] || return 1
  grep -qa 'update-commit' "/proc/$p/cmdline" 2>/dev/null
}
if test -f /opt/.rc2fx-install-pending && test -f "$previous/rc2fx.sh" \
   && grep -q '^  launch) ' "$previous/rc2fx.sh" && ! driver_alive; then
  exec /bin/bash "$previous/rc2fx.sh" launch "$@"
fi
for s in /run/rc2fx-dev/rc2fx.sh /opt/rc2fx/rc2fx.sh; do
  if test -f "$s" && grep -q '^  launch) ' "$s"; then exec /bin/bash "$s" launch "$@"; fi
done
exec /usr/bin/rc_audio_mixer "$@"
