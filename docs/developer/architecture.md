# Architecture

```text
 computer                               RØDECaster Pro II
┌──────────────┐  USB 1 (vendor HID)   ┌────────────────────────────────────────┐
│ rc2fx-app    │◄─────────────────────►│ rc_audio_mixer (RØDE, unmodified)      │
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

RØDE's audio launcher starts `rc_audio_mixer` by name, with `/usr/sbin` before
`/usr/bin` on its `PATH`. RC2FX installs a shim at `/usr/sbin/rc_audio_mixer`,
which the launcher finds first. The shim runs `rc2fx.sh launch`, which:

1. checks the firmware against the [catalog](../reference/firmware.md) and
   checks that stock audio hasn't been requested during this boot;
2. execs RØDE's `/usr/bin/rc_audio_mixer` with `librc2fx_native.so` loaded
   through `LD_AUDIT`;
3. starts `rc2fx-supervisor`, which starts the `rc2fxd` controller.

While the engine runs, the launcher lifts the kernel's real-time CPU cap. When
a core reaches that cap, the kernel suspends every real-time thread on it,
including the USB gadget's. RC2FX's suppression worker already runs at a lower
priority than all of RØDE's real-time threads. The default cap is restored
when the engine exits.

During the first boot after a firmware update, `/usr/bin/first-boot` exists
and the mixer reflashes its controllers. The shim then starts the stock engine
without RC2FX.

The screen uses the same mechanism. A shim at `/usr/sbin/rc_gui_juce` starts
RØDE's screen program with RC2FX's screen module if a bundle is present and
passes its checks, and starts it unchanged otherwise. The module only changes
how large redraws are rotated (the
[screen conversion](../user/settings-and-diagnostics.md#screen-conversion)
setting).

## Inside the audio engine

`LD_AUDIT` lets the library observe and redirect symbol binding while the
engine loads. `LD_PRELOAD` is insufficient because JUCE loads libjack with
`dlopen`/`dlsym`. The library wraps a few libjack calls: callback and xrun
registration, buffer lookup, port registration and client close. It then wraps
RØDE's JACK process callback, after checking the executable's hash and the
instructions at the callback address. If either check fails, the stock
callback runs unwrapped.

Each cycle is 112 samples at 48 kHz (2.33 ms). The wrapped callback handles
the model's bound paths, 25 on the Pro II and 23 on the Duo:

- Outputs (USB sends, headphones, monitors) are processed in place inside the
  callback, after RØDE's processing and before the callback returns.
- Sources (microphones and other inputs) are processed one cycle later by a
  helper thread, working on copies the callback makes. RØDE's raw buffers
  aren't written. The helper runs on the audio cores one priority level below
  the callback. This one-cycle delay is the 2.33 ms block shift in the latency
  figures.
- Noise suppression runs on a separate worker thread (SCHED_RR priority 5, on
  CPU 0), several cycles behind, connected through bounded lock-free queues.
  If a result is late, the equally delayed dry signal plays instead.
- Paths with no processing are only metered, every eighth cycle.

The callback doesn't lock, allocate or wait.

For Chat Mix Out, the library captures the microphone contributions from
RØDE's routing code, after faders, mutes and mix-minus. It runs one mono
denoiser and voice gate on them, delays the rest of the mix by the same
amount, and recombines the two before the normal stereo effects. If the
microphone contributions can't be captured exactly, suppression on Chat Mix
Out is refused instead of denoising the finished mix.

See the [rc2fx-native](../crates/rc2fx-native.md) and
[rc2fx-suppression](../crates/rc2fx-suppression.md) chapters for details.

## Falling back to stock

The supervisor monitors the controller and the engine. If the controller
exits, or the engine isn't ready within 60 seconds, the supervisor records a
stock request for the rest of the boot and ends the engine. RØDE's launcher
restarts it, and the shim starts it stock. An engine that exits early twice in
one boot is handled the same way. A RAM run (`just mixer-run`) is gone after
the next reboot.

The supervisor is watched too. A panic inside it takes the same route to stock,
and if it crashes or is killed, the shell that launched the engine records the
stock request and ends the engine itself.

This logic is in `xtask/mixer/rc2fx.sh`, under subcommands such as `launch`,
`start`, `restart`, `stop`, `status` and `logs`. Change mixer-side behaviour in
that script. The supervisor runs beside live audio for as long as the engine
does and recovers from everything else failing, so it is its own small binary:
it's in `crates/rc2fx-supervisor`, shares no code with the controller, and calls
the script back for the rare steps, such as ending the engine.

## Settings flow

1. The app edits a local copy of the settings and sends it together with the
   revision it started from (`PUT /api/v1/config`, see the
   [controller API](../reference/api.md)).
2. `rc2fxd` validates the settings, prepares the change in the native library,
   saves `state.json`, and commits the prepared change
   ([rc2fx-native-control](../crates/rc2fx-native-control.md)).
3. The new settings apply from the next cycle with no restart. Meter, engine
   and state events are sent back to the app.

If the revision is out of date, the write is refused so it can't overwrite a
newer change.

## USB control channel

The mixer exposes a vendor HID interface on USB 1 for RØDE's app. RC2FX adds
report IDs 5 and 6 to RØDE's HID collection by staging a modified copy of the
descriptor in RAM. A hook on the engine's HID read passes RØDE's reports
through unchanged and forwards RC2FX's reports to `rc2fxd`. No USB function,
endpoint or audio channel is added. [USB control](../reference/usb-control.md)
covers the design and how it survives restarts.

## Installing

The installer downloads RØDE's official firmware, checks it, adds RC2FX's
files to the root filesystem image on the computer, and flashes it through
RØDE's USB update mode. It doesn't log in to a running mixer. See
[Installer internals](../reference/installer.md).

## Work in progress

- Touchscreen panel: a launcher button and settings panel on the mixer's
  screen for choosing a preset and turning processing on or off, served by
  `rc2fxd` over a local socket. It builds and has run on hardware from RAM,
  but isn't included in the installed firmware yet. See
  `experiments/device-panel/README.md`.
- RØDECaster Duo: implemented from firmware analysis and
  [not yet run on a Duo](../reference/duo.md).
