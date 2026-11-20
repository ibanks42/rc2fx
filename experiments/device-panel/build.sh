#!/usr/bin/env bash
# Builds the ACTUAL device GUI audit library. Does not upload or restart anything.
set -euo pipefail
root=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/../.." && pwd)
out="$root/target/device-ui"
sysroot="$root/.deps/stock-sysroot"
test -f "$sysroot/usr/include/linux/videodev2.h" || {
  echo 'Missing cached stock sysroot headers (.deps/stock-sysroot)' >&2; exit 1;
}
mkdir -p "$out"
zig cc -target aarch64-linux-gnu.2.35 -mcpu=cortex_a53 -mno-outline-atomics \
  -std=c11 -O2 -g -fPIC -shared -fvisibility=hidden -fno-omit-frame-pointer \
  -mbranch-protection=standard -Wall -Wextra -Werror \
  -isystem "$sysroot/usr/include" -isystem "$sysroot/usr/include/libdrm" \
  "$root/experiments/device-panel/rc2fx_ui.c" "$root/experiments/device-panel/control.c" \
  -o "$out/librc2fx_ui.so"
zig cc -target aarch64-linux-gnu.2.35 -mcpu=cortex_a53 -mno-outline-atomics \
  -std=c11 -O2 -fPIE -pie -Wall -Wextra -Werror -isystem "$sysroot/usr/include" \
  "$root/experiments/device-panel/status.c" -o "$out/rc2fx-ui-status"
# Stage the experiment's own RAM-only preview runner beside its binaries.
cp "$root/experiments/device-panel/ui.sh" "$out/ui.sh"
(cd "$out" && sha256sum librc2fx_ui.so rc2fx-ui-status ui.sh > SHA256SUMS)
printf 'Actual AArch64 GUI module built: %s\n' "$out/librc2fx_ui.so"
printf 'Not loaded. No mixer/audio/desktop process was changed.\n'
