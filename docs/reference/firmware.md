# Firmware support

Because RC2FX hooks RØDE's audio engine at specific instruction addresses, it will only run on firmware whose bytes have been checked. Matching a version number isn't enough: if RØDE quietly re-releases an image under the same version, RC2FX treats it as unsupported until someone reviews it.

## Supported firmware

| Mixer | Firmware | Status |
| --- | --- | --- |
| RØDECaster Pro II | 1.7.7 | Supported and the installer's default. Qualified offline against 1.7.6 and verified on hardware. |
| RØDECaster Pro II | 1.7.6 | Supported. RC2FX was originally built against this release. |
| RØDECaster Duo | 1.7.6, 1.7.7 | [Experimental](duo.md). Untested on real hardware; opt-in only. |

The screen module ([rc2fx-gui](../crates/rc2fx-gui.md)) is pinned separately to the Pro II 1.7.7 `rc_gui_juce` binary (`aa92a92d384bdb24bc1cdb01615ac280d4d4867e661158df5c7f86ff98b605ca`). On any other screen binary, including 1.7.6, the touchscreen runs completely stock.

## The catalog

`config/firmware-profiles.json` is shared by the installer, the launcher, and the native library. Each supported entry pins:

- the SHA-256 of the official archive (the installer checks RØDE's MD5 too);
- the SHA-256 of the audio engine (`/usr/bin/rc_audio_mixer`) and its launcher (`/usr/bin/audio-start.sh`);
- the addresses of the JACK process callback and the four functions in a RØDE job that RC2FX repairs, along with the 16 original bytes expected at each site;
- named ABI selectors: the callback signature (`jack-process-v1`), job capture layout (`capture-values-v1`), route layout (`pro-ii-25-v1` or `duo-23-v1`), saved-show source list (`pro-ii-sources-v1` or `duo-sources-v1`), and the 48 kHz / 112-frame audio format.

`rc2fx-firmware` validates the catalog. Every ABI name just selects code that's already compiled into RC2FX; the JSON never defines struct layouts dynamically. Unknown ABI names, misaligned addresses, duplicate identities, or missing instruction signatures are all rejected.

### Where the catalog is checked

1. The installer only builds RC2FX into catalog releases matching the connected model, newest first. It constructs download URLs from the model's firmware folder and pinned version, then verifies each download against both the pinned SHA-256 and MD5. (Stock firmware comes from RØDE's own release manifest instead; see [Stock firmware](installer.md#stock-firmware).)
2. Before pausing stock audio, the launcher runs `rc2fx-firmware check-device` to verify the engine and launcher hashes on disk.
3. The native library re-verifies the executable and instruction bytes right before installing any hooks. If anything doesn't match, it bails out and lets the stock callback run unwrapped.

The audio callback itself never parses config or allocates memory to pick a profile.

### Telling a Pro II from a Duo

Both models ship the exact same audio engine binary, so hashing `rc_audio_mixer` won't tell them apart. Instead, RC2FX checks the board directly via `/etc/hostname` and the device tree listed in `/boot/extlinux/extlinux.conf`. Both have to agree, and there's no override. A Duo profile is only accepted on a Duo board with the experimental opt-in marker present, and a Duo always gets the Duo route layout. See [Duo support](duo.md) for details.

## How 1.7.7 was qualified

1.7.7 originally shipped as a beta, which we compared offline against 1.7.6:

- We diffed nine functions instruction-by-instruction after normalizing relocations: the callback, job enqueuer, closure manager and invoker, queued body, and the parent functions that register and invoke them. Every one matched.
- We verified the job's closure layout and lifetime by running RØDE's ARM64 code under emulation.
- The launcher, updater, USB gadget script, libc, libstdc++, and libjack were all byte-identical to 1.7.6.

The full report is in [`firmware-audits/rodecaster-pro-ii-1.7.7.json`](../firmware-audits/rodecaster-pro-ii-1.7.7.json). To run it yourself:

```sh
python -m pip install -r tools/firmware_audit/requirements.txt
python -m tools.firmware_audit --baseline PATH/1.7.6/os.tar.gz \
  --candidate PATH/1.7.7/os.tar.gz --output audit.json
```

Once RØDE promoted 1.7.7 to stable, we re-downloaded both the Pro II and Duo stable archives on 2026-11-03. Both turned out byte-identical to the beta archives: same archive SHA-256 and MD5, same engine and launcher hashes, and `rc2fx-firmware check` passes every instruction-site check. None of the catalog pins had to change. That verification was offline only; we didn't run a separate hardware test for it.

## Adding a release

1. Grab the image from RØDE (`https://update.rode.com/rcp2/VERSION/os.tar.gz` for Pro II, `rcp2-duo/` for Duo) and record the archive, engine, and launcher hashes.
2. Locate the callback and job functions, then verify port and source mappings, frame format, and closure layout. If nothing structural changed, add the new addresses and their original 16-byte signatures to the catalog. If something did change, write and test a new ABI implementation first.
3. Add the profile once that review is done. The installer will start offering the release as soon as its catalog entry has a `runtime` section that the current build implements.
4. Run the local checks:

   ```sh
   cargo test -p rc2fx-firmware
   go -C desktop test ./firmware/...
   cargo run -p rc2fx-firmware -- check config/firmware-profiles.json MIXER LAUNCHER rodecaster-pro-ii
   ```

5. Test on real hardware as a separate step, coordinated with whoever owns the mixer.

`inspect CATALOG MIXER LAUNCHER MODEL` lets you check an engine binary offline without enabling that model's routes. `check` resolves addresses through the ELF segments, so it doesn't assume virtual addresses equal file offsets.

## On the mixer

The installed catalog lives at `/opt/rc2fx/firmware-profiles.json` (or alongside a RAM build). The launcher hands its path and the chosen profile to the native library via `RC2FX_FIRMWARE_PROFILES` and `RC2FX_FIRMWARE_PROFILE`, and the native audit report logs whichever profile was picked.
