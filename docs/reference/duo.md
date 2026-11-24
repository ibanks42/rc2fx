# RØDECaster Duo support

RC2FX includes experimental support for the RØDECaster Duo on official Duo
firmware 1.7.6 and 1.7.7. It is disabled by default: flashing a Duo requires
both a persisted opt-in toggle and an explicit risk confirmation on every run.

None of this has been tested on physical Duo hardware: flashing, recovery, and
audio playback are all untested. Everything documented here is derived from
official firmware archives, static analysis, and ARM64 emulation. Flashing a
Duo could brick the mixer, recovery could fail, and audio could come out wrong
or dangerously loud. Proceed strictly at your own risk.

Never flash Pro II firmware onto a Duo or Duo firmware onto a Pro II. Although
both models share the same mixer binary, their kernel, boot files, and
controller firmware are different.

## Enabling it

When a Duo is connected, the start screen and installer page display a
**RC2FX is untested on the Duo. Install anyway.** switch, and the install
button stays disabled until it is toggled on. This preference is saved as
`experimentalDuoFlash` in the app's `view.json`, defaulting to off (missing or
malformed values are treated as off in `desktop/app/state/prefs.go`). The
switch is locked while an installer run is active.

With the switch enabled and a Duo connected:

- For RC2FX installs, the installer only lists the two pinned official Duo
  archives (1.7.7 and 1.7.6). Stock firmware options come from RØDE's Duo
  manifest (`rcp2-mini-manifest.json`). Pro II images are never offered for a
  Duo.
- Every standard safety check still runs: archive hash pins, model and board
  identity, instruction signatures, and USB serial continuity.
- The patched image includes `/opt/rc2fx/experimental-duo` containing `1`. The
  launcher (`mixer/scripts/rc2fx.sh`) translates that file into
  `RC2FX_EXPERIMENTAL_DUO=1`, allowing the runtime to accept the Duo board with
  the Duo profile while still identifying the mixer as a Duo. Rollback copies
  booted after an interrupted in-place update read the same marker.

Turning the switch back off prevents future Duo flashes without removing RC2FX
from a Duo that already has it. Removing RC2FX requires flashing stock Duo
firmware (which also requires the switch to be on inside the RC2FX installer).
Once installed, the desktop app connects to a Duo runtime over the same RC2FX
USB descriptor and handshake used on the Pro II.

## Processing paths

The Duo uses its own route layout (`duo-23-v1`) and saved-show source enum
(`duo-sources-v1`). Just like on the Pro II, processing starts off.

| Inputs | Outputs |
| --- | --- |
| Mic 1, Mic 2, Headset mic | Headphones 1, Headphones 2, Headset |
| USB 1, USB 2, Chat | Monitors |
| Game, Music, Virtual A, Virtual B | |
| Bluetooth, CallMe 1-3 | Main Mix Out, Chat Mix Out, Secondary Mix Out (to USB 1, Chat, and USB 2) |
| Wireless 1, Wireless 2 | |

All saved Duo target IDs are prefixed with `duo-` so a Pro II preset can never
attach an effect chain to a differently wired Duo socket. The layout includes
the four virtual USB inputs, and omits SMART Pads and the extra per-channel
multitrack output feeds. Source names are documented independently from route
availability in [the source-name evidence](source-name-evidence.md).

See [the Duo routing note](duo-routing.md) for exact port mappings, firmware
addresses, and supporting evidence. In brief:

- Headphones 1 maps to `out_3`/`out_4` and Headphones 2 maps to
  `out_1`/`out_2`. The headset output mirrors Headphones 1 on `out_8` (left)
  and `out_7` (right).
- USB sends are named connections from mixer ports to USB endpoints:
  `out_15`/`16` to USB 1, `17`/`18` to Chat, and `19`/`20` to USB 2, confirmed
  by emulating the firmware's own connection setup code.
