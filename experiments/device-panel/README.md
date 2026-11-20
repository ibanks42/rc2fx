# On-device launcher and settings panel (unfinished)

This is an unfinished experiment (not wired into the standard build or the firmware produced by the RC2FX installer), kept here for later completion. `rc2fxd` already exposes its local control socket, and `mixer/scripts/rc2fx.sh ui-preview` can run the panel directly from RAM.

The goal is an on-device launcher button and settings overlay right on the RØDECaster Pro II's touchscreen.

## Status

Progress so far falls into two milestones:

1. The first cut (a read-only panel with a touchscreen **Close** button) was verified on hardware by the user.
2. The current version (launcher button, in-process modal, and live settings controls) builds and passes host integration tests. A timed hardware trial on 2026-10-29 reached a real launcher page-flip acknowledgement and cleanly restored stock afterward. Because the user wasn't at the desk during that run, visual alignment, touch handling, open/close/reopen cycles, and live settings writes are still unverified on hardware.

## Controls

- An **RC2FX** button sits in the empty gap in the top bar between the clock and the monitor controls.
- Tapping it opens the settings overlay; **Close** returns to RØDE's screen inside the same process.
- Pick a saved preset and tap **Apply**, with paging across the full preset list.
- **Turn on** / **Turn off** toggles processing through the regular controller (including an explicit off during a fault when the controller permits it).
- Status shows whether an edit is offline, pending, or confirmed by the audio engine. Opening the panel doesn't mutate anything, and failed writes aren't retried automatically.

Under the hood, the panel shares the same settings store, revision/generation guards, and save/apply transactions as the desktop app. Writes against a stale revision are rejected so concurrent desktop edits aren't clobbered. There's no on-device EQ editor or preset creation; use the desktop app for those.

## Building and checking offline

None of these touch a mixer:

```sh
bash experiments/device-panel/build.sh
bash experiments/device-panel/check.sh
CC=clang SANITIZE=1 bash experiments/device-panel/check.sh
cargo test --locked -p rc2fx-device ui_control --lib
```

You'll need Zig and the cached `.deps/stock-sysroot` headers, plus Linux for the Rust tests. Outputs land in `target/device-ui/`: the AArch64 `librc2fx_ui.so`, the read-only `rc2fx-ui-status` utility, the launcher script, and their checksums.

The Rust tests drive the actual C client against a local `rc2fxd` controller socket, covering preset application, stale-revision rejection, and delayed replies. Input tests run against the real input code, all without hardware.

## RAM trial

Running a trial restarts the mixer's GUI (not audio). Only run it when a GUI restart is expected and no other mixer experiment is active. It leaves the desktop app and any unsaved drafts untouched.

```sh
export RC2FX_MIXER_KEY="$HOME/.ssh/rc2fx_rodecaster_pro_ii"
export RC2FX_MIXER_KNOWN_HOSTS="$PWD/.deps/flash-pro-ii/known_hosts"
bash experiments/device-panel/preview.sh --restart-gui 30
# Or keep the launcher running from RAM:
bash experiments/device-panel/preview.sh --restart-gui --keep-running
```

With `--keep-running`, the script prints a PID-checked stop command. **Close** hides the panel while leaving the launcher button active. Timed trials start their countdown after the first real page-flip ack, with a 30-second cap on startup itself.

Before touching anything, the script verifies:

- the mixer model,
- exact checksums of the GUI binary and display launcher,
- that this isn't a first boot or production boot,
- that audio has settled,
- that no other GUI injection is active.

It stages files purely in `/run`, starts a detached restore watchdog, pauses the stock display supervisor, stops the stock GUI, and relaunches it with `librc2fx_ui.so` while preserving its environment, working directory, umask, CPU affinity, cpuset, and nice value.

When the timer expires, the module faults, you stop it, or the owning session exits, the script tears down the injected GUI and helpers and resumes the stock supervisor. A `--keep-running` session renews a watchdog lease. Boot config, update files, and the vendor binary are never modified, and a reboot or audio restart clears the session. Firmware installer packaging hasn't been done yet.

