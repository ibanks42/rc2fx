# Installing RC2FX

The installer downloads official RØDE firmware, adds RC2FX and any saved
settings to it on your computer, and flashes the result through the mixer's
USB update mode. RC2FX then starts automatically each time the mixer turns
on. Installing, updating and repairing all use the same process.

When the app finds a mixer without RC2FX, it offers **Install RC2FX**. On a
mixer that already runs RC2FX, open the installer from **Settings › Mixer ›
Open installer**.

## Before you start

- Audio stops while the mixer updates, and the update takes a few minutes.
- Keep the mixer switched on and connected to USB 1 until the installer
  reports that RC2FX is installed.
- The computer needs an internet connection to check signed mixer releases,
  download RC2FX from GitHub, and download the firmware from RØDE.
- On Linux, the installer asks for administrator access first. Press
  **Grant access** and enter your password when the system prompts for it.

## Install steps

Choose the RØDE firmware version and the RC2FX mixer software to install. The
mixer software menu lists signed releases compatible with this app and the
chosen firmware. Check the installed and offered versions,
press **Install…** (or **Update…**), and confirm. Selecting an older version
requires an explicit downgrade confirmation.

Firmware profiles are downloaded as signed metadata from GitHub. If a mixer
release or firmware profile needs a newer app, the installer shows the minimum
version and asks you to update the app first. Without a verified catalog, a
download-only build offers no firmware choices.

Normal app builds download mixer software separately. A build made with the
optional embedded payload also offers its bundled version as an explicit choice.
A failed download never silently substitutes that version. See
[Versions and updates](getting-started.md#versions-and-updates).

The installer then runs these steps. If a step fails, the
installer stops, and **Retry** continues from that step.

1. **Save settings.** If RC2FX is already running, its settings and presets
   are copied to the computer so the new install keeps them.
2. **Mixer software.** The installer downloads and verifies the selected
   release, including its signature, checksum, and compatibility. On Linux,
   the administrator helper independently verifies the same release. Previously
   downloaded files are checked again before reuse. The optional bundled choice
   is checked locally instead. No mixer changes happen during this step.
3. **Prepare firmware.** The installer downloads the chosen version from
   RØDE, verifies it, and builds a copy with RC2FX and your settings added.
   The mixer is unchanged at this point.
4. **Switch to USB update mode.** The mixer stops audio and shows "Updating
   via USB". Nothing has been written yet.
5. **Flash firmware.** The installer copies the firmware to the mixer and
   starts the install. Up to this step, switching the mixer off for 30
   seconds brings it back on its previous firmware.
6. **Wait for RC2FX.** The mixer installs the update and usually restarts
   three times: to install, to update its internal controllers, and to start
   normally. Don't touch it during this time. The installer finishes when
   RC2FX reports that it is running with your settings.

## Firmware versions

RC2FX supports RØDE firmware 1.7.7 (the default) and 1.7.6. The installer
only offers versions whose files have been checked against RC2FX, so a new
RØDE release appears only after it has been checked.

## Going back to stock

Switch to the **Stock firmware** tab, choose a version, press **Flash stock
firmware…** and confirm. The menu lists the stable and beta firmware RØDE publishes, even
versions RC2FX doesn't support yet. This flashes an unmodified RØDE release
and removes RC2FX. Your RC2FX settings are saved
to the computer first and are restored if you install RC2FX again. The mixer
may restart once more afterwards to finish updating its controllers.

To pause processing without removing RC2FX, see
[Turning RC2FX off](using-the-app.md#turning-rc2fx-off).

## RØDECaster Duo (experimental)

RC2FX can be flashed onto a RØDECaster Duo, but it has not been tested on a
real Duo. Flashing or recovery could fail and leave the mixer unusable, and
the audio could be wrong or loud. Only try it if you accept that risk.

Duo flashing is off by default. To enable it, turn on **Allow experimental
Duo flashing** in the installer. The app remembers this setting on the
computer. Every Duo flash still shows a warning and asks you to confirm with
**Flash Duo at my own risk**.

- The installer offers only official Duo firmware (1.7.6 and 1.7.7) for a
  Duo.
- Back up your RØDE show and unplug headphones and speakers before flashing.
- Leave the mixer alone after the flash while it updates and restarts.
- Turning the setting off blocks future Duo flashes but leaves RC2FX on a Duo
  that already has it. To remove it, use **Flash stock firmware…** with
  the setting still on.

The [Duo reference](../reference/duo.md) lists the channels RC2FX supports on
a Duo.

## SSH access (developers only)

The installer doesn't add SSH access. Developers can turn it on and off
without reflashing under **Settings › Developer**; see [Working on a
mixer](../developer/mixer-workflow.md#1-enable-ssh-access). Installing and
using RC2FX don't need it.

## Installer files

The installer stores downloads, settings backups and a log of every command
sent to the mixer in an `RC2FX/installer` folder inside your user data
folder:

| System | User data folder |
| --- | --- |
| Windows | `%LOCALAPPDATA%` |
| macOS | `~/Library/Application Support` |
| Linux | `~/.local/share` |

## Testing status

The installer has flashed a real Pro II from Windows and macOS, and RC2FX
started afterwards with its settings on Windows. Flashing from Linux has not
been tested on hardware, and no Duo has been flashed.