- Monitors maps to `out_5`/`out_6` when all eight hardware outputs are active.
  Because nobody has observed a real Duo's JACK graph at startup or across USB
  reconnects, this mapping remains unconfirmed in some startup and USB modes.

The native pipeline binds ports strictly by name and never falls back to
numeric buffer indices. A stereo output is only processed when both of its
named buffers appear in the vendor callback; otherwise it passes through as
stock so RC2FX never processes the wrong buffer or only half of a stereo pair.
That said, the pipeline cannot verify physical socket wiring, and seeing both
port names does not by itself prove the stereo mix is right.

## Firmware

| Release | Archive SHA-256 | Mixer program |
| --- | --- | --- |
| 1.7.6 | `47d2ccb0d613b223ca6d5ed57c62ae6ee32ba71b662be5fbfb4ad3f44c757daa` | Same as Pro II 1.7.6 |
| 1.7.7 | `4f3c7a022f07fd5b05955846e6a9052ad6ae214e12c04cda2d4749d775a304ed` | Same as Pro II 1.7.7 |

Both releases share the same launcher. Both are covered by the hook audit
([`rodecaster-duo-1.7.7.json`](../firmware-audits/rodecaster-duo-1.7.7.json))
and route audit
([`rodecaster-duo-routes.json`](../firmware-audits/rodecaster-duo-routes.json)),
explained in detail in the routing note. Both audits ran against the 1.7.7 beta
archive; re-downloading the stable 1.7.7 archive on 2026-11-03 confirmed it is
byte-identical.

## Identifying the model

Because the mixer binary is identical across both models, its hash cannot tell
a Pro II from a Duo. Instead, RC2FX defines each model explicitly (`Model` in
`crates/rc2fx-firmware/src/model.rs`) and checks the board directly:

| | Pro II | Duo |
| --- | --- | --- |
| Hostname | `rodecaster-pro-ii` | `rodecaster-pro-ii-mini` |
| Boot device tree | `meson-axg-rodecaster-pro-ii.dtb` | `meson-axg-rodecaster-pro-ii-mini.dtb` |
| Firmware folder | `rcp2/` | `rcp2-duo/` |
| USB IDs, normal modes | `0037` `0078` `008d` `0092` `0094` | `0050` `0079` `008e` `0093` `0095` |
| USB ID, update mode | `0030` | `004f` |

On-device, RC2FX reads both `/etc/hostname` and the device tree referenced in
`/boot/extlinux/extlinux.conf`. Both must agree, with no override. The USB IDs
are taken from RØDE's shared USB gadget script and have not been observed on a
physical Duo.

## How RØDE's app updates a Duo

These details come from static analysis of RØDECaster App 2.0.115 for macOS
(whose binary retains symbols); the Windows build references the same manifests
and filenames.

- A single class, `Rodecaster2FirmwareUpdater`, handles the Pro II, Duo,
  Streamer X, and an unreleased "RØDECaster Pro II-A" with no model-specific
  branching.
- It sends HID `M` (report 1, 63 bytes) and expects an `A` reply, waits up to
  30 seconds for update mode, copies `archive.md5` followed by
  `archive.tar.gz` to the update drive, runs `sync`, pauses 1 second, ejects
  the drive, and sends HID `U`. A factory reset sends `R` before `M`. If a
  mixer is already in update mode, it queries the drive's volume ID with HID
  `V`. On macOS, the drive mounts as `NO NAME` on the same USB device.
- Each model uses a separate manifest: `manifest.json` for Pro II (`0030`),
  `rcp2-mini-manifest.json` for Duo (`004f`), and `rcp2-a-manifest.json` for
  Pro II-A (`009f`, not yet published).
- RØDE's device table lists the Duo as `004f` `0050` `0073` `0079` `0093`
  `0095`, the Pro II as `0030` `0037` `0072` `0078` `0092` `0094`, and the
  Pro II-A as `009b` through `009f`. RC2FX omits `0072` and `0073`, while RØDE
  omits `008d` and `008e`. RC2FX ignores any USB product ID not in its table.

