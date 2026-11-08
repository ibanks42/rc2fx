# Firmware support

RC2FX hooks RØDE's audio engine at specific instructions, so it only runs on
firmware whose bytes have been checked. A matching version number is not
enough. If RØDE re-releases an image under the same number, it is
unsupported until it has been reviewed.

## Supported firmware

| Mixer | Firmware | Status |
| --- | --- | --- |
| RØDECaster Pro II | 1.7.7 | Supported and the installer's default. Qualified offline against 1.7.6 and used on hardware. |
| RØDECaster Pro II | 1.7.6 | Supported. RC2FX was first built on this release. |
| RØDECaster Duo | 1.7.6, 1.7.7 | [Experimental](duo.md). Untested on hardware. Opt-in only. |

The screen module ([screen conversion](../user/settings-and-diagnostics.md#screen-conversion))
is pinned separately, to the Pro II 1.7.7 `rc_gui_juce`
(`aa92a92d384bdb24bc1cdb01615ac280d4d4867e661158df5c7f86ff98b605ca`). With
any other screen program, including the one in 1.7.6, the screen runs stock.

## The catalog

`config/firmware-profiles.json` is read by the installer, the launcher and
the native library. Each supported release pins:

- the SHA-256 of the official archive (RØDE's MD5 is checked too);
- the SHA-256 of the audio engine (`/usr/bin/rc_audio_mixer`) and its
  launcher (`/usr/bin/audio-start.sh`);
- the address of the JACK process callback and of the four functions in a
  RØDE job that RC2FX repairs, each with the 16 original bytes expected at
  that address;
- named ABI selectors: the callback signature (`jack-process-v1`), the job
  capture layout (`capture-values-v1`), the route layout (`pro-ii-25-v1` or
  `duo-23-v1`), the saved-show source list (`pro-ii-sources-v1` or
  `duo-sources-v1`), and the 48 kHz / 112-frame format.

`rc2fx-firmware` validates the file. Each ABI name selects code that is
already compiled into RC2FX. The JSON does not describe structures for RC2FX
to interpret. Unknown ABI names, misaligned addresses, duplicate identities
and missing signatures are rejected.

### Where the catalog is checked

1. The installer builds RC2FX into the catalog's releases for the connected
   model only, newest first. Download URLs are built from the model's
   firmware folder and the pinned version, and each download is checked
   against the pinned SHA-256 and MD5. Stock firmware comes from RØDE's
   release manifest instead; see [Stock firmware](installer.md#stock-firmware).
2. The launcher checks the engine and launcher hashes before it pauses stock
   audio (`rc2fx-firmware check-device`).
3. The native library checks the executable and the instruction bytes again
   before installing any hook. If they differ, the stock callback runs
   unwrapped.

The audio callback does not parse configuration or allocate memory to pick a
profile.

### Telling a Pro II from a Duo

The Pro II and Duo ship the same audio engine, so the engine hash cannot
identify the model. The model is read from the board: `/etc/hostname` and
the device tree named in `/boot/extlinux/extlinux.conf`. Both must agree, and
there is no override. A Duo profile is accepted only on a Duo board with the
experimental opt-in marker present. A Duo always gets the Duo route layout. See [Duo support](duo.md).

## How 1.7.7 was qualified

1.7.7 first came out as a beta, which was compared with 1.7.6 offline:

- Nine functions were compared instruction by instruction after normalizing
  relocations: the callback, the job enqueuer, the closure manager and
  invoker, the queued body, and the parent functions that register and call
  them. All matched.
- The job's closure layout and lifetime were checked by running RØDE's ARM64
  code under emulation.
- The launcher, updater, USB gadget script, libc, libstdc++ and libjack are
  byte-identical to 1.7.6.

The result is
[`firmware-audits/rodecaster-pro-ii-1.7.7.json`](../firmware-audits/rodecaster-pro-ii-1.7.7.json).
To reproduce the audit:

```sh
python -m pip install -r tools/firmware_audit/requirements.txt
python -m tools.firmware_audit --baseline PATH/1.7.6/os.tar.gz \
  --candidate PATH/1.7.7/os.tar.gz --output audit.json
```

After RØDE promoted 1.7.7 to stable, the stable Pro II and Duo archives were
downloaded again on 2026-11-03. Both are byte-identical to the beta archives
that were qualified: same archive SHA-256 and MD5, same engine and launcher
hashes, and `rc2fx-firmware check` passes every instruction-site check. The
catalog pins did not change. This check was offline only; there was no new
hardware test.

## Adding a release

1. Download the image from RØDE
   (`https://update.rode.com/rcp2/VERSION/os.tar.gz` for the Pro II,
   `rcp2-duo/` for the Duo) and record the archive, engine and launcher
   hashes.
2. Find the callback and job functions. Check the port and source mappings,
   the frame format and the closure layout. If nothing changed, add the new
   addresses and their original 16-byte signatures to the catalog. If
   something changed, write and test a new ABI implementation first.
3. Add the profile only after that review. The installer offers a release
   once its catalog entry has a `runtime` section that this build implements.
4. Validate locally:

   ```sh
   cargo test -p rc2fx-firmware
   cargo run -p rc2fx-firmware -- check config/firmware-profiles.json MIXER LAUNCHER rodecaster-pro-ii
   ```

5. Test on a real mixer, as a separate step agreed with whoever owns the
   hardware.

`inspect CATALOG MIXER LAUNCHER MODEL` checks an engine's code offline
without enabling that model's routes. `check` resolves addresses through the
executable's ELF segments, so it does not assume addresses equal file
offsets.

## On the mixer

The installed catalog is `/opt/rc2fx/firmware-profiles.json`, or sits next to
a RAM build. The launcher passes its path and the selected profile to the
library in `RC2FX_FIRMWARE_PROFILES` and `RC2FX_FIRMWARE_PROFILE`. The native
audit report records which profile was used.
