# Desktop app

This is the RC2FX desktop app, written in Go on GPUI-kit through goui-kit
(packages `app`, its subpackages, and `cmd/rc2fx`). It communicates
with `rc2fxd` on the mixer over USB 1 with no background daemon. Once you close
the app, audio processing keeps running on the mixer.

```sh
just app        # ./xtask app: development build in target/debug, then run it
just desktop    # build target/release/rc2fx (rc2fx.exe on Windows)
```

## Build requirements

- Go at the version specified in `desktop/go.mod`, plus a C compiler for cgo:
  MinGW-w64 gcc on Windows, gcc (or clang) on Linux, or the Xcode command-line
  tools on macOS.
- The goui-kit static library for your platform in `desktop/third_party/goui-kit`
  (fetched by `just setup`, or `./xtask fetch-goui-kit TARGET` for CI or
  another platform), linked directly into the binary. Builds pass
  `-tags gkstatic` to select the static library; without it, package `gk` looks
  for the shared library at runtime.
- On Linux: a graphical desktop session and a working Vulkan driver (see
  [Linux setup](../../docs/developer/linux.md)).

See [Building from source](../../docs/developer/building.md) for full build
instructions.

## Connecting

The app scans USB 1 for a mixer and keeps retrying until it finds one. Until a
ready mixer connects, the start screen reflects whatever is on USB: no mixer, a
mixer without RC2FX (**Install RC2FX**, or **Fix the update bug only** on a Pro
II staying on RØDE's firmware), a Duo (requiring the "Install anyway" switch
first, since RC2FX is untested on Duo hardware), a mixer temporarily gone while
finishing an update, a mixer where RC2FX is still starting, a mixer sitting in
update mode (**Continue the install**), a mixer in storage mode (**Repair
mixer**, currently Linux-only), or multiple mixers connected at once.

Once a mixer running RC2FX connects, the app opens to the Overview. It shows a
strip with live meters for every fader in the current show (except SMART Pads),
followed by headphones and outputs, and finally any processed channels that
aren't currently assigned to a fader.

## Install, Update, Remove, Repair and the storage fix

Installing, updating, removing RC2FX, repairing the mixer, and fixing RØDE's
update bug all run as guided tasks. Each task takes over the window from
**Before you start** until **Done**, **Stop**, **Close**, or **Not now**,
regardless of whether the mixer disconnects along the way. The left sidebar
tracks each step, and the footer tells you whether it's safe to unplug. If a
step fails, the page explains what state the mixer is currently in, and **Try
again** resumes from whichever step matches that state rather than blindly
repeating the failed step. If the mixer stops responding for a few minutes, an
escape hatch appears. Mixer updates start only from **Settings › Versions**,
and **Settings › Mixer** holds **Remove RC2FX**.

RØDE's updater formats whichever system slot it writes to, and the first slot
is small enough that it gets formatted with 1 KB blocks, where the mixer can
occasionally lose file data. The storage fix (`rodecaster/autofix`) permanently
enlarges that slot using RØDE's own USB update mechanism: the mixer flashes
twice, first with RØDE firmware carrying a one-time script that resizes the
slot, and then with the firmware it should keep. It runs wherever flashes run
(inside the privileged helper on Linux). **Install RC2FX** runs it first on any
Pro II not yet running RC2FX, then flashes RC2FX on the second pass. On a mixer
already running RC2FX it is skipped, since RC2FX can't inspect the partition
table yet. If the fix fails, it doesn't block the install: the installer
continues with a normal flash and notes on the result page that the fix wasn't
applied. **Fix the update bug only** runs the fix by itself and leaves RØDE's
stock firmware intact.

Between the two flashes, the app waits for the mixer to report the fix result.
On Linux, once the mixer's USB has been up for 20 seconds, the script exposes
the result on a small disk in the mixer's normal-mode USB slot. The app reads
it right away, writes an acknowledgement back, ejects the disk (signaling the
script to finish), and moves straight to the second flash. If the disk vanishes
before the app finishes reading it, the script re-offers it up to three times
within two minutes. After that, the script writes the result to the update
drive instead, and the app reads it there after waiting roughly five minutes
from the mixer's return. On other operating systems, the app simply waits two
and a half minutes after the mixer returns and reads the result from the update
drive. In development builds, setting `RC2FX_DEV_FIX_DEBUG=1` tells the script
to leave a log and USB-slot trace on the mixer under
`/Application/update-staging/rcfix-debug/`.

The start screen and installer only treat a flash sent within the last 20
minutes as still in progress when they see either no mixer or a mixer in update
mode. If a mixer is back in normal mode (whether on stock firmware or RC2FX),
it's no longer treated as updating. The backup of the original layout saved by
the fix (`storage-fix-backups/` in the installer folder) is owned by the user
running the app.

Mixer updates start only from **Settings › Versions**, where the button is
labeled for what changes: **Update mixer software** when the connected mixer
accepts signed in-place updates and a newer compatible signed release is
available, **Update RØDE firmware** when only RØDE firmware changes, and
**Update RC2FX** otherwise. **Update mixer software** opens the in-place flow
directly with no full flash. To copy the file quickly, the mixer attaches a
temporary USB drive named **RC2FX** to RØDE's removable drive; if that isn't
possible, the app falls back to USB 1. Audio goes silent for a few seconds
while RC2FX restarts (and can stutter during the transfer if it falls back to
USB 1), and the mixer rolls back to the previous version automatically if the
new one fails to come up. **Reinstall firmware as well…** switches to the full
flash update.

