# On-device launcher and settings panel (unfinished)

This is an experiment. It is not part of the build or of the firmware the
RC2FX installer produces, and it is kept here to be finished later. `rc2fxd`
already serves its socket, and the launcher script can run it from RAM with
`xtask/mixer/rc2fx.sh ui-preview`.

The experiment adds a launcher and settings panel to the RØDECaster Pro II's
own screen.

## Status

There are two milestones, tested to different degrees:

1. The first version, a read-only panel with a touchscreen **Close** button,
   was confirmed on hardware by the user.
2. The current version (launcher, in-process modal and settings) builds and
   passes the host integration tests. A bounded hardware trial on 2026-10-29
   reached a real launcher page-flip acknowledgement and then restored stock.
   The user was away during that trial, so visual placement, touch, open,
   Close, reopen and live setting changes are still unverified.

## Controls

- An **RC2FX** button sits in the empty part of the top bar between the clock
  and the monitor controls.
- Tapping it opens the panel. **Close** returns to RØDE's screen in the same
  process.
- Select a saved preset and press **Apply**. Paging reaches the whole saved
  list.
- **Turn on** / **Turn off** switches processing through the existing
  controller. An explicit off during a fault is still available when the
  controller allows it.
- The panel shows whether a change is offline, pending, or confirmed by the
  audio engine. Opening the panel changes no settings, and failed writes are
  not retried automatically.

The panel uses the same settings store, revision and generation checks, and
save/apply transactions as the desktop app. A write based on a stale revision
is refused, so desktop edits made at the same time are kept. The panel has no
EQ editor and cannot create or edit presets. Do those on the desktop.

## Building and checking offline

These commands do not access the mixer:

```sh
bash experiments/device-panel/build.sh
bash experiments/device-panel/check.sh
CC=clang SANITIZE=1 bash experiments/device-panel/check.sh
cargo test --locked -p rc2fx-device --features native-pipeline ui_control --lib
```

They need Zig and the cached `.deps/stock-sysroot` headers. The build writes
to `target/device-ui/`: the AArch64 `librc2fx_ui.so`, the read-only
`rc2fx-ui-status` tool, the launcher script and their hashes.

The Rust tests run the real C client against a real local `rc2fxd` controller
socket. They cover preset apply, rejection of a stale revision, and a delayed
reply to a change. The input tests exercise the production input code. No test
accesses hardware.

## RAM trial

The trial restarts the mixer's GUI. It does not restart audio. Run it only
when the user expects a GUI restart and no other mixer experiment is active.
It does not touch the desktop window or its draft.

```sh
export RC2FX_MIXER_KEY="$HOME/.ssh/rc2fx_rodecaster_pro_ii"
export RC2FX_MIXER_KNOWN_HOSTS="$PWD/.deps/flash-pro-ii/known_hosts"
bash experiments/device-panel/preview.sh --restart-gui 30
# Or keep the launcher running from RAM:
bash experiments/device-panel/preview.sh --restart-gui --keep-running
```

The second form prints a stop command that checks the process identity by
PID. **Close** hides the panel and leaves the launcher running. A bounded
trial starts its timer after the first real page-flip acknowledgement, and
startup itself is limited to 30 seconds.

Before starting, the script checks:

- the mixer model,
- the exact GUI and display-launcher hashes,
- that this is neither a first boot nor a production boot,
- that audio has settled,
- that no GUI injection is already running.

It then stages files under `/run` only, starts a detached restore watchdog,
pauses the original display supervisor, stops the stock GUI and relaunches it
with this module. The relaunched GUI keeps the original environment, working
directory, umask, CPU affinity, cpuset and nice value.

When the trial expires, the module faults, the user stops it or the owner
goes away, the script stops the injected GUI and its helpers and resumes the
original launcher. A kept session renews a watchdog lease. Boot settings,
update files and the vendor executable are left alone. A reboot or a later
audio restart ends the development session. Packaging this in the firmware
installer has not been done.

