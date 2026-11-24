# RØDECaster Duo audio routing (1.7.6 and 1.7.7)

Here's how we derived the experimental Duo route layout (`duo-23-v1`) from RØDE's official firmware, and why an earlier draft left several outputs out.

Everything below is based on static analysis and ARM64 emulation of official RØDE binaries. We haven't connected, flashed, or passed audio through a real Duo, so this doesn't prove a live Duo outputs sound on these ports in every USB mode or across every reconnect.

## Sources

| Item | Value |
| --- | --- |
| 1.7.6 archive | <https://update.rode.com/rcp2-duo/1.7.6/os.tar.gz>, SHA-256 `47d2ccb0d613b223ca6d5ed57c62ae6ee32ba71b662be5fbfb4ad3f44c757daa`, MD5 `50fb0c434ea5d4b643f7ff5a38253fe5` |
| 1.7.7 archive | <https://update.rode.com/rcp2-duo/1.7.7/os.tar.gz>, SHA-256 `4f3c7a022f07fd5b05955846e6a9052ad6ae214e12c04cda2d4749d775a304ed`, MD5 `09393fef8a9f3b627a39801148d85d1d` |
| 1.7.6 `rc_audio_mixer` | `897b9e25919d826ade563cad89a5acd140541b274b74524f655273488089f48c` (same as Pro II 1.7.6) |
| 1.7.7 `rc_audio_mixer` | `5151d0eb843dab46f441da4c2cca66f1636bfe522b2302345c17f28ecbffd0dd` (same as Pro II 1.7.7) |
| `audio-start.sh`, both | `f27368e6d68296ebb9c6f22401f2fe00317643bee23c08b86d44d183408fa263` |

In both releases, the mixer binary is byte-identical to the Pro II's. The board files (hostname, device tree, kernel, modules, and controller firmware) are specific to the Duo, so you can't cross-flash either archive.

We originally ran this analysis on the 1.7.7 beta archive; re-checking the stable 1.7.7 archive on 2026-11-03 confirmed it's byte-identical.

The committed evidence files are exact copies from `.deps/duo-investigation/`:

- [`docs/firmware-audits/rodecaster-duo-1.7.7.json`](../firmware-audits/rodecaster-duo-1.7.7.json): hook audit run with `--baseline-id rodecaster-duo-1.7.6 --candidate-id rodecaster-duo-1.7.7`. Nine whole functions are relocation-equivalent between the two releases: the callback, enqueue, manager, invoker, job body, `Mixer::Process`, the function copy constructor, JACK registration teardown, and active registration. Both ARM64 job-lifetime fixture sets pass, so the hook offsets match the Pro II releases; only the route layout and source enum differ.
- [`docs/firmware-audits/rodecaster-duo-routes.json`](../firmware-audits/rodecaster-duo-routes.json): the route audit detailed below across both releases, including the source-enum table.

## Paths in `duo-23-v1`

