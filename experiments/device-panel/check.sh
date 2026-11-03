#!/usr/bin/env bash
# Host logic only; no device, firmware, GUI, audio, or controller is started.
set -euo pipefail
root=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/../.." && pwd)
tmp=$(mktemp -d "${TMPDIR:-/tmp}/rc2fx-device-ui-check.XXXXXX")
trap 'rm -rf -- "$tmp"' EXIT
cc=${CC:-cc}
flags=(-std=c11 -O1 -g -fno-omit-frame-pointer -Wall -Wextra -Werror
       -idirafter "$root/.deps/stock-sysroot/usr/include"
       -idirafter "$root/.deps/stock-sysroot/usr/include/libdrm" -pthread)
case "${SANITIZE:-0}" in
  0) ;;
  1) flags+=(-fsanitize=address,undefined -fno-sanitize-recover=all) ;;
  *) echo 'SANITIZE must be 0 or 1' >&2; exit 1 ;;
esac
"$cc" "${flags[@]}" "$root/experiments/device-panel/input_test.c" \
  "$root/experiments/device-panel/control.c" -o "$tmp/input_test" -ldl
"$tmp/input_test"
"$cc" "${flags[@]}" "$root/experiments/device-panel/client_test.c" \
  -o "$tmp/client_test" -ldl
printf 'client_test: compiled (run separately against the local Rust test server)\n'
