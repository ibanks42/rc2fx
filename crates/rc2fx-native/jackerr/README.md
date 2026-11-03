# JACK error recorder

`librc2fx_jackerr.so` is an interposer for the mixer's stock `jackd` that
records errors only. In the vendor build `jack_error` does nothing. The
recorder captures calls to it into a fixed 4096-slot ring in shared memory.
It doesn't capture audio, create JACK ports, run a processing callback, or
turn on jackd's callback or ALSA tracing.

The normal mixer build produces this library and `rc2fx_jackerr_snapshot`.
Both are required, checksummed files in the SSH installer, the release package
and the firmware-image installer. RØDE's executables are left unchanged. The
launcher integration is in `xtask/mixer/rc2fx.sh`.

## Startup

When both files are present and valid, the launcher starts the recorder as
part of the normal mixer start, including at boot. There is no RAM marker or
developer command to enable it. The launcher preloads the library from the
active bundle, normally `/opt/rc2fx`. It is not started on the first boot after
a firmware update or under the other conditions that fall back to stock.

Setting the persistent `data/jack-recorder-off` flag stops it from starting.

Starting the recorder means replacing the running jackd. The launcher checks
the original JACK arguments, environment, thread placement, PCM devices and
audio IRQ state. If the replacement fails, it tries a verified fallback to a
plain server, and it records the failure for the rest of the boot so it isn't
retried in a loop. A mapped library doesn't by itself mean the recorder is
working. Check its status.

## Storage

Recordings are kept in RAM only and are cleared on reboot. The recorder starts
again on the next boot.

- `/run/rc2fx-recorder/status`: the last launch's status.
- `/run/rc2fx-recorder/active`: the current attempt's path and the JACK PID and
  birth token.
- `run.XXXXXX/`: one directory per attempt, with inventories, server logs, a
  readiness snapshot and a ring directory.

The root and attempt directories are private. Earlier attempts are kept
without pruning or overwriting. The ring has a fixed size, and its decoder reports
wraparound, loss, and unpublished or torn records.

The 8 MiB check on the whole root applies when a new attempt starts. It doesn't
limit vendor stdout logs while running. The recorder doesn't rotate, truncate
or cap any file the real-time server writes.

## Reading records

From the computer, run `just mixer-xruns [SECONDS]` (default 60). It checks
that the recorder is attached to the live jackd, takes a snapshot at the start
and end of the window, and prints each record that arrived in between. It then
prints a summary by kind and by late client, and anything the ring couldn't
show. `just mixer-xruns 0` prints everything still in the ring for that jackd.

To take a snapshot by hand on the device, run the bundled snapshot program
with a new output file name:

```text
/opt/rc2fx/rc2fx_jackerr_snapshot RING_DIRECTORY NEW_SNAPSHOT_PATH
```

It reads published slots with sequence checks and writes a snapshot that isn't
modified afterwards. Copying the live ring directory doesn't give the same
result. Decode the snapshot on the computer with the decoder used by
`cargo xtask mixer xruns`, or with `tests/jackerr_ring.py`.

When reading records, keep track of the PID, the creation clock, the window
indices, unknown formats, and the loss and error counts. A late client's name
tells you where a failure was reported. It doesn't establish the root cause,
whether it was audible, how many distinct dropouts there were, or exactly how
long the incident lasted.

## Real-time behaviour

The error hook doesn't allocate, lock, format or do I/O. Its atomic
read-modify-write operations can retry, so it has no hard bound on execution
time. Its pages are prefaulted but not locked by this code.

## Tests

```sh
python3 -I -m unittest discover -s crates/rc2fx-native/jackerr/tests -p test_jackerr.py
python3 xtask/mixer/tests/recorder_launcher.py
python3 xtask/mixer/tests/install.py
cargo test -p rc2fx-installer
cargo test -p xtask
```
