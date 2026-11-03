# rc2fx-native

`librc2fx_native.so` runs RC2FX's processing inside RØDE's stock
`rc_audio_mixer` on a qualified firmware profile. RØDE's launcher runs the
RC2FX shim in place of the mixer. The shim calls `xtask/mixer/rc2fx.sh launch`,
which execs the stock mixer with this library loaded as an `LD_AUDIT` module.

The crate is a Linux-only cdylib and has its own Cargo workspace. `just
mixer-build` cross-builds it for the mixer.

## Hooks

The ELF audit interface redirects six libjack bindings made by the mixer:

- process callback registration,
- xrun callback registration,
- buffer lookup,
- client close,
- port registration,
- port unregistration.

Bindings made inside libjack itself and lookups through `dlsym` are left as
they are. The xrun wrapper records jackd's notification and then calls the
mixer's own xrun callback.

The mixer's process callback is wrapped only at the executable-relative offset
given in `config/firmware-profiles.json`, and only after the executable's hash
and the original instructions at that offset have been checked. If either
check fails, the stock callback runs unwrapped. The same check guards a small
fix to a vendor job in that callback, which used stack locals after their
lifetime ended.

At startup the library reads the launch plan written by `rc2fxd plan`, binds
every path in it, and processes the selected paths with `rc2fx-dsp`. The audio
callback does not lock, allocate or wait.

`rc2fxd` connects to the library over a Unix control socket
(`rc2fx-native-control`) to publish new settings, which apply without
restarting the mixer, and to read meters and timing.

## Outputs

Outputs are the USB sends and the mixer's hardware outputs, headphones and
monitors. They are processed in place in the callback, after the mixer's own
processing and before the callback returns (`pipeline::DIRECT_OUTPUT`).

## Sources

Sources are processed by a helper thread, from copies made by the callback:

1. At the end of each callback, the callback copies the frame into a private
   double buffer and hands it to the helper.
2. The helper processes it with `rc2fx-dsp`.
3. The next callback gives the processed source to the mixer.

A processed source reaches the mixer one frame (2.3 ms) late. The raw source
buffer is left intact for the mixer's other consumers, and the helper doesn't
touch any buffer owned by jackd.

If the helper hasn't finished by the next callback, that callback waits up to
200 µs. If it is still not done, the frame passes through unprocessed once and
the miss is reported (`helper.late`, `helper.stalls`, and `helperBusy` and
`helperStall` events).

Unprocessed paths are left alone. Their buffers are only resolved for a meter
reading every eighth callback.

### Helper placement

The helper runs on the audio cores the callback isn't pinned to, one real-time
priority below the callback. It doesn't compete with the callback and has most
of a period for each frame. Before RØDE pins the callback, or if the callback
runs on every audio core, the helper still stays on the audio cores. It stays
off CPU 0, which the suppression worker uses.

The callback checks its placement every 64 callbacks until it is real-time on
its own cores and the helper has applied the result, for at most the first
8192 callbacks. After that it checks every 2048 callbacks. The helper reads the
placement only when it starts a frame, so placement is sampled at a handoff. If
a check falls on a callback that found the helper busy, it happens at the next
callback that hands off and is counted in `placement.deferred`.

Until the helper has been placed it runs with the placement it started with,
so the first callbacks after start aren't covered. The report's
`placement.history` keeps the first 16 placement changes since the library
loaded, without overwriting. Each entry records the pipeline, the callback's
mask, policy and priority, the helper's requested and actual placement, the
cycle, whether it was deferred, and the monotonic time.

### Why the helper

Measured on the device with a microphone (noise suppression and EQ) and a stereo
headphone EQ enabled: RØDE's own callback takes 670–750 µs and starts
300–600 µs into the 2333 µs cycle. The DSP for the two paths costs about
240 µs. Running it inside the callback pushed 0.1 % of callbacks past the end
of the cycle under USB audio load, and each of those was an xrun. A helper on
the callback's own core spent half its time preempted. The placement described
above came out of these measurements.

## Noise suppression