RC2FX follows the same flash sequence, additionally reading both files back
before ejecting and waiting for the drive to disappear. None of this validates
the RC2FX image or runtime on real Duo hardware.

## Untested

The following still require physical Duo hardware and an explicit decision to
test on one:

1. Flashing either firmware release, completing the first-boot controller
   update, and returning to stock.
2. USB mode switches, reconnects, and the desktop app's control session.
3. Audio loopback verification on every path, including Monitors across all USB
   modes and headset left/right channel order.
4. Source reassignment, fallback to stock on error, and sustained DSP load (all
   measurements in [Hardware](hardware.md) are from a Pro II).

Before attempting a first hardware test, back up the RØDE show, unplug
headphones and speakers, and leave the mixer untouched through its first boot
after flashing.

## Offline development

You don't need a Duo for offline checks. Duo firmware archives aren't committed
to git; download `https://update.rode.com/rcp2-duo/1.7.6/os.tar.gz` and
`https://update.rode.com/rcp2-duo/1.7.7/os.tar.gz` to
`.deps/duo-investigation/duo-1.7.6-os.tar.gz` and `duo-1.7.7-os.tar.gz`.

Inspect an official archive without extracting it:

```sh
for v in 1.7.6 1.7.7; do
  go -C desktop run ./cmd/inspect-firmware \
    rodecaster-duo "$PWD/.deps/duo-investigation/duo-$v-os.tar.gz"
done
```

This checks the archive checksums, confirms the root filesystem and boot files
belong to a Duo, and verifies the mixer hash, launcher hash, and hooked
instruction bytes. A passing run reports `executableVerified: true`, while
`runtimeQualified` stays `false` because Duo runtime loading requires the
explicit opt-in.

Inspect an extracted mixer binary and launcher script:

```sh
cargo run -p rc2fx-firmware -- inspect config/firmware-profiles.json \
  .deps/duo-investigation/duo/usr/bin/rc_audio_mixer \
  .deps/duo-investigation/duo/usr/bin/audio-start.sh rodecaster-duo
```

Run the automated Duo test suite:

```sh
cargo test -p rc2fx-firmware -p rc2fx-device
cargo test --manifest-path crates/rc2fx-native/Cargo.toml --lib
go -C desktop test ./firmware/... ./installer/... ./app/installflow/...
python3 mixer/scripts/tests/usb_restart.py

# These need the official archives:
for v in 1.7.6 1.7.7; do
  RC2FX_DUO_ARCHIVE="$PWD/.deps/duo-investigation/duo-$v-os.tar.gz" \
    go -C desktop test ./installer -run TestPinnedDuoArchivePassesOfflineOnlyAndFailsAsProIi
done
```

To build a temporary Duo image from an official archive and verify that it
preserves the Duo's boot and controller files and writes the opt-in marker,
cross-build the mixer runtime first:

```sh
go -C tools/xtask run . mixer-build
RC2FX_DUO_ARCHIVE="$PWD/.deps/duo-investigation/duo-1.7.7-os.tar.gz" \
  RC2FX_REAL_ROOT="$PWD" \
  go -C desktop test ./installer -run TestRealDuoImagePreservesBoardFilesAndCarriesExplicitRuntimeOptIn
```

The routing note lists the audit commands and offline results under its
verification receipt. None of those results come from a real Duo.

## Factory self-test mode

RØDE's launcher has a "production" branch triggered by the presence of
`/usr/bin/production-boot`. This is a factory self-test mode: it switches to
128-frame buffers, reflashes the VL805 controller firmware, deletes saved
`system-config*` files, modifies bootloader settings, and launches a self-test
engine. Never create that file, not even to experiment with buffer sizes. RC2FX
only supports standard 112-frame operation on both models.
