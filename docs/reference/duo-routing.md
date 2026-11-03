# RØDECaster Duo audio routing (1.7.6 and 1.7.7)

This note explains how the experimental Duo route layout, `duo-23-v1`, was
derived from official firmware, and why an earlier draft had fewer paths.

All of it comes from static analysis and ARM64 emulation of official RØDE
files. No Duo was contacted, flashed or used for audio. The note does not show
that a real Duo produces sound on these ports in every USB mode or after every
reconnect.

## Sources

| Item | Value |
| --- | --- |
| 1.7.6 archive | <https://update.rode.com/rcp2-duo/1.7.6/os.tar.gz>, SHA-256 `47d2ccb0d613b223ca6d5ed57c62ae6ee32ba71b662be5fbfb4ad3f44c757daa`, MD5 `50fb0c434ea5d4b643f7ff5a38253fe5` |
| 1.7.7 archive | <https://update.rode.com/rcp2-duo/1.7.7/os.tar.gz>, SHA-256 `4f3c7a022f07fd5b05955846e6a9052ad6ae214e12c04cda2d4749d775a304ed`, MD5 `09393fef8a9f3b627a39801148d85d1d` |
| 1.7.6 `rc_audio_mixer` | `897b9e25919d826ade563cad89a5acd140541b274b74524f655273488089f48c` (same as Pro II 1.7.6) |
| 1.7.7 `rc_audio_mixer` | `5151d0eb843dab46f441da4c2cca66f1636bfe522b2302345c17f28ecbffd0dd` (same as Pro II 1.7.7) |
| `audio-start.sh`, both | `f27368e6d68296ebb9c6f22401f2fe00317643bee23c08b86d44d183408fa263` |

In both releases the mixer program is byte-identical to the Pro II's. The
board files (hostname, device tree, kernel, modules and controller firmware)
are the Duo's, so neither archive can be used on the other model.

The analysis used the 1.7.7 beta archive. The stable 1.7.7 archive, checked
again on 2026-11-03, is byte-identical to it.

The evidence files are byte-for-byte copies from `.deps/duo-investigation/`:

- [`docs/firmware-audits/rodecaster-duo-1.7.7.json`](../firmware-audits/rodecaster-duo-1.7.7.json)
  is the hook audit run with `--baseline-id rodecaster-duo-1.7.6
  --candidate-id rodecaster-duo-1.7.7`. Nine whole functions are
  relocation-equivalent across the releases: the callback, enqueue, manager,
  invoker, job body, `Mixer::Process`, the function copy constructor, JACK
  registration teardown and active registration. Both ARM64 job-lifetime
  fixture sets pass. The hook offsets therefore match the corresponding Pro II
  releases. Only the route layout and source enum differ.
- [`docs/firmware-audits/rodecaster-duo-routes.json`](../firmware-audits/rodecaster-duo-routes.json)
  is the route audit described below, for both images, including the
  source-enum table.

## Paths in `duo-23-v1`