Watch out for indexing: JACK port numbers are 1-based (`JUCEJack:in_3`), while the mixer's internal packed buffer arrays are 0-based, so packed index 4 isn't `out_4`. See [Why outputs were held back](#why-outputs-were-held-back).

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

This lives in `DUO_PATHS` inside `crates/rc2fx-device/src/engine/catalog.rs`. Prefixing every ID with `duo-` keeps Pro II presets from binding to differently wired Duo sockets. The four virtual USB inputs were added on top of the initial 19 paths (see [USB path qualification](usb-paths.md)). SMART Pads and the extra per-channel multitrack USB outputs aren't included.

The saved-show source enum is `duo-sources-v1` (defined in `crates/rc2fx-firmware/src/model.rs` and projected in `crates/rc2fx-device/src/engine/stock_inputs.rs`). It has seven faders, with source 2 labeled "Headset mic". RØDE's binary also gives source ID 2 its own digital-gain and phantom-power bypass logic (1.7.6 `0x40a27c`, `0x3a20d8`, and surrounding code).

### Source-enum table

The model-aware source-change lookup (1.7.6 `0x40a360`, 1.7.7 `0x40cfd0`) is relocation-equivalent as a whole function between both releases. The route audit executes the table initializer it reads from (1.7.6 `0x135730`, 1.7.7 `0x1342a8`) and verifies all 31 entries. Both releases construct the exact same 31 `(source_id, slot_a, slot_b)` triples ending in a `-1` sentinel. The full table and addresses are in the audit report.

Those internal slots are the mixer's own source indices. They don't map 1-to-1 to JACK port numbers, and not every enum value is an RC2FX processing path.

## How each group was found

Addresses below are 1.7.6 ELF virtual addresses unless noted; the route audit lists the 1.7.7 equivalents.

### Board check

`0x2572f0` (1.7.7 `0x259640`) parses `/boot/extlinux/extlinux.conf` and returns 1 when it sees the Duo's "mini" device tree. Every Duo-specific branch below keys off that flag.

### Analog inputs and headset mic

Inside `Mixer::Process` (`0x657250`, 1.7.7 `0x657470`), the Duo branch at `0x657698` jumps to `0x659008`, keeping the third hardware input alive and zeroing out the fourth. So the two combo inputs are the first two capture ports and the headset mic is the third.

### Headphones and headset

The headphone branch at `0x658830` jumps to `0x659014`. Headphone 1 (mixer struct `+0xc2040`) writes packed outputs 2 and 3 (`out_3`/`out_4`), while Headphone 2 (`+0xc2048`) writes packed 0 and 1 (`out_1`/`out_2`). Then `0x659154`..`0x65918c` copies Headphone 1's left channel into packed 7 and its right channel into packed 6, meaning the headset carries the Headphone 1 mix with swapped buffers (`out_8` left, `out_7` right).

### Digital inputs and USB sends

The JACK port-event handler (`0x3716e0`, 1.7.7 `0x374410`) wires ports by literal string name. The route audit runs its real instructions in Unicorn (stubbing JUCE strings, allocations, and the JACK connect call) and logs every connect call. Both releases make these exact connections:

| Mixer port | USB endpoint |
| --- | --- |
| `JUCEJack:out_15`, `out_16` | `usb1out:playback_1`, `_2` |
| `JUCEJack:out_17`, `out_18` | `usb1-chat-out:playback_1`, `_2` |
| `JUCEJack:out_19`, `out_20` | `usb2out:playback_1`, `_2` |

The same handler wires up the digital inputs (`usb1in`, `usb1-chat-in`, `usb2in`, `btphone`, `RodeRfSPI`), listed in [the Duo reference](duo.md). CallMe uses the same input ports as on the Pro II.

### Monitors

The mixer constructor initializes three monitor routing fields. On the Pro II (`0x66b494`, 1.7.7 `0x67488c`), it stores 10, 4, and 7. On the Duo (`0x66f77c`, 1.7.7 `0x678ad4`), it overrides them with 8, 4, and 5. The route audit locates each store by instruction pattern and emulates it for both board types.

The job body at `0x41bcb0` (1.7.7 `0x41ecf0`) then copies monitor left and right samples into packed outputs 4 and 5. Emulated in Unicorn against eight hardware output buffers, it writes the expected samples into packed 4 and 5 (`out_5`/`out_6`) without touching the headphone or headset buffers in either release. Because the 1.7.7 constructor's instructions differ slightly from 1.7.6, each field store is matched individually.

## Why outputs were held back

An earlier 15-path Duo layout left out Monitors and all three USB sends. Those outputs still passed the stock mix cleanly since RC2FX ignores ports outside its layout.

We held them back because of a port-numbering ambiguity. The monitor evidence above is in packed buffer indices, and which `out_N` a packed index lands on depends on which hardware buffers the JACK active mask (`0x61cc90`) enables. Until we nailed down that mapping, we kept those outputs out.

Turns out the USB sends don't depend on the active mask at all: the port-event handler connects named mixer ports straight to named USB endpoints, which the emulated connect calls confirm directly. For Monitors, emulating the constructor stores and job body pins the producer side to packed 4 and 5, which is `out_5`/`out_6` whenever all eight Duo hardware outputs are active.

As a runtime safety net, the native pipeline binds every path by port name with no fallback to packed indexes, and only processes a stereo output if both named buffers appear in the vendor callback. That stops us from accidentally touching the next packed buffer or processing just one channel. It still can't verify physical jack wiring, and seeing both names doesn't prove the stereo mix itself is right. Like all outputs, the Duo's hardware outputs and USB sends are processed inline in the audio callback.

## Open questions

- **Monitors across all modes.** Our monitor fixture assumes eight active hardware buffers. The shared startup callback also contains the Pro II's balanced-monitor rewiring, and nobody has captured a real Duo's JACK graph at startup or after a reconnect. `out_5`/`out_6` is the firmware's intended producer mapping, backed by the runtime guard: if both buffers aren't present, Monitors stays untouched.
- **Live behavior.** The Unicorn fixtures stub JUCE strings, allocations, events, and JACK connections, and skip PAC/BTI hint instructions.
- **Real hardware.** No flashing, recovery, audio loopback, timing, CPU load, or USB mode tests have been run on a physical Duo.

## Reproducing

Both commands are read-only against the official archives. Create a virtualenv with `tools/firmware_audit/requirements.txt` first:

```sh
python -m tools.firmware_audit \
  --baseline PATH/duo-1.7.6-os.tar.gz --candidate PATH/duo-1.7.7-os.tar.gz \
  --baseline-id rodecaster-duo-1.7.6 --candidate-id rodecaster-duo-1.7.7 \
  --output duo-runtime-audit.json

python -m tools.firmware_audit.duo_routes \
  --baseline PATH/duo-1.7.6-os.tar.gz --candidate PATH/duo-1.7.7-os.tar.gz \
  --output duo-routing-audit.json
```

Either script aborts on any hash, instruction, or fixture mismatch, and writes a `limitations` section into its JSON output noting what wasn't covered.

## Verification receipt

Offline checks run when this layout was added:

- Firmware, device, and installer unit tests all pass.
- Native library tests pass, including cases where a stereo output only sees one channel (leaving both untouched) and where `out_6` is missing (verifying it doesn't shift onto `out_7`).
- `go -C tools/xtask run . mixer-build` passes its AArch64, glibc 2.35, and GCC 11 checks.
- A temporary image built from the official Duo 1.7.7 archive preserves the Duo boot and controller files and headers, keeps the exact mixer and launcher hashes and Duo device tree, includes the runtime opt-in marker, and stays under the 275 MiB size limit.
