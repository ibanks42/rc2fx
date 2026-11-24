# JACK error recorder

`librc2fx_jackerr.so` is an interposer for the mixer's stock `jackd` that only
records errors. In RØDE's stock build, `jack_error` is a no-op; this library interposes only
`jack_error` and captures calls to it into a fixed 4096-slot ring buffer in
shared memory.

Standard mixer builds compile both `librc2fx_jackerr.so` and
`rc2fx_jackerr_snapshot`. Both are required, checksummed artifacts in the SSH
installer, release packages, and firmware-image installer. Launcher integration
lives in `mixer/scripts/rc2fx.sh`.

## Startup

Whenever both binaries are present and pass verification, the launcher attaches
the recorder automatically during normal mixer startup, including at boot. The
launcher preloads the library
from the active bundle (typically `/opt/rc2fx`). It stays off on the first boot
after a firmware update and under any other condition that triggers a stock
fallback.

Creating the persistent `data/jack-recorder-off` flag disables it.

Attaching the recorder requires restarting the running `jackd`. Before doing
so, the launcher inspects the original JACK arguments, environment, thread
placement, PCM devices, and audio IRQ state. If replacement fails, it attempts
a verified fallback to a plain `jackd` and records the failure for the rest of
the boot so it won't retry in a loop. Seeing the library mapped into memory
doesn't guarantee the recorder is healthy; always check its status.

## Storage

All recordings live in RAM and vanish on reboot. The recorder starts fresh on
the next boot.

- `/run/rc2fx-recorder/status`: status of the most recent launch attempt.
- `/run/rc2fx-recorder/active`: path to the active attempt directory, along
  with the JACK PID and birth token.
- `run.XXXXXX/`: one directory per attempt containing inventories, server logs,
  a readiness snapshot, and the ring directory.

Both the root directory and individual attempt directories are private. Older
attempts are retained without pruning or overwriting. Because the ring has a
fixed size, its decoder explicitly flags wraparound, lost entries, and
unpublished or torn records.

An 8 MiB size check on the root directory runs before starting a new attempt.
It does not cap vendor stdout logs while running, and the recorder never
rotates, truncates, or limits any file written by the real-time server.

## Reading records

From your computer, run `just mixer-xruns [SECONDS]` (default 60). It verifies
the recorder is attached to the live `jackd`, captures snapshots at the start
and end of the window, prints every record logged in between, and summarizes
counts by kind, late client, and any gaps the ring couldn't cover. Pass `0`
(`just mixer-xruns 0`) to dump everything still in the ring for the current
`jackd`.

To capture a snapshot manually on the mixer, run the bundled tool with a new
output path:

```text
/opt/rc2fx/rc2fx_jackerr_snapshot RING_DIRECTORY NEW_SNAPSHOT_PATH
```

It reads published slots with sequence checks and writes an immutable snapshot;
simply copying the live ring directory does not produce a consistent view.
Decode snapshots on your computer using `just mixer-xruns`'s decoder or
`tests/jackerr_ring.py`.

When analyzing records, check the PID, creation clock, window indices, unknown
formats, and loss/error counts. A late client name only shows where the fault
was reported; it does not prove root cause, audibility, how many distinct
dropouts happened, or the exact duration of an incident.

## Real-time behavior

The error hook never allocates, locks, formats strings, or performs I/O.
Because its atomic read-modify-write operations can retry under contention, it
does not have a hard upper bound on execution time. Its memory pages are
prefaulted, though not locked by this code.

## Tests

```sh
python3 -I -m unittest discover -s mixer/jackerr/tests -p test_jackerr.py
python3 mixer/scripts/tests/recorder_launcher.py
python3 mixer/scripts/tests/install.py
(cd desktop && go test ./installer/...)
(cd tools/xtask && go test ./...)
```
