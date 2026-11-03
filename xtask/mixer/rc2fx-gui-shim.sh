#!/bin/bash
# Installed by RC2FX as /usr/sbin/rc_gui_juce. RØDE's display loop
# (display-start.sh) starts the GUI by name with /usr/sbin ahead of /usr/bin,
# so this runs in its place, in the loop's own environment, and ends in an
# exec of the stock GUI: with RC2FX's partial-conversion module when a bundle
# is present and passes its checks, otherwise unchanged. A build run from RAM
# wins over the installed one until the next boot.
for s in /run/rc2fx-dev/rc2fx.sh /opt/rc2fx/rc2fx.sh; do
  if test -f "$s" && grep -q '^  gui-launch) ' "$s"; then exec /bin/bash "$s" gui-launch "$@"; fi
done
exec /usr/bin/rc_gui_juce "$@"
