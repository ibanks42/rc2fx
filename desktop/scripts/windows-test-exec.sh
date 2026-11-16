#!/usr/bin/env bash
# windows-test-exec.sh TEST-BINARY [ARGS...]: `go test -exec` for the UI
# packages on Windows. gpuikit.dll uses common controls 6, which Windows gives
# only to a program whose manifest asks for it, and a test binary has none
# (rc2fx.exe embeds one: desktop/internal/xtask/packaging). Without it the
# test binary does not start (0xc0000139, an entry point not found). This
# puts gpuikit's manifest beside the binary, then runs it.
#
#   go test -exec "bash $PWD/scripts/windows-test-exec.sh" ./app/... ./cmd/rc2fx
set -eu
here=$(cd "$(dirname "$0")" && pwd)
cp "$here/../third_party/gpuikit/windows/gpuikit.manifest" "$1.manifest"
exec "$@"