Noise suppression (DeepFilterNet3) runs on a separate worker from
`rc2fx-suppression`. A late frame falls back to the delayed dry signal. One
enabled target holds suppression at a time: a mono microphone or the Chat
voice mix. It keeps that slot while bypassed or deselected.

### Chat

For Chat, `chat_mix` captures only the microphone contributions from the
qualified vendor routing functions. It takes them after per-source processing
and fader gains, through main, custom, solo and mix-minus routing. Each
original function is called once, and vendor routing objects are only read.

Four hash-pinned detours, installed before any threads start, provide a stem
for the current callback. If the layout is unknown or a stem is missing,
suppression is not applied to the completed mix. `rc2fx-dsp::Chat` runs one
mono model and voice gate, delays the stereo bypass by the same amount, and
then applies the normal stereo output effects. Pads and playback don't go
through the model or the voice gate.

Tests check the detours against the vendor's ARM64 code, using an emulated
oracle and trampoline tests. The trained model's sound quality on a live
mixer, simultaneous speakers, and whether the Smart Pad bypass is audible
still need testing on a device.

## Source accounting

The report's `sources` section counts, for each source frame that was meant to
be processed, what the mixer received:

- `processed`,
- the first raw cause:
  - `rawHelperUnready`,
  - `rawNoHandoff`: the first frame, or the frame after a stall, an inactive
    frame or a switch to processing,
  - `rawNotDone`,
  - `rawInvalid`,
- `unobserved`, when the mixer didn't ask for it.

A frame with any raw request is not counted as processed. `rawReturns` counts
the raw requests themselves. The counts are cumulative from when the library
loaded, including startup, so take differences over a window. They don't show
what the mixer does with a source buffer after reading it, whether it reads
the buffer again after the callback, or the physical input-to-headphone
latency.

## Environment

`rc2fx.sh` sets these:

| Variable | Meaning |
| --- | --- |
| `RC2FX_FIRMWARE_PROFILES` | Path to the validated firmware catalog (embedded catalog is the fallback) |
| `RC2FX_FIRMWARE_PROFILE` | Expected profile ID; exact executable hash selects and verifies it |
| `RC2FX_NATIVE_PROBE_CALLBACK_OFFSET` | Optional diagnostic cross-check; must equal the selected profile's callback |
| `RC2FX_NATIVE_PROBE_PLAN` | Launch plan JSON (version 1, 48 kHz, 112 frames, at most 32 paths, 64 KiB) |
| `RC2FX_NATIVE_PROBE_CONTROL_SOCKET` | Control socket path for `rc2fxd` |
| `RC2FX_NATIVE_PROBE_REPORT` | Path for the audit report, rewritten every second: callback, helper and jackd-cycle timing, xruns and the latest anomaly events |
| `RC2FX_NATIVE_PROBE_EVENTS` | Log file appended once a second with every timestamped anomaly (late callback starts or ends, helper late or stalled, gaps, xruns, late vendor jobs, USB gadget rebuilds), each with the time since the last USB control report |
| `RC2FX_NATIVE_PROBE_REPORT_SELECTED_ONLY` | `1` reports processed paths only |
| `RC2FX_NATIVE_PROBE_VENDOR_JOB_LIFETIME` | Diagnostic opt-in to profile selection; profile checks still apply |

## Reading xrun diagnostics

`xruns` counts the notifications received by instrumented JACK clients. It
doesn't count unique failed periods or confirmed audible dropouts, and the
client that received a notification may not be the one that was late.

In an `xrun` event, the legacy `valueNs` and `notificationCyclePositionNs` give
the receiver's position in its cycle when it was notified. They are not an
overrun duration. If the clock wasn't available, the value is null.

`lateEnd` events fire at 90 % of the period. `cycle.lateEnds` counts estimated
ends past the full period. Neither replaces evidence from the server.

The [JACK error recorder](jackerr/README.md) is loaded into jackd on every
RC2FX start. It records the late client and state, or the ALSA error, that the
server reports. `just mixer-xruns` shows what it recorded over a window. Check
it together with the callback, helper and worker diagnostics before changing
buffering.

## Tests

Unit tests run on a Linux host:

```sh
cargo test --manifest-path crates/rc2fx-native/Cargo.toml
```