Normal restoration relies on the vendor process exiting and its supervisor
working. If something keeps holding the DRM device, the script reports
`recovery-failed` and does not start a second display owner. Logs stay in the
RAM directory it prints. Do not run the trial across a firmware update, and
do not treat the development launcher as a reliable recovery mechanism.

## Controller socket

`rc2fxd --ui-socket /run/rc2fx/ui.sock` turns on an optional local listener,
accessible only to its owner. It is served by the same coordinator as USB
control, and the standard mixer supervisor passes the argument. The listener
adds no TCP port, second store, direct native-transaction client, vendor
SyncTree write or settings-file edit. The versioned compact wire format is
documented in `crates/rc2fx-device/src/ui_control.rs`, and both ends limit
packet and client counts.

A private C worker thread does all socket I/O away from rendering and input.
It sends the revision, generation and absolute intended value from the screen
the user touched. A reply can report that a change is saved but not yet
applied. Lost replies to changes are not replayed. Only read-only snapshots
are resumed. If the socket is unavailable, the launcher and **Close** still
work and the settings actions are disabled. `rc2fx-ui-status` reads the same
socket without changing anything.

Installing a controller update normally restarts audio through the current
supervisor. Do not do that without coordinating with the user and getting
approval. A current RAM build may already include the socket. The UI upload
command does not replace or restart `rc2fxd`.

## Rendering and input

The module accepts only the Pro II GUI with SHA-256
`aa92a92d384bdb24bc1cdb01615ac280d4d4867e661158df5c7f86ff98b605ca`.
Its imported call sites are pinned to the matching firmware map. It uses no
instruction patches and no separately compiled JUCE widgets.

### Drawing

JUCE paints into a private anonymous shadow of its OUTPUT mapping. Under the
vendor repaint mutex, just before the real V4L2 QBUF, the module copies that
clean image to the real CPU-owned source and draws the launcher or panel on
top. It writes only to a source that is not queued. Both dequeues have to
succeed without V4L2 error flags before presentation metadata is accepted, so
a partial stock repaint cannot contain RC2FX pixels.

Tagged DRM flips publish immutable scenes through retained atomic tickets.
Content epochs cover paging, IDs, selection and control eligibility as well
as whether the panel is open. The original callback selection and user data
are preserved.

### Input

The module buffers whole evdev reports, tracks contacts in stream
coordinates, and passes stock reports through byte for byte. It classifies
each contact with the coordinates that came with it, and it does not open a
second input reader. Handing input over requires:

- an actual presentation,
- an empty queue and readiness result after the acknowledgement,
- a quiet contact snapshot,
- a final zero-readiness check.

Actions with several contacts or a replaced contact are cancelled. Any failure
revokes settings commands and quarantines input straight away.

### Frame pacing

Left alone, the stock frame loop sleeps before it services a previous flip. A
low-priority heartbeat on CPU 0 signals the GUI's existing native event, at
4 Hz while hidden and 10 Hz while open, using the event's mapped bytes. The
module creates no synchronization objects of its own. Signal and wait are
pinned at `0x8bf130` and `0x930100`, and the condition and mutex offsets
`+0x38` and `+8` are validated at the imported wait. Discovery does not block
behind the heartbeat while holding the native mutex, and the event is retired
in step with condition destruction and source unmap. Forked helpers are
excluded by the original GUI PID, so they cannot touch copied GUI mutexes or
fault the parent's session.

## Latest hardware trial

- Directory: `/run/rc2fx-ui.54wBXe`
- Time: 2026-10-29, 10:44–10:45 UTC
- Controller state: revision 325, generation 115, one preset, processing on
  and applied.
- Result: the new source shadow and launcher reached a real DRM flip
  acknowledgement. The 30-second trial expired and the stock GUI resumed.
  Audio (PID 2061) and the controller (PID 2254) kept running unchanged, and
  no settings change was requested.

That trial was neither a visual or touch test nor evidence of zero underruns.
The user was at work and put off further device testing.

Still to test, in an attended trial: modal handoff, visual placement, idle
redraw behaviour, setting changes on the device, and CPU and audio impact.
The addresses used here are documented in the
[pinned frame and input map](ui-1.7.7-map.md).
