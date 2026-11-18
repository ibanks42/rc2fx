# Desktop app

The RC2FX desktop app, written in Go on GPUI-kit through the gpuikit Go
bindings (packages `app` and its subpackages, and `cmd/rc2fx`). It talks to
`rc2fxd` on the mixer over USB 1 and has no background service. Processing
keeps running on the mixer after the app is closed.

```sh
just app        # ./xtask app: a development build in target/debug, then run it
just desktop    # build target/release/rc2fx (rc2fx.exe on Windows)
```

## Build requirements

- Go, the version `desktop/go.mod` names, and a C compiler for cgo:
  MinGW-w64 gcc on Windows, gcc (or clang) on Linux, the Xcode command-line
  tools on macOS.
- The gpuikit download for your platform in `desktop/third_party/gpuikit`
  (`desktop/scripts/fetch-gpuikit.sh`), a static library linked into the
  executable.
- Linux: a graphical session and a Vulkan driver (see
  [Linux setup](../../docs/developer/linux.md)).

See [Building from source](../../docs/developer/building.md) for the rest.

## Connecting

The app looks for the mixer on USB 1 and retries until it finds one. Until
then the first screen names what the app sees on USB: no mixer, a mixer
without RC2FX (**Install RC2FX**, or **Fix the update bug only** for a Pro
II that stays on RØDE's firmware), a Duo (an "Install anyway" switch first,
since RC2FX is untested there), a mixer gone while it finishes an update, one with RC2FX
still starting, a mixer waiting in update mode (**Continue the install**), a
mixer in storage mode (**Repair mixer**, on systems that can repair it; Linux
for now), or two mixers. A mixer running RC2FX opens on the Overview, which has a strip for
each show fader except SMART Pads with its live level, then the headphones
and outputs, then any processed channel that isn't on a fader.

## Install, Update, Remove, Repair and the storage fix

Installing, updating and removing RC2FX, repairing the mixer and fixing
RØDE's update bug are guided tasks: a page that stays in front of the window
from **Before you start** until **Done**, **Stop**, **Close** or **Not now**,
whether or not the mixer is connected meanwhile. The steps are listed down
the left; the footer says whether the mixer may be unplugged. After a failure
the page says what the app sees now, and **Try again** starts from the step
the mixer's state calls for, not from the step that failed. A wait with no
answer from the mixer offers a way out after a few minutes.
**Settings › Mixer** has **Update RC2FX** and **Remove RC2FX**.

RØDE's updater formats the system slot it is about to write, and the first
slot is small enough to get 1 KB blocks, on which the mixer sometimes loses
file data. The storage fix (`rodecaster/autofix`) grows that slot for good,
through RØDE's own USB update: the mixer is updated twice, once with RØDE's
firmware carrying a one-time script that makes the change, then once more
with the firmware it is to keep. It runs where flashes run (the helper, on
Linux). **Install RC2FX** runs it first on a Pro II that isn't running RC2FX,
and its second update is then the RC2FX firmware; on a mixer running RC2FX
it is skipped, since RC2FX can't report the partition table yet. A fix that
fails never blocks the install: it goes on with a normal flash, and the
result page says the fix wasn't applied. **Fix the update bug only** runs it
alone and leaves RØDE's firmware as it was.

Between the two updates the app waits for the mixer to confirm the fix. On
Linux the script offers its result on a small disk in the mixer's normal-mode
USB slot, once the mixer's USB has been up for 20 seconds; the app reads it as
soon as it appears, writes an acknowledgement onto it and ejects it (which
tells the script to finish), and goes straight on to the second update. A
disk that disappears before the app is done with it is offered again, up to
three times within two minutes; after that the script writes the result to
the update drive, and the app, having waited about five minutes from the
mixer's return (enough for all of that), reads it there. Elsewhere the app
waits until the mixer has been back for two and a half minutes and reads the
result from its update drive. Development builds started with
`RC2FX_DEV_FIX_DEBUG=1` have the script keep a log and a trace of the USB
slot on the mixer in `/Application/update-staging/rcfix-debug/`.

