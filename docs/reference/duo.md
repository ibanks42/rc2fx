# RØDECaster Duo support

RC2FX has experimental support for the Duo on official Duo firmware 1.7.6 and
1.7.7. It is off by default. To flash a Duo, the installer needs a saved
opt-in plus a risk confirmation on every run.

None of it has been tried on a real Duo: flashing, recovery and audio are all
untested. Everything on this page comes from the official firmware files,
static analysis and ARM64 emulation. Flashing a Duo may leave it unusable,
recovery may fail, and audio may be loud or wrong. Use it at your own risk.

Do not flash Pro II firmware onto a Duo or Duo firmware onto a Pro II. The two
share a mixer program, but their kernel, boot files and controller firmware
differ.

## Enabling it

When the installer is idle, it shows an **Allow experimental Duo flashing**
switch above Developer options. The setting is stored as
`experimentalDuoFlash` in the app's `view.json`. It defaults to off, and a
missing or malformed value is treated as off
(`crates/rc2fx-app/src/connection.rs`).

With the switch on and a Duo connected:

- Each run, and each retry before the flash step, shows `DUO_WARNING`
  (`crates/rc2fx-installer/src/lib.rs`) and asks for **Flash Duo at my own
  risk**. The confirmation is cancelled if the model, serial or release
  changes while the question is open.
- The installer offers only the two pinned official Duo archives, 1.7.7 and
  1.7.6, both for RC2FX and for stock firmware. It does not offer Pro II
  images for a Duo.
- All other checks still apply: archive pins and model, board identity,
  instruction signatures and serial continuity.
- The prepared image gets a file `/opt/rc2fx/experimental-duo` containing
  `1`. The launcher (`xtask/mixer/rc2fx.sh`) turns it into
  `RC2FX_EXPERIMENTAL_DUO=1`, which lets the runtime accept this Duo board
  with the Duo profile. The Duo is still identified as a Duo.

Turning the switch off blocks further Duo flashes. It does not remove RC2FX
from a Duo that already has it. To remove it, flash stock Duo firmware, which
in the RC2FX installer also requires the switch to be on. The app connects to
an installed Duo runtime through the RC2FX USB descriptor and handshake, as it
does for a Pro II.

## Processing paths

The Duo has its own route layout, `duo-23-v1`, and saved-show source enum,
`duo-sources-v1`. As on the Pro II, processing starts off.

| Inputs | Outputs |
| --- | --- |
| Mic 1, Mic 2, Headset mic | Headphones 1, Headphones 2, Headset |
| USB 1, USB 2, Chat | Monitors |
| Game, Music, Virtual A, Virtual B | |
| Bluetooth, CallMe 1-3 | Main Mix Out, Chat Mix Out, Secondary Mix Out (to USB 1, Chat and USB 2) |
| Wireless 1, Wireless 2 | |

Every saved Duo target ID starts with `duo-`, so a Pro II preset cannot put a
chain on a Duo socket that is wired differently. The layout includes the four
virtual USB inputs. It does not include SMART Pads or the extra per-channel
multitrack output feeds. Source names are established separately from route
availability, in [the source-name evidence](source-name-evidence.md).

[The Duo routing note](duo-routing.md) gives the port for each path, the
firmware addresses behind it and the evidence. In summary:

- Headphones 1 is `out_3`/`out_4` and Headphones 2 is `out_1`/`out_2`. The
  headset output mirrors Headphones 1 on `out_8` (left) and `out_7` (right).
- The USB sends are named connections from mixer ports to USB endpoints:
  `out_15`/`16` to USB 1, `17`/`18` to Chat and `19`/`20` to USB 2. Running
  the firmware's own connection code confirmed them.
- Monitors is `out_5`/`out_6`. That is the firmware's mapping when all eight
  hardware outputs are active. Nobody has seen a real Duo's JACK graph at
  startup or after a reconnect, so this mapping is unconfirmed for some USB
  and startup modes.

The native pipeline binds ports by name and does not fall back to numbered
buffer indexes. A stereo output is processed only when both of its named
buffers appear in the vendor callback. Otherwise it is left stock, which
prevents processing the wrong buffer or only one channel. The pipeline does
not check which physical socket a port is wired to, and having both names
present does not prove the stereo mix is correct.

## Firmware

| Release | Archive SHA-256 | Mixer program |
| --- | --- | --- |
| 1.7.6 | `47d2ccb0d613b223ca6d5ed57c62ae6ee32ba71b662be5fbfb4ad3f44c757daa` | Same as Pro II 1.7.6 |
| 1.7.7 | `4f3c7a022f07fd5b05955846e6a9052ad6ae214e12c04cda2d4749d775a304ed` | Same as Pro II 1.7.7 |