JACK port numbers start at 1 (`JUCEJack:in_3`). The mixer's internal packed
buffer arrays start at 0, so packed index 4 is a different buffer from
`out_4`. See [why outputs were held back](#why-outputs-were-held-back).

| RC2FX path | Saved ID | Mixer ports |
| --- | --- | --- |
| Mic 1, Mic 2 (combo inputs) | `duo-analog-1`, `duo-analog-2` | `in_1`, `in_2` |
| Headset mic | `duo-headset-mic` | `in_3` |
| Bluetooth | `duo-bluetooth` | `in_5`/`in_6` |
| USB 1 | `duo-usb-main` | `in_7`/`in_8` |
| USB 2 | `duo-usb-secondary` | `in_9`/`in_10` |
| Chat | `duo-usb-chat` | `in_11`/`in_12` |
| Game | `duo-usb-game` | `in_13`/`in_14` |
| Music | `duo-usb-music` | `in_15`/`in_16` |
| Virtual A | `duo-usb-a` | `in_17`/`in_18` |
| Virtual B | `duo-usb-b` | `in_19`/`in_20` |
| CallMe 1-3 | `duo-callme-1`..`3` | `in_29`..`in_34` |
| Wireless 1, 2 | `duo-wireless-1`, `duo-wireless-2` | `in_35`, `in_36` |
| Headphones 1 | `duo-headphones-1` | `out_3`/`out_4` |
| Headphones 2 | `duo-headphones-2` | `out_1`/`out_2` |
| Headset | `duo-headset` | `out_8` (left), `out_7` (right) |
| Monitors | `duo-monitors` | `out_5`/`out_6` |
| USB 1 send | `duo-send-main` | `out_15`/`out_16` to `usb1out:playback_1/2` |
| Chat send | `duo-send-chat` | `out_17`/`out_18` to `usb1-chat-out:playback_1/2` |
| USB 2 send | `duo-send-secondary` | `out_19`/`out_20` to `usb2out:playback_1/2` |

The table is `DUO_PATHS` in `crates/rc2fx-device/src/catalog.rs`. Every saved
ID starts with `duo-`, so a Pro II preset cannot bind a chain to a Duo socket
that is wired differently. The four virtual USB inputs were added to the
original 19 paths, as described in [USB path qualification](usb-paths.md).
SMART Pads and the extra per-channel multitrack USB outputs are not in the
layout.

The saved-show source enum is `duo-sources-v1` (named in
`crates/rc2fx-firmware/src/model.rs` and projected in
`crates/rc2fx-device/src/stock_inputs.rs`). It has seven faders, and source 2
is labelled "Headset mic". The vendor code also gives source ID 2 its own
digital-gain and phantom bypass (1.7.6 `0x40a27c`, `0x3a20d8` and nearby).

### Source-enum table

The model-aware source-change lookup (1.7.6 `0x40a360`, 1.7.7 `0x40cfd0`) is
relocation-equivalent as a whole function across the two releases. The route
audit runs the table initializer that the lookup reads (1.7.6 `0x135730`,
1.7.7 `0x1342a8`) and checks all 31 entries. Both releases build the same 31
triples of a source ID and two internal slots, ending with a `-1` sentinel.
The report has the full table and its addresses.

The internal slots are the mixer's own source indices. They are unrelated to
JACK port numbers, and not every enum value is an RC2FX processing path.

## How each group was found

Addresses are 1.7.6 ELF virtual addresses unless marked. The route audit lists
the 1.7.7 equivalents.

### Board check

`0x2572f0` (1.7.7 `0x259640`) reads `/boot/extlinux/extlinux.conf` and returns
1 for the Duo's "mini" device tree. The branches below depend on that result.

### Analog inputs and headset mic

In `Mixer::Process` (`0x657250`, 1.7.7 `0x657470`), the Duo branch at
`0x657698` jumps to `0x659008`, which keeps the third hardware input and
silences the fourth. The Duo's combo inputs are the first two capture ports,
and the headset mic is the third.

### Headphones and headset

The headphone branch `0x658830` jumps to `0x659014`. Headphone 1 (mixer
struct `+0xc2040`) writes packed outputs 2 and 3 (`out_3`/`out_4`). Headphone
2 (`+0xc2048`) writes packed 0 and 1 (`out_1`/`out_2`). The code at
`0x659154`..`0x65918c` copies headphone 1's left channel to packed 7 and its
right channel to packed 6. The headset therefore carries the headphone 1 mix
with the buffers swapped: `out_8` is left and `out_7` is right.

### Digital inputs and USB sends

The JACK port-event handler (`0x3716e0`, 1.7.7 `0x374410`) connects ports by
literal name. The route audit runs its real instructions in Unicorn, stubbing
JUCE strings, allocations and the JACK connect call, and records the
arguments passed to connect. Both releases produce exactly these
connections:

| Mixer port | USB endpoint |
| --- | --- |
| `JUCEJack:out_15`, `out_16` | `usb1out:playback_1`, `_2` |
| `JUCEJack:out_17`, `out_18` | `usb1-chat-out:playback_1`, `_2` |
| `JUCEJack:out_19`, `out_20` | `usb2out:playback_1`, `_2` |

The same handler connects the inputs (`usb1in`, `usb1-chat-in`, `usb2in`,
`btphone`, `RodeRfSPI`), listed in [the Duo reference](duo.md). CallMe uses
the same input ports as on the Pro II.

### Monitors

The mixer constructor stores three monitor routing fields. The Pro II default
store (`0x66b494`, 1.7.7 `0x67488c`) sets 10, 4 and 7. The Duo override
(`0x66f77c`, 1.7.7 `0x678ad4`) sets 8, 4 and 5. The route audit finds each
store by its instruction pattern and runs it for both board results.

The job body at `0x41bcb0` (1.7.7 `0x41ecf0`) then copies the monitor left and
right samples into packed outputs 4 and 5. Run in Unicorn against eight
hardware output buffers, it writes the expected samples into packed 4 and 5
(`out_5`/`out_6`) and leaves the headphone and headset buffers alone, in both
releases. The 1.7.7 constructor differs at the instruction level from 1.7.6's,
so the field stores were located one by one.

## Why outputs were held back

An earlier Duo layout had 15 paths and left out Monitors and the three USB
sends, so RC2FX offered no processing on those outputs. They still played the
stock mix, because RC2FX leaves ports outside its layout alone.

The outputs were left out because of a numbering question. The monitor
evidence above is in packed buffer indices, and which `out_N` a packed index
reaches depends on which hardware buffers the JACK active mask (`0x61cc90`)
enables. The mapping from mask to port was unknown, so the outputs were
excluded until it could be established.

The USB sends do not depend on that mapping. The port-event handler connects
named mixer ports directly to named USB endpoints, so the emulated connect
calls answer the question. For Monitors, emulating the constructor stores and
the job body fixes the producer side at packed 4 and 5, which is
`out_5`/`out_6` when the Duo's eight hardware outputs are active.

The native pipeline adds a runtime guard. It binds every path by port name,
without falling back to packed indexes, and processes a stereo output only
when both of its named buffers appear in the vendor callback. That prevents
processing the next packed buffer or only one channel. It does not check
which physical socket a port is wired to, and both names being present does
not prove the stereo mix is correct. As with every output, the Duo's hardware
outputs and USB sends are processed in the audio callback.

## Open questions

- Monitors in every mode. The monitor fixture assumes eight active hardware
  buffers. The shared startup callback also contains the Pro II's
  balanced-monitor rewiring, and nobody has observed a real Duo's JACK graph
  at startup or after a reconnect. The `out_5`/`out_6` mapping is the
  firmware's intended producer mapping, backed by the runtime guard. If the
  guard does not see both buffers, Monitors stays unprocessed.
- Live behaviour. The fixtures stub JUCE strings, allocations, events and JACK
  connections, and skip PAC/BTI hint instructions.
- Hardware. No flashing, recovery, audio loopback, timing, CPU load or USB
  mode test has been done on a Duo.

## Reproducing

Both commands read the official archives and change nothing. Set up a
virtualenv with `tools/firmware_audit/requirements.txt` first.

```sh
python -m tools.firmware_audit \
  --baseline PATH/duo-1.7.6-os.tar.gz --candidate PATH/duo-1.7.7-os.tar.gz \
  --baseline-id rodecaster-duo-1.7.6 --candidate-id rodecaster-duo-1.7.7 \
  --output duo-runtime-audit.json

python -m tools.firmware_audit.duo_routes \
  --baseline PATH/duo-1.7.6-os.tar.gz --candidate PATH/duo-1.7.7-os.tar.gz \
  --output duo-routing-audit.json
```

Both stop on any hash, instruction or fixture mismatch. Each report has a
`limitations` field describing what it does not cover.

## Verification receipt

These offline checks were run when the layout was added, with no device:

- The firmware, device and installer unit tests pass.
- The native library tests pass. They include a stereo output with only one
  channel observed, where both channels are left untouched, and a missing
  `out_6`, which is not replaced by `out_7`.
- `cargo xtask mixer-build` passes its AArch64, glibc 2.35 and GCC 11 checks.
- A temporary image built from the official Duo 1.7.7 archive keeps the boot
  and controller content and headers, the exact mixer and launcher hashes and
  the Duo device tree. It carries the runtime opt-in marker and fits the
  275 MiB size limit.
