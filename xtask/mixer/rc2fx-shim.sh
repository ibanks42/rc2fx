#!/bin/bash
# Installed by RC2FX as /usr/sbin/rc_audio_mixer. RØDE's audio launcher
# (audio-start.sh) starts the mixer by name with /usr/sbin ahead of /usr/bin,
# so this runs in its place, in the launcher's own environment, and ends in an
# exec of the stock executable: with RC2FX injected when a bundle is present
# and passes its checks, otherwise unchanged. A build run from RAM wins over
# the installed one until the next boot.
for s in /run/rc2fx-dev/rc2fx.sh /opt/rc2fx/rc2fx.sh; do
  if test -f "$s" && grep -q '^  launch) ' "$s"; then exec /bin/bash "$s" launch "$@"; fi
done
exec /usr/bin/rc_audio_mixer "$@"