Clean restoration depends on the vendor process exiting and its supervisor behaving normally. If something refuses to let go of the DRM device, the script reports `recovery-failed` and avoids starting a second display owner. Logs stay in the `/run` directory it prints. Don't run this across a firmware update, and don't rely on the dev launcher for recovery.

## Controller socket

Passing `--ui-socket /run/rc2fx/ui.sock` to `rc2fxd` (which the standard mixer supervisor does) opens an owner-only local Unix socket handled by the same coordinator as USB control. It introduces no TCP ports, separate store, direct native-transaction client, vendor SyncTree writes, or direct config file edits. See `crates/rc2fx-device/src/control/ui_control.rs` for the versioned wire format; both sides enforce packet and client limits.

A dedicated C worker thread handles all socket I/O off the render and input threads, sending the revision, generation, and exact target value from whatever screen the user tapped. Replies can indicate a change is saved on disk but not yet applied to audio. Lost write replies aren't replayed; only read-only snapshots auto-resume. If the socket is down, the launcher and **Close** still function while settings buttons disable themselves. `rc2fx-ui-status` queries the same socket read-only.

Updating `rc2fxd` itself normally restarts audio through the supervisor, so always coordinate with the user before doing that. Recent RAM builds may already include the socket, and uploading the UI bundle doesn't touch or restart `rc2fxd`.

## Rendering and input

The module only attaches to the Pro II GUI matching SHA-256 `aa92a92d384bdb24bc1cdb01615ac280d4d4867e661158df5c7f86ff98b605ca`. Imported call sites are pinned to the firmware map with zero instruction patching and no separately compiled JUCE widgets.

### Drawing

JUCE renders into a private anonymous shadow of its V4L2 OUTPUT mapping. Under the vendor repaint mutex, right before the real V4L2 `QBUF`, the module copies that clean shadow into the real CPU-owned source buffer and composites the launcher or panel on top, never writing to a queued buffer. Both dequeues must succeed without V4L2 error flags before presentation metadata is accepted, keeping RC2FX pixels out of partial stock repaints.

Tagged DRM flips publish immutable scenes via retained atomic tickets, with content epochs tracking paging, IDs, selection, control eligibility, and open/closed state while preserving the original callback selection and user data.

### Input

The module buffers complete evdev reports, tracks contacts in raw stream coordinates, and forwards stock reports byte for byte. Each contact is classified using its own coordinates without opening a second input reader. Handing input over requires:

- a real presentation,
- an empty queue and readiness result after the ack,
- a quiet contact snapshot,
- a final zero-readiness check.

Multi-contact gestures or replaced contacts are canceled, and any error immediately revokes settings commands and quarantines input.

### Frame pacing

Left alone, the stock frame loop sleeps before servicing the previous flip. A low-priority heartbeat thread on CPU 0 nudges the GUI's existing native event (4 Hz hidden, 10 Hz open) through the event's mapped memory without creating new sync primitives. Signal and wait are pinned at `0x8bf130` and `0x930100`, with condition and mutex offsets `+0x38` and `+8` verified at the imported wait. Discovery avoids blocking behind the heartbeat while holding the native mutex, and the event retires cleanly alongside condition destruction and source unmap. Forked child processes are filtered out by the original GUI PID so they can't touch copied mutexes or crash the parent session.

## Latest hardware trial

- Directory: `/run/rc2fx-ui.54wBXe`
- Time: 2026-10-29, 10:44–10:45 UTC
- Controller state: revision 325, generation 115, one preset, processing on and applied.
- Result: the source shadow and launcher reached a real DRM flip acknowledgement, then after 30 seconds the trial expired and restored the stock GUI. Audio (PID 2061) and `rc2fxd` (PID 2254) ran uninterrupted, with no settings changes requested.

This wasn't a visual/touch verification or proof of zero xruns; the user was away at work and deferred interactive testing.

Remaining items for an attended trial: modal handoff, visual placement, idle redraw rate, on-device settings edits, and CPU/audio overhead. All pinned addresses are documented in the [pinned frame and input map](ui-1.7.7-map.md).