Both releases use the same launcher. The hook audit
([`rodecaster-duo-1.7.7.json`](../firmware-audits/rodecaster-duo-1.7.7.json))
and the route audit
([`rodecaster-duo-routes.json`](../firmware-audits/rodecaster-duo-routes.json))
cover both, and the routing note explains what each one shows. Both audits
were run on the 1.7.7 beta archive. The stable 1.7.7 archive, downloaded
again on 2026-11-03, is byte-identical to it.

## Identifying the model

The mixer program is the same on both models, so its hash cannot identify the
mixer. RC2FX describes each model separately (`Model` in
`crates/rc2fx-firmware/src/model.rs`) and identifies the board directly:

| | Pro II | Duo |
| --- | --- | --- |
| Hostname | `rodecaster-pro-ii` | `rodecaster-pro-ii-mini` |
| Boot device tree | `meson-axg-rodecaster-pro-ii.dtb` | `meson-axg-rodecaster-pro-ii-mini.dtb` |
| Firmware folder | `rcp2/` | `rcp2-duo/` |
| USB IDs, normal modes | `0037` `0078` `008d` `0092` `0094` | `0050` `0079` `008e` `0093` `0095` |
| USB ID, update mode | `0030` | `004f` |

On the mixer, RC2FX reads the model from `/etc/hostname` and from the device
tree named in `/boot/extlinux/extlinux.conf`. The two must agree, and there is
no override. The USB IDs come from RØDE's gadget script, which both firmwares
share. They have not been observed on a real Duo.

## Untested

These all need a real Duo, and a separate decision to flash one:

1. Flashing either release, the first-boot controller update, and returning
   to stock.
2. USB mode changes, reconnects and the app's control session.
3. Each path with an audio loopback, including Monitors in each USB mode and
   the headset's channel order.
4. Source reassignment, fallback to stock on failure, and sustained
   processing load. The figures in [Hardware](hardware.md) are for the Pro II.

For a first test, back up the RØDE show, disconnect headphones and speakers,
and leave the mixer alone through its first boot after the flash.

## Offline development

This work needs no Duo. The Duo firmware is not in git. Download
`https://update.rode.com/rcp2-duo/1.7.6/os.tar.gz` and
`https://update.rode.com/rcp2-duo/1.7.7/os.tar.gz` to
`.deps/duo-investigation/duo-1.7.6-os.tar.gz` and `duo-1.7.7-os.tar.gz`.

To check an official archive without extracting it:

```sh
for v in 1.7.6 1.7.7; do
  cargo run -p rc2fx-installer --example inspect-firmware -- \
    rodecaster-duo ".deps/duo-investigation/duo-$v-os.tar.gz"
done
```

This verifies the checksums, that the root filesystem and boot files are a
Duo's, the mixer and launcher hashes, and the bytes RC2FX hooks. A good result
shows `executableVerified: true`. `runtimeQualified` stays `false`, because
the Duo runtime is accepted only with the opt-in.

To check an extracted mixer and launcher:

```sh
cargo run -p rc2fx-firmware -- inspect config/firmware-profiles.json \
  .deps/duo-investigation/duo/usr/bin/rc_audio_mixer \
  .deps/duo-investigation/duo/usr/bin/audio-start.sh rodecaster-duo
```

Tests that cover Duo handling:

```sh
cargo test -p rc2fx-firmware -p rc2fx-installer -p rc2fx-device
cargo test --manifest-path crates/rc2fx-native/Cargo.toml --lib
cargo test -p rc2fx-app installer::
python3 xtask/mixer/tests/usb_restart.py

# These need the official archives:
for v in 1.7.6 1.7.7; do
  RC2FX_DUO_ARCHIVE="$PWD/.deps/duo-investigation/duo-$v-os.tar.gz" \
    cargo test -p rc2fx-installer pinned_duo_archive -- --ignored
done
```

To build a temporary Duo image from an official archive and check that it
keeps the Duo's boot and controller files and has the opt-in marker, first
cross-build the mixer runtime locally:

```sh
cargo xtask mixer-build
RC2FX_DUO_ARCHIVE="$PWD/.deps/duo-investigation/duo-1.7.7-os.tar.gz" \
  RC2FX_REAL_ROOT="$PWD" \
  cargo test -p rc2fx-installer real_duo_image -- --ignored
```

The routing note has the audit commands and the offline results so far,
under its verification receipt. None of those results involve a real Duo.

## Factory self-test mode

RØDE's launcher has a "production" branch, selected by the file
`/usr/bin/production-boot`. It is the factory self-test mode. It runs at 128
frames, reflashes the VL805 controller firmware, deletes saved
`system-config*` files, changes bootloader settings and starts a self-test
engine. Do not create that marker, including to experiment with buffer sizes.
RC2FX supports only normal-mode 112-frame operation on both models.
