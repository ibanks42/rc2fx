# Installer internals

The installer flashes official RØDE firmware with RC2FX added to it. It
builds a modified root filesystem image on the computer containing the
runtime, the start-up shims and the saved settings, and the mixer's USB
update mode installs it. Installing, updating and repairing all use the same
flash. Firmware images are not stored in git.

The installer engine is `crates/rc2fx-installer`, written in Rust with no
external tools. The installer page is `crates/rc2fx-app/src/installer`.

## Inputs

The desktop executable embeds the [firmware catalog](firmware.md), the mixer
runtime and the installer scripts at build time. The installer and its
privileged helper use only those embedded bytes. They ignore files beside the
executable or in the working directory. Developer tools and tests can pass a
folder explicitly.

Before embedding, the build checks the payload checksums, checks that the
binaries are ARM64, and checks that the runtime matches the catalog. Missing
or corrupt inputs fail the build. Official RØDE firmware is downloaded and
checked at install time.

Working files go in `RC2FX/installer` under the user data folder:

| OS | Data folder |
| --- | --- |
| Windows | `%LOCALAPPDATA%` |
| macOS | `~/Library/Application Support` |
| Linux | `$XDG_DATA_HOME`, or `~/.local/share` |

`RC2FX_INSTALLER_WORK` overrides the location. The folder holds
`installer.json`, the prepared firmware, settings backups and the command
log. Only one installer can use a given folder at a time.

### Platforms

- Windows uses its built-in HID and volume APIs.
- macOS uses IOKit, `ioreg` and `diskutil`.
- Linux needs root for raw USB, mounting and ejecting. The app itself runs as
  the user, and so do checking the mixer, listing releases, saving settings
  and preparing firmware. Switching to update mode and flashing are done by a
  helper started through `pkexec`, which is the same executable in
  installer-helper mode. The helper starts when the user presses
  **Grant access** on the installer page, which opens the password prompt.
  Access lasts until the installer closes. Files the helper creates are owned
  by the invoking user.

HTTPS uses the operating system's trust store.

## The plan

The installer checks the mixer on USB 1 (normal or update mode, and its
serial number) and asks the app's connection whether RC2FX is running. It
then builds a single plan and skips any step the mixer already satisfies. One
confirmation starts the plan, which runs to the end unattended. It stops at
the first error and offers **Retry** from that step.

1. **Save settings.** If RC2FX is running, the app reads its settings file
   over USB with `GET /api/v1/settings`. This is `state.json`, containing
   effects and presets. The installer stores it against the mixer's USB
   serial.
2. **Prepare firmware.** The installer downloads the chosen release, checks
   RØDE's MD5 and the catalog's SHA-256, and builds a copy of the root
   filesystem with these additions, all owned by root:
   - `/opt/rc2fx/`: the runtime bundle with `bundle.sha256`,
     `mixer-release.json` (the RC2FX mixer software version, listed in the
     bundle's `SHA256SUMS` and reported by
     [`GET /api/v1/version`](api.md#versions)), `data/state.json` (the newest saved settings for this mixer) and `logs/`.
   - `/usr/sbin/rc_audio_mixer`: the audio engine shim (see
     [Architecture](../developer/architecture.md#starting-with-the-mixer)).
   - `/usr/sbin/rc_gui_juce`: the matching shim for the screen program.
   - With **Developer options › SSH access** enabled: RC2FX's SSH public key
     in root's `authorized_keys`, and a host key kept in the cache folder so
     the mixer keeps the same identity across flashes. Without the option,
     `authorized_keys` stays empty, as RØDE ships it.

   Files in RØDE's image are left as they are. The installer refuses an image
   that already contains any of these paths, requires the audio binaries to
   match a supported profile, and requires the runtime to support that
   profile. A prepared image is reused when the release, runtime, key and
   settings all match.
3. **USB update mode.** HID command `M` puts the mixer in update mode. Audio
   stops and the screen shows "Updating via USB". Nothing has been written
   yet.
4. **Flash.** When the mixer's update drive appears, the installer
   identifies it by the mixer's USB serial and its capacity, copies
   `archive.tar.gz` and `archive.md5` to it, reads them back, flushes, and
   ejects it. It then checks that the drive is gone and that the same mixer
   is still in update mode, and sends HID `U`. Any failure stops the plan
   before `U` is sent. Up to that point, removing power for 30 seconds
   returns the mixer to its current firmware.
5. **Wait for RC2FX.** The mixer installs the update and restarts. On its
   first boot after the update it reflashes its controllers and restarts
   again, and the shim runs stock during that boot. On the following boot the
   first engine start loads RC2FX. The app keeps reconnecting over USB 1. The
   plan finishes when RC2FX reports the engine running, audio advancing and
   the saved settings applied. The computer sends nothing to the mixer during
   these boots.

### Offered releases

The installer offers the [catalog's](firmware.md) releases for the connected
model, newest first. It does not read RØDE's release manifest, so new or
withdrawn RØDE releases have no effect. The download URL is built from the
model's firmware folder and the pinned version, for example
`https://update.rode.com/rcp2/1.7.7/os.tar.gz` with `os.md5` beside it. The
archive must match the catalog's SHA-256 and MD5.

### Stock firmware

The **Stock firmware** card flashes one of the same pinned releases exactly
as RØDE publishes it. This removes RC2FX and its SSH key. A cached stock
image that is not pinned is refused. Settings are saved first. The plan
finishes when the mixer is back in normal USB mode.

### USB commands

Each `M` and `U` is logged before it is sent and then marked as acknowledged
or unacknowledged. The mixer may have received a command even without an
acknowledgement, so the installer does not resend commands automatically. A
flash within the last 20 minutes is treated as an update in progress. Windows
can pad a HID write and report more bytes written than were sent, so only a
short write count is treated as an error.

The command sequence comes from a
[published write-up of the Duo's update process](https://hhh.hn/rodecaster-duo-fw/)
and its traces. The Pro II's USB product IDs come from RØDE's
`uac2_gadget.sh`: `0037`, `0078`, `008d`, `0092` and `0094` in normal modes,
and `0030` in update mode.

## Release package

`just package`, or `cargo xtask package` after `just mixer-build`, writes
`target/package/rc2fx-<os>-<arch>.zip` on Windows and macOS, or
`rc2fx-linux-x64.AppImage` on Linux (see
[packaging](../developer/building.md#3-package-it)). It holds:

- the self-contained app,
- the project licence, third-party notices, training-data credits and every
  crate's licence text, built into the app under **Settings › Versions › Licenses › View licenses** (the
  AppImage also carries them as `LICENSES.html`),
- on Windows and macOS, a `SHA256SUMS` covering every file.

The runtime, catalog, `mixer-release.json` and scripts are embedded in the app and are not shipped
as separate files. Packaging inspects the executable's embedded payload and
fails if it is missing, corrupt, stale or from the wrong build kind. This
check also runs with `--skip-build`.

`--target TRIPLE` builds another architecture for the same OS when that
toolchain is installed. `--skip-build` reuses an existing release build.
Builds are unsigned.

## Test status

The installer has flashed a real Pro II from Windows, and RC2FX came up
afterwards with its settings applied. Flashing from Linux and macOS has not
been tried on hardware, and the macOS backend has not yet been built on a
Mac.

The automated tests cover image patching, the added files, runtime
integrity, settings backups, key generation, the command log, HID write
quirks, update-drive ownership, ejection failures, cancellation, and plan
steps and retries. They do not replace testing on hardware.
