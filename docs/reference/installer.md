# Installer internals

The installer flashes official RØDE firmware with RC2FX integrated into it. It
constructs a modified root filesystem archive on the host computer containing
the RC2FX runtime, startup shims, and saved settings, then installs it through
the mixer's USB update mode. Initial installs, repairs from a broken state, and
RØDE firmware upgrades all use this full flash. When only RC2FX itself changes,
the app instead updates the signed mixer payload in place: it copies the file
through a temporary USB drive named `RC2FX` attached to RØDE's removable drive,
falling back to USB 1 when that isn't possible (see
[signed mixer releases](../developer/mixer-releases.md#in-place-updates)),
keeping the full flash as the recovery path. Firmware archives are never stored
in git.

The core engine lives in `desktop/installer`, written in pure Go with no
external CLI dependencies; Repair mixer reads and writes ext4 via libext2fs,
vendored under `desktop/ext2fs` and linked through cgo. The UI lives in
`desktop/app/installer.go` with state transitions in `desktop/app/installflow`.

## Inputs

When built with an embedded payload, the desktop binary compiles in the
[firmware catalog](firmware.md), the mixer runtime, and the installer scripts
at build time. The installer and its privileged helper read only those embedded
bytes and ignore loose files next to the binary or in the working directory.
Developer commands and tests can pass an explicit directory instead.

Before an `embedded` build compiles, `desktop/cmd/stage-payload` verifies
payload checksums, confirms the binaries are ARM64, and checks that the runtime
matches the catalog; any missing or corrupt input aborts the build. Official
RØDE firmware is always downloaded and verified at install time.

Working files live under `RC2FX/installer` inside the platform's user data
directory:

| OS | Data folder |
| --- | --- |
| Windows | `%LOCALAPPDATA%` |
| macOS | `~/Library/Application Support` |
| Linux | `$XDG_DATA_HOME`, or `~/.local/share` |

Set `RC2FX_INSTALLER_WORK` to override this path. The directory stores
`installer.json`, prepared firmware archives, settings backups, and the USB
command log. A lock prevents two installer instances from sharing the same work
folder at once.

### Platforms

- **Windows:** Uses native Windows HID and volume APIs.
- **macOS:** Uses IOKit, `ioreg`, and `diskutil`.
- **Linux:** Raw USB access, mounting, and ejecting require root. The desktop
  app itself stays unprivileged (handling mixer inspection, release listings,
  and settings backups as the normal user), while firmware preparation, update
  mode switching, and flashing run in a helper launched via `pkexec` (the same
  binary started in installer-helper mode). Because the cache directory is
  user-writable, the helper refuses to flash any RC2FX image it didn't build
  itself. Clicking **Grant access** on the installer page starts the helper
  through the system password prompt, and privileges remain active until the
  installer closes. Files written by the helper are chowned back to the
  invoking user. For in-place updates, mounting and unmounting the temporary
  `RC2FX` update drive runs directly in the unprivileged desktop session via
  `udisksctl` with no password prompt (falling back to USB 1 if that isn't
  available).

All HTTPS requests rely on the operating system's certificate trust store.

## The plan

The installer inspects the mixer on USB 1 (normal or update mode, plus its
serial number) and checks with the app's active connection whether RC2FX is
running. It builds a single linear plan and skips any step already satisfied.
Once confirmed, the plan executes unattended until completion or the first
error, where it stops and offers **Retry** from that step. Retrying keeps the
confirmed firmware, mode, and mixer software; switching modes cancels the
failed run.

1. **Save settings.** If RC2FX is running, the app pulls `state.json` (effects
   and presets) over USB via `GET /api/v1/settings` and saves it keyed by the
   mixer's USB serial number.
2. **Prepare firmware.** The installer downloads the selected RØDE release,
   verifies RØDE's MD5 and the catalog's SHA-256, and writes a copy of the root
   filesystem with these root-owned additions:
   - `/opt/rc2fx/`: the runtime bundle with `bundle.sha256`,
     `mixer-release.json` (the mixer software version, covered by the bundle's
     `SHA256SUMS` and served at [`GET /api/v1/version`](api.md#versions)),
     `data/state.json` (the latest saved settings for this mixer), and `logs/`.
   - `/usr/sbin/rc_audio_mixer`: the audio engine shim (see
     [Architecture](../developer/architecture.md#starting-with-the-mixer)).
   - `/usr/sbin/rc_gui_juce`: the matching shim for the screen process.
   - `/opt/rc2fx/image.sha256`: SHA-256 hashes of every regular RØDE file in
     the archive, used by the on-device [image check](#image-check). It omits
     files RØDE modifies or deletes itself (`authorized_keys`,
     `/boot/extlinux/extlinux.conf`, which RØDE's updater points at the second
     root slot on install, `/etc/asound.conf`, `/etc/timestamp`,
     `/usr/bin/first-boot`, and `/var/lib/alsa/asound.state`) as well as
     RC2FX's own files (already covered by the bundle's `SHA256SUMS`).

   `authorized_keys` stays empty just as RØDE ships it; SSH access is toggled
   post-install over USB 1 (see [Developer SSH](api.md#developer-ssh)). Any
   cached archive built by an older installer with its SSH option is never
   reused or flashed.

   Existing RØDE files are never modified. The installer rejects any firmware
   archive that already contains RC2FX's target paths, requires the audio
   binaries to match a qualified profile, and confirms the runtime supports
   that profile. A previously prepared image is reused only when the firmware
   release, runtime, and saved settings all match.

   RØDE's updater extracts the archive into whichever of its two root
   partitions is currently inactive: 476,546 1 KiB blocks (465 MiB) and 142,585
   4 KiB blocks (557 MiB) on a Pro II running 1.7.7. The installer estimates
   the unpacked footprint for both block sizes (rounding each file up to whole
   blocks and counting one block per directory) and refuses any image that
   would leave less than 48 MiB free on either slot. On 1.7.7, this estimate
   matched actual usage within 0.3 MiB on both partitions, with the 0.2.0 image
   leaving roughly 90 MiB free on the smaller slot.
3. **USB update mode.** Sending HID command `M` puts the mixer into update
   mode. Audio stops and the screen displays "Updating via USB", with nothing
   written yet. Mixers reporting a placeholder USB serial (two letters followed
   by zeros, like `RD0000000`) are rejected here and in the next step: RØDE's
   USB init produces that serial when `unit-serial.txt` is missing because the
   settings partition didn't mount, and the update drive resides on that same
   partition.
4. **Flash.** Once the mixer's update drive mounts, the installer matches it by
   USB serial, verifies free space, copies `archive.tar.gz` and `archive.md5`,
   reads both back to verify, flushes, and ejects the volume. It confirms the
   drive has disappeared and the same mixer is still in update mode, then sends
   HID `U`. Any failure before `U` aborts the plan cleanly; up to that point,
   removing power for 30 seconds restores the mixer to its previous firmware.

   Update drive capacity varies by unit. RØDE's engine creates the image with
   `dd count=560000` on its staging partition (a 286,720,000-byte ceiling), and
   drives as small as 224 MB have been observed on a Pro II.
5. **Wait for RC2FX.** The mixer unpacks the update and reboots. On the first
   boot after an update it flashes its internal controllers and reboots again,
   with the shim running stock during that pass. On the subsequent boot, the
   first engine launch brings up RC2FX. The app polls USB 1 to reconnect,
   finishing once RC2FX reports the engine running, audio advancing, and saved
   settings applied, then waits for the [image check](#image-check). The host
   sends no commands to the mixer during these reboots.

### Image check

On every boot, `rc2fx.sh launch` kicks off `rc2fx.sh image-check` in the
background unless it is RØDE's first boot after an update or the check has
already passed for this image. It runs `sha256sum -c` against `image.sha256`
from `/` at the lowest CPU and I/O priority. For any mismatched file, it mounts
the inactive root partition read-only and, if the copy there matches the
expected hash, writes it back (in place over the file's existing blocks when
the size matches so it works even on a full root, otherwise via a temporary
sibling file). Nothing else is touched. Results are written to
`/opt/rc2fx/data/image-check` as `key=value` pairs tagged with the manifest's
SHA-256 (preventing stale records from an earlier image from being reported)
and served by `rc2fxd` at [`GET /api/v1/image-check`](api.md#image-check).

Once RC2FX is up, the installer polls that endpoint every 3 seconds:

- `ok` or `none` (no manifest, or older mixer software predating the check):
  the plan completes.
- `repaired`: because RØDE's boot already ran with the corrupted files, the
  installer prompts for a reboot and waits; the check passes on the next boot.
- `damaged`: the plan halts and asks the user to install again.

This safeguard exists because a flash once left
`/usr/bin/rode-init-update-env.sh` zero-filled on the new root (correct file
size, all zero bytes). Because that script mounts the settings partition, the
mixer booted without it, lost its setup on every reboot, and let RØDE's engine
fill the root partition with its update-drive image. Reflashing the exact same
archive worked, showing the corruption happened during unpack rather than in
the archive itself.

### Offered releases

RC2FX only patches releases listed in the [catalog](firmware.md) for the
connected model, ordered newest first. Download URLs combine the model's
firmware folder and pinned version (for example,
`https://update.rode.com/rcp2/1.7.7/os.tar.gz` and its companion `os.md5`).
Every archive must match both the catalog's SHA-256 and MD5.

### Stock firmware

The **Stock firmware** tab flashes an unmodified RØDE release, removing RC2FX
and any RC2FX-managed SSH key. Available versions come from RØDE's model
manifest (the same manifests the RØDECaster App reads):
`https://update.rode.com/manifest.json` for Pro II and
`https://update.rode.com/rcp2-mini-manifest.json` for Duo. Only the stable
(`main-version`) and beta (`beta-version`) entries are listed, and only when
both URLs stay inside the model's folder on `update.rode.com` (`rcp2/` or
`rcp2-duo/`). Private RØDE channels are never shown, and downloads and
redirects are restricted to `update.rode.com`.

Each download must match RØDE's `os.md5` (plus the catalog's SHA-256 and MD5 if
pinned). For unpinned stock images, the Linux helper records a digest when
staging and only flashes the image if it still matches that record. RC2FX
settings are backed up first, and the plan completes once the mixer returns to
normal USB mode.

### USB commands

Both `M` and `U` commands are logged to disk before transmission and then
marked acknowledged or unacknowledged. Because the mixer may have acted on a
command even if its reply was lost, the installer never auto-resends them. Any
flash issued within the past 20 minutes is treated as an in-progress update. On
Windows, HID writes can be padded and report more bytes written than sent, so
only a short write count is treated as a failure.

The update protocol comes from a
[published write-up of the Duo's update process](https://hhh.hn/rodecaster-duo-fw/)
and its USB traces. Pro II USB product IDs come from RØDE's `uac2_gadget.sh`:
`0037`, `0078`, `008d`, `0092`, and `0094` in normal modes, and `0030` in update
mode.

## Release package

`just package` (or `./xtask package`) builds release packages for the current
OS into `target/package` (see [packaging](../developer/building.md#3-package-it)):
on Windows and macOS, an installer and the Velopack update package and feed; on
Linux, an AppImage plus its update package and feed, all created by Velopack's
`vpk`. Each package includes:

- the self-contained desktop executable, and
- the project license, third-party notices, training-data credits, and license
  texts for every Rust crate, Go module, and gpuikit component, viewable inside
  the app under **Settings › About › View licenses** (the AppImage also ships
  them as `LICENSES.html`).

Standard release builds download mixer payloads at install time. Passing
`--embedded` compiles the runtime, catalog, `mixer-release.json`, and scripts
directly into the executable rather than shipping separate files. Packaging
inspects the compiled binary's payload mode (and its embedded payload when
present), failing if it is missing, corrupt, stale, or mismatched. This check
runs even with `--skip-build`.

`--target TRIPLE` (using Rust-style triples such as `x86_64-unknown-linux-gnu`)
targets another architecture for the same OS when cgo supports it and the
matching gpuikit and Velopack libraries are present. macOS packages are always
universal (`arm64` and `x86_64`), regardless of which Apple triple is passed.
`--skip-build` reuses an existing release binary, and `--build-only` stops
after compilation. Windows and macOS packages are signed only when signing
credentials are configured; see [Signing](../developer/releases.md#signing).

## Test status

The installer has been verified on a physical RØDECaster Pro II from Windows,
Linux, and macOS, with RC2FX booting afterward and restoring its settings. No
Duo has been flashed.

Automated tests cover image patching, added files, runtime integrity, settings
backups, key generation, command logging, HID write quirks, update-drive
ownership, ejection failures, cancellation, and plan execution/retries. They do
not replace testing on real hardware.
