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
  (`desktop/scripts/fetch-gpuikit.sh`). `./xtask app` copies its library
  beside the executable, which finds it there.
- Linux: a graphical session and a Vulkan driver (see
  [Linux setup](../../docs/developer/linux.md)).

See [Building from source](../../docs/developer/building.md) for the rest.

## Connecting

The app looks for the mixer on USB 1 and retries until it finds one. A stock
mixer shows **Install RC2FX**. A mixer running RC2FX opens on the Overview,
which has a strip for each show fader except SMART Pads with its live level,
then the headphones and outputs, then any processed channel that isn't on a
fader.

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

- Mixer: the connection, the installer, and the **Processing** switch for all
  of RC2FX.
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
