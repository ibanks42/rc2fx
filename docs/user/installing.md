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
- The computer needs an internet connection to download the firmware from
  RØDE.
- On Linux, the installer asks for administrator access first. Press
  **Grant access** and enter your password when the system prompts for it.

## Install steps

Choose **RC2FX firmware** and a version, press **Install…** (or **Update…**)
and confirm. The installer then runs these steps. If a step fails, the
installer stops, and **Retry** continues from that step.

1. **Save settings.** If RC2FX is already running, its settings and presets
   are copied to the computer so the new install keeps them.
2. **Prepare firmware.** The installer downloads the chosen version from
   RØDE, verifies it, and builds a copy with RC2FX and your settings added.
   The mixer is unchanged at this point.
3. **Switch to USB update mode.** The mixer stops audio and shows "Updating
   via USB". Nothing has been written yet.
4. **Flash firmware.** The installer copies the firmware to the mixer and
   starts the install. Up to this step, switching the mixer off for 30
   seconds brings it back on its previous firmware.
5. **Wait for RC2FX.** The mixer installs the update and usually restarts
   three times: to install, to update its internal controllers, and to start
   normally. Don't touch it during this time. The installer finishes when
   RC2FX reports that it is running with your settings.

## Firmware versions

RC2FX supports RØDE firmware 1.7.7 (the default) and 1.7.6. The installer
only offers versions whose files have been checked against RC2FX, so a new
RØDE release appears only after it has been checked.

## Going back to stock

Choose **Stock firmware** and press **Flash stock firmware…**. This flashes
an unmodified RØDE release and removes RC2FX. Your RC2FX settings are saved
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
  that already has it. To remove it, flash **Stock firmware** with the
  setting still on.

The [Duo reference](../reference/duo.md) lists the channels RC2FX supports on
a Duo.

## SSH access (developers only)

**Developer options › SSH access** adds this computer's SSH key to the
firmware so a developer can log in to the mixer. Installing and using RC2FX
don't require it. Leave it off unless you are developing RC2FX.

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

The installer has flashed a real Pro II from Windows, and RC2FX started
afterwards with its settings. Flashing from Linux and macOS has not been
tested on hardware, and no Duo has been flashed.