After a fresh install, **Open Mic N** jumps straight to that channel's noise
suppression page with a one-time hint above the switch. Once dismissed or used,
the hint never comes back.

In `rc2fxdev` builds, `RC2FX_DEV_PAGE=flow:<state>` opens any task state using
synthetic data with no mixer, helper, or network required; valid states are
listed in `flow_fixtures_on.go` (for example `flow:B4`).
`RC2FX_DEV_AUTOFLASH_FIX=on|off` controls whether unattended autopilot flashes
(`RC2FX_DEV_AUTOFLASH`) run the storage fix first; when unset, it follows the
normal Install rule (only when RC2FX isn't running).

## Layout

The sidebar pins **Overview** at the top (also bound to Esc), lists channels in
the middle, and keeps **Settings** at the bottom.

Channels are ordered with processed channels first, then inputs assigned to
faders in the current show, then headphones and outputs. SMART Pads (source ID
11) are excluded from source lists and editor lookups, including cached
inventories, since RC2FX doesn't process pads and leaves RØDE's pad playback
untouched.

Clicking a channel opens its editor. The effect chain runs left to right: click
a stage to edit it, or drag one onto another to reorder. The **Processing**
switch in the header turns processing on for that channel. Processing is opt-in
per channel, and every stage on a new channel starts off.

## Effects

- High-pass filter.
- Noise suppression (DeepFilterNet3) with wet/dry blend and a voice gate. The
  gate offers a single strength slider (0% is off) or full threshold,
  reduction, attack, hold, and release controls under **Custom**.
- EQ with up to 32 bands, linked or independent stereo, and a draggable curve.
- Compressor.
- Gain.
- Sample-peak limiter.

Noise suppression can only be active on one target at a time: either one mono
microphone or the Chat voice mix. On Chat, routed microphone sources are summed
into a single model input while SMART Pads and playback bypass suppression and
gating in stereo; the voice stage runs before the stereo output chain. Moving
suppression to another target requires turning it off on the current target and
saving first.

EQ curves can be imported from APO / AutoEQ text or EasyEffects JSON (from a
file or the clipboard) and exported as EasyEffects JSON.

## Absent sources

If two consecutive complete assignment readings agree that an input is no
longer in the show, the runtime turns processing off for that input while
preserving its effect settings. The same check runs at startup. Moving a source
to a different fader preserves its identity, and failed or partial readings
leave processing untouched.

When a removed source returns to the show, it stays off until you re-enable it.
If you manually turn on an absent source, the runtime turns it back off once
its absence is confirmed. Outputs are never affected. An absent input that is
still processed remains accessible in the app while reconciliation is pending.

## Saving

There is no save button; edits save automatically. Every tweak goes straight to
the mixer's live audio immediately (via live edits), and full settings are
persisted about 2 seconds after you stop making changes. If the app closes
before then, the mixer saves whatever is currently playing. Mixers without live
editing (no `api/v1/live`) don't hear changes until the save lands, which fires
0.6 seconds after edits stop. Failed saves retry after 1 second, backing off up
to 30 seconds, with the error shown on the status line below the channel in the
chain rail.

That status line also shows whether the channel is saved or how long ago it
changed. **Revert** resets the channel to its state before the current round of
edits. Undo and redo (Ctrl+Z / Ctrl+Shift+Z, or Cmd on macOS, plus the inline
buttons) step through changes; rapid edits without a 2-second pause group into
a single undo step.

Changes that require restarting the audio engine (toggling noise suppression or
its lookahead, or reordering effects while suppression is on) ask for
confirmation first. **Apply** saves and restarts; **Undo the change** restores
what's currently playing.

Every save includes the settings revision. If the mixer's settings changed
elsewhere, the app fetches the latest state, replays your edits on top, and
saves. If your edits no longer apply (for example, they reference channels the
mixer no longer has), a banner offers **Reload mixer settings**. Closing the
app with unsaved edits prompts before quitting.

The app can also open and save full settings files on disk, and manage presets
stored on the mixer.

## Settings

**Settings** includes:

- **Mixer:** connection details, **Remove RC2FX**, and the global
  **Processing** switch.
- **Versions:** app, mixer software, and RØDE firmware versions, plus updates.
- **About:** licenses and third-party notices.
- **Presets:** presets stored on the mixer.
- **Files:** settings files on your computer.
- **Appearance.**
- **Developer mode.**
- **Diagnostics.**

On the Diagnostics page:

- **Open events** launches a separate window that polls the mixer's event log
  every 500 ms while open.
- **Show telemetry** (persisted across launches) displays engine state, timing,
  levels, latency, counters, and warnings for every processed path, along with
  **Show events…** and **Reset diagnostics…**.
- **Export diagnostics…** writes the mixer's diagnostics out to a file.

In `rc2fxdev` builds (like `just app`), running
`rc2fx --telemetry-preview [old|events]` opens the telemetry view or events
window with synthetic data and no mixer connected.