The first screen and the installer treat a flash sent in the last 20 minutes
as possibly still installing only while they see no mixer, or one in update
mode: a mixer back in normal mode, on RØDE's firmware or with RC2FX, is not
updating, whatever was sent. The
copy of the original layout the fix saves (`storage-fix-backups/` in the
installer's folder) belongs to the user who runs the app.

**Update RC2FX** on a mixer whose RC2FX installs signed releases itself is
the short in-place update: no flash, a brief pause in audio while RC2FX
restarts, and the mixer goes back to the version it had if the new one
doesn't start. **Reinstall firmware as well** switches to the full install.

After an install, **Open Mic N** opens that channel's noise suppression with
one hint line above its switch; once dismissed or done it never shows again.

In builds with the `rc2fxdev` tag, `RC2FX_DEV_PAGE=flow:<state>` opens one
state of these flows from made-up data, with no mixer, helper or network:
the states are listed in `flow_fixtures_on.go` (for example `flow:B4`).
`RC2FX_DEV_AUTOFLASH_FIX=on|off` sets whether the unattended flash
(`RC2FX_DEV_AUTOFLASH`) runs the storage fix first; unset, it runs only where
RC2FX isn't running, as Install decides.

## Layout

The sidebar has **Overview** at the top (Esc also opens it), the channel list
below it, and **Settings** at the bottom.

The channel list shows processed channels first, then sources assigned to
faders in the current show, then headphones and outputs. SMART Pads (source
ID 11) are left out of source views and editor lookup, including cached
inventories. RC2FX has no pad processing path, and RØDE's pad playback is
unchanged.

Selecting a channel opens its editor. The chain is drawn as a left-to-right
signal flow. Click a stage to edit it, or drag it onto another stage to
reorder. The **Processing** switch in the editor header turns the channel on.
Processing is opt-in per channel, and every stage of a new channel starts off.

## Effects

- High-pass filter.
- Noise suppression (DeepFilterNet3) with a wet/dry blend and a voice gate.
  The gate has one strength slider (0% is off), or separate threshold,
  reduction, attack, hold and release controls under **Custom**.
- EQ with up to 32 bands, linked or independent stereo, and a draggable
  response curve.
- Compressor.
- Gain.
- Sample-peak limiter.

Noise suppression can be on for one target at a time: one mono microphone or
the Chat voice mix. On Chat, the routed microphone sources are combined into
one model input. Smart Pads and playback skip suppression and gating and stay
stereo. The voice stage comes before the stereo output chain. To move
suppression to a different target, the old target's disable has to be applied
first.

EQ curves can be imported from APO / AutoEQ text or EasyEffects JSON, from a
file or the clipboard, and exported as EasyEffects JSON.

## Absent sources

When two complete assignment readings in a row agree that an input is not in
the show, the runtime turns processing off for that input. This also happens
at startup. The input's effects are kept. Moving a fader keeps the source's
identity. A failed or partial reading leaves processing as it is.

A source that comes back is not turned on automatically. If you turn on an
absent source by hand, the runtime turns it off again once its absence is
confirmed. Outputs are not affected. An absent input that is still processed
stays reachable in the app while confirmation or reconciliation is pending.

## Saving

**Save to mixer** checks the settings revision. If the mixer's settings were
changed somewhere else, the app asks whether to reload them or keep your
edits. Closing the app with unsaved edits asks first.

The app also opens and saves complete settings files, and saves, loads and
deletes presets stored on the mixer.

## Settings

**Settings** contains:

- Mixer: the connection, **Update RC2FX** and **Remove RC2FX**, and the
  **Processing** switch for all of RC2FX.
- Presets and settings files.
- Appearance.
- Screen conversion.
- **Diagnostics**.

On the Diagnostics page:

- **Open events** opens a separate window that reads the mixer's event list
  every 500 ms while it is open.
- **Show telemetry** (remembered between runs) adds the engine's state and
  timing, plus levels, delay, counters and warnings for every processed path,
  with **Show events…** and **Reset diagnostics…**.
- **Export diagnostics…** saves the mixer's diagnostics to a file.

In builds with the `rc2fxdebug` tag (development builds, as `just app` makes),
`rc2fx --telemetry-preview [old|events]` shows the telemetry section or the
events window with made-up data and no connection.
