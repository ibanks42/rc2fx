# Installing RC2FX

The built-in installer downloads official RØDE firmware, adds RC2FX and any
saved settings to it on your computer, and flashes the result using the mixer's
USB update mode. Once installed, RC2FX starts automatically every time you turn
the mixer on. First-time installs, updates, and repairs all follow the same
flow.

If your mixer doesn't have RC2FX yet, the app offers **Install RC2FX** on the
connection screen. If you're already running RC2FX, open the installer from
**Settings › Mixer › Update RC2FX**.

## Before you start

- Audio stops during the update, and the whole process takes a few minutes.
- Leave the mixer powered on and plugged into **USB 1** until the installer
  says it's done.
- You need an internet connection so the app can check signed RC2FX releases,
  download the mixer software from GitHub, and fetch official firmware from
  RØDE.
- On Linux, click **Grant access** first and enter your password when prompted.

## Install steps

Pick the RØDE firmware version and the RC2FX mixer software you want to
install. The mixer software menu only shows signed releases that are compatible
with your app and chosen firmware. If a release or firmware profile requires a
newer desktop app, the installer tells you the minimum version and asks you to
update the app first.

Double-check the installed and selected versions, click **Install…** (or
**Update…**), and confirm. Picking an older version asks for an explicit
downgrade confirmation.

The installer then walks through six steps. If anything fails, it stops where
it is and **Retry** picks up from that step:

1. **Save settings.** If RC2FX is already running on the mixer, your settings
   and presets are backed up to your computer so they carry over.
2. **Mixer software.** The installer downloads the selected RC2FX release and
   verifies its signature, checksum, and compatibility. Nothing on the mixer
   changes yet.
3. **Prepare firmware.** The installer downloads the chosen RØDE firmware,
   verifies it, and builds a copy with RC2FX and your saved settings included.
   The mixer is still untouched.
4. **Switch to USB update mode.** The mixer stops audio and displays "Updating
   via USB". Nothing has been written yet.
5. **Flash firmware.** The installer copies the prepared firmware to the mixer
   and starts the update. Up until this step, turning the mixer off for 30
   seconds brings it right back on its previous firmware.
6. **Wait for RC2FX.** The mixer installs the firmware and typically restarts
   three times: once to install, once to update its internal controllers, and
   once to boot normally. Don't touch it while this is happening. The installer
   finishes once RC2FX comes up with your settings restored.

## Firmware versions

RC2FX supports RØDE firmware 1.7.7 (the default) and 1.7.6. The installer only
offers firmware versions that have been verified against RC2FX, so brand-new
RØDE releases won't appear here until they've been checked.

## Going back to stock

Open the **Stock firmware** tab, pick a version, click **Flash stock
firmware…**, and confirm. This list includes RØDE's stable and beta releases,
even versions RC2FX doesn't support yet. Flashing stock installs an unmodified
RØDE image and removes RC2FX.

Your RC2FX settings are backed up to your computer first, so they'll be
restored automatically if you ever reinstall RC2FX. The mixer may reboot one
extra time afterward to finish updating its internal controllers.

If you just want to bypass effects without uninstalling RC2FX, see
[Turning RC2FX off](using-the-app.md#turning-rc2fx-off).

## RØDECaster Duo (experimental)

RC2FX can be flashed onto a RØDECaster Duo, but it has never been tested on
real Duo hardware. Flashing or recovery could fail and leave the mixer
unusable, and audio could come out wrong or unexpectedly loud. Only try this
if you're comfortable taking that risk.

Duo flashing is locked off by default. With a Duo connected, turn on **RC2FX is
untested on the Duo. Install anyway.** on the connection screen or installer
page. The app remembers this choice on your computer.

- The installer only offers official Duo firmware (1.7.6 and 1.7.7) when a Duo
  is connected.
- Back up your RØDE show and unplug headphones and speakers before flashing.
- Leave the mixer alone after the flash while it updates and restarts.
- Turning the switch back off blocks future Duo flashes, but leaves RC2FX
  installed on a Duo that already has it. To remove RC2FX from a Duo, use
  **Remove RC2FX** under **Settings › Mixer** while the switch is still on.

See the [Duo reference](../reference/duo.md) for the list of channels RC2FX
supports on a Duo.

## SSH access (developers only)

The installer never adds SSH access. Developers can turn SSH on and off without
reflashing under **Settings › Developer**; see
[Working on a mixer](../developer/mixer-workflow.md#1-enable-ssh-access). You
don't need SSH to install or use RC2FX.

## Installer files

Downloads, settings backups, and a log of every command sent to the mixer live
in `RC2FX/installer` inside your user data folder:

| System | User data folder |
| --- | --- |
| Windows | `%LOCALAPPDATA%` |
| macOS | `~/Library/Application Support` |
| Linux | `~/.local/share` |

## Testing status

The installer has been tested on a real RØDECaster Pro II from Windows, Linux,
and macOS, with RC2FX starting up afterward and restoring its settings. No Duo
has been flashed yet.
