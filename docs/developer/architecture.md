# Architecture

```text
 computer                               RØDECaster Pro II
┌──────────────┐  USB 1 (vendor HID)   ┌────────────────────────────────────────┐
│ desktop app  │◄─────────────────────►│ rc_audio_mixer (RØDE, unmodified)      │
│  transport   │  reports 5 and 6      │  └─ librc2fx_native.so (LD_AUDIT)      │
│  installer   │                       │      ├─ JACK callback: 25 paths, DSP   │
└──────────────┘                       │      ├─ helper thread                  │
                                       │      ├─ suppression worker             │
                                       │      └─ HID read hook ──► rc2fxd       │
                                       │ rc2fxd: control API, settings store    │
                                       │ rc_gui_juce (RØDE) + screen module     │
                                       └────────────────────────────────────────┘
```

## Starting with the mixer

RØDE's audio launcher looks up `rc_audio_mixer` on `PATH` with `/usr/sbin` ahead
of `/usr/bin`. RC2FX drops a shim at `/usr/sbin/rc_audio_mixer` so it gets
picked up first. That shim invokes `rc2fx.sh launch`, which:

1. checks the firmware against the [catalog](../reference/firmware.md) and makes
   sure stock audio wasn't requested for this boot;
2. execs RØDE's real `/usr/bin/rc_audio_mixer` with `librc2fx_native.so` loaded
   via `LD_AUDIT`;
3. starts `rc2fx-supervisor`, which in turn spawns the `rc2fxd` controller.

While the engine is up, the launcher lifts the kernel's real-time CPU cap. If a
core hits that cap, the kernel throttles every real-time thread on it, including
the USB gadget. RC2FX's suppression worker already runs at a lower priority than
all of RØDE's RT threads. Once the engine exits, the default cap goes back in
place.

On the first boot after a firmware update, `/usr/bin/first-boot` is present
while the mixer reflashes its internal controllers. The shim detects that and
starts the stock engine without RC2FX.

On subsequent boots, until the check passes once, the launcher also runs a
background check of RØDE's files against the list the installer embedded in the
image. If a file is damaged and the other root partition has a clean copy, it
restores it from there. See
[Installer internals](../reference/installer.md#image-check).

The screen works the same way. A shim at `/usr/sbin/rc_gui_juce` launches RØDE's
screen binary with RC2FX's screen module if a valid bundle is present, and
launches it untouched otherwise. That module only changes how screen redraws are
rotated (see [rc2fx-gui](../crates/rc2fx-gui.md)).

## Inside the audio engine

`LD_AUDIT` lets `librc2fx_native.so` watch and redirect symbol bindings as the
engine loads. `LD_PRELOAD` isn't enough because JUCE loads `libjack` with
`dlopen`/`dlsym`. The library wraps a small set of `libjack` calls (callback and
xrun registration, buffer lookup, port registration, and client close). Once it
verifies the executable's hash and the instructions at the callback address, it
wraps RØDE's JACK process callback. If either check fails, the stock callback
runs unwrapped.

Audio runs in 112-sample cycles at 48 kHz (2.33 ms). The wrapped callback
handles the model's bound paths (25 on the Pro II, 23 on the Duo):

- Outputs (USB sends, headphones, monitors) are processed in place inside the
  callback after RØDE finishes its own work and before the callback returns.
- Sources (microphones and other inputs) are processed one cycle later on a
  helper thread using copies captured by the callback, leaving RØDE's raw
  buffers untouched. That helper runs on the audio cores one priority below the
  callback, which is where the 2.33 ms block shift in the latency numbers comes
  from.
- Noise suppression runs on its own worker thread (`SCHED_RR` priority 5 on CPU
  0), several cycles behind, fed by bounded lock-free queues. If a denoised
  block arrives late, the equally delayed dry signal plays instead.
- Paths with no active processing are just metered every eighth cycle.

The callback itself never locks, allocates, or waits.

For Chat Mix Out, the library taps the microphone contributions directly out of
RØDE's routing code after faders, mutes, and mix-minus. It runs one mono
denoiser and voice gate on those mics, delays the rest of the mix to match, and
sums them back together before the regular stereo effects. If those microphone
taps can't be captured cleanly, suppression on Chat Mix Out is refused instead
of trying to denoise the full mix.

See the [rc2fx-native](../crates/rc2fx-native.md) and
[rc2fx-suppression](../crates/rc2fx-suppression.md) chapters for the deeper
details.

## Falling back to stock

`rc2fx-supervisor` watches both the controller and the audio engine. If `rc2fxd`
exits, or if the engine doesn't report ready within 60 seconds, the supervisor
writes a stock-fallback request for the rest of the boot and terminates the
engine. RØDE's launcher immediately restarts it, and the shim sees the request
and starts stock audio. If the engine exits early twice in the same boot, the
same fallback kicks in. A RAM run (`just mixer-run`) disappears on the next
reboot anyway.

The supervisor itself is watched too. A panic inside it follows the same path to
stock, and if it crashes or gets killed, the launch shell writes the stock
request and kills the engine on its own.

All of this orchestration lives in `mixer/scripts/rc2fx.sh` under subcommands
like `launch`, `start`, `restart`, `stop`, `status`, and `logs`, which is where
mixer-side behavior changes go. Because the supervisor stays up alongside live
audio for the whole lifetime of the engine and has to recover when everything
else fails, it's its own tiny binary in `crates/rc2fx-supervisor`. It shares no
code with the controller and calls back into `rc2fx.sh` for rare actions like
stopping the engine.

## Settings flow

1. The desktop app edits a local copy of the settings and sends it along with
   the revision it started from (`PUT /api/v1/config`, documented in the
   [controller API](../reference/api.md)).
2. `rc2fxd` validates the payload, stages the update in the native library,
   writes `state.json`, and commits the staged change
   ([rc2fx-native-control](../crates/rc2fx-native-control.md)).
3. The new settings take effect on the next audio cycle with no restart, and
   meter, engine, and state events stream back to the app.

If the client's revision is stale, the write is rejected so it won't clobber a
newer update.

## USB control channel

The mixer already exposes a vendor HID interface on USB 1 for RØDE's own app.
RC2FX adds report IDs 5 and 6 to that HID collection by staging a modified
descriptor in RAM. A hook on the engine's HID read lets RØDE's reports pass
through untouched and routes RC2FX's reports to `rc2fxd`, without adding any new
USB function, endpoint, or audio channel. See
[USB control](../reference/usb-control.md) for the protocol and how it survives
restarts.

## Installing

The installer fetches RØDE's official firmware, verifies it, adds RC2FX's files
to the root filesystem image on your computer, and flashes the result through
RØDE's USB update mode. It never logs into a running mixer. See
[Installer internals](../reference/installer.md).

## Work in progress

- Touchscreen panel: a launcher button and settings UI right on the mixer's
  screen for picking presets and toggling processing, served by `rc2fxd` over a
  local socket. It compiles and has run on hardware from RAM, but isn't part of
  the installed firmware yet. See `experiments/device-panel/README.md`.
- RØDECaster Duo: implemented from firmware analysis and
  [not yet run on a Duo](../reference/duo.md).
