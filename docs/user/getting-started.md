# Getting started

## Download the app

Download the app from the [latest release](https://github.com/ibanks42/rc2fx/releases/latest).
Downloads include the app version, system and processor in their names:

- Windows (x64): a zip.
- macOS (Apple silicon): a zip.
- Linux (x64): an AppImage, a `.deb` package (Debian, Ubuntu) or an `.rpm`
  package (Fedora, openSUSE).

The installer is built into the app, and it downloads official RØDE firmware
when needed. Updating the app is separate from updating the mixer: the app
never flashes the mixer by itself.

To start the app:

- Windows: extract the zip and run `rc2fx.exe`.
- macOS: extract the zip and run `rc2fx`. The build isn't signed or
  notarized by Apple, so macOS may ask you to allow it under **System Settings › Privacy & Security**. The
  macOS build has not been tested on a real Mac yet.
- Linux: either make the downloaded AppImage executable (for example
  `chmod +x rc2fx-*-linux-x64.AppImage`) and run it, or install the `.deb` or
  `.rpm` package with your distribution's package manager and start RC2FX
  from your applications. Linux needs some one-time setup, described [below](#linux).

If there is no download for your platform, you can
[build it from source](../developer/building.md).

## Versions and updates

The app, the RC2FX mixer software and RØDE's firmware have separate versions.
**Settings › About** shows them on one line with a status. Select **View
versions** to see each part:

| Line | Installed | Available |
| --- | --- | --- |
| **App** | The desktop app's version. | The newest published app. |
| **Mixer software** | The RC2FX mixer software on the mixer now. | The mixer software this app installs, or the published version when only a newer app carries it. |
| **RØDE firmware** | The RØDE firmware on the mixer now. | The newest firmware this app installs for your mixer model. |

Each line ends with a status. **Too old** means the mixer software doesn't speak
this app's control protocol and needs updating. **Blocked** means an update
exists but this app can't install it safely, and the reason appears below the
table.

Lines with an update have a **Read changes** button. It shows the notes for every
version between yours and the one available, newest first. Release notes for the
app and the mixer software appear in the dialog, and links to RØDE's own release
notes for firmware.

A version the app can't read shows **Unknown**. That includes a mixer running
RC2FX mixer software too old to report its version. The mixer's versions are
cleared when it disconnects and read again when it reconnects.

The bundled mixer software is the one included in the app you downloaded. An
app update can include newer mixer software, or keep the same one when only the
app changed. The Versions dialog can also show the newest mixer software
published in any app release. That is information only: the installer always
installs the mixer software bundled with the app you are running, so a newer
published version never changes what this app can install. If a newer mixer
version is only published in a newer app, update the app to get it.

The newest supported firmware is the newest version this app has
been checked against for your model, not necessarily the newest one RØDE has
published. The firmware versions are compared only with what this app bundles.
The mixer software is compared with both the bundled version and, for display,
the newest published version.

The installer flashes the mixer software and the firmware together as one
image, so the app compares both:

- If the mixer software is older than this app supports, the app offers the
  installer, as long as the installed mixer software isn't newer than the
  bundled one and the installed firmware is known and not newer than the newest
  supported one. If the firmware is newer or unknown, the dialog says this app
  can't update the mixer safely and offers nothing.
- If the mixer software was made for a newer app, the dialog says
  compatibility is unknown and asks you to update the app before changing
  anything on the mixer. It doesn't offer the installer.
- If both are known, neither is newer than what this app installs, and at
  least one is older, the dialog offers **Open installer**. The installer
  explains what happens before anything is flashed.
- If either is newer than what this app installs, the dialog says installing
  would replace it with older versions and ends with *Update the app first.* It
  doesn't offer the installer.
- Otherwise there is no offer to install: for example when both
  are current, when a version or whether the mixer software works with this
  app is unknown, or when a version can't be compared (such as a pre-release).

Nothing is flashed automatically. To update, open the installer from the Versions
dialog or from **Settings › Mixer › Open installer** and confirm there. Downloading a
new app doesn't flash the mixer, and an app update with the same bundled mixer
software leaves the mixer alone. See [Installing RC2FX](installing.md).

### App updates

The app checks GitHub for a newer RC2FX app once when it starts, and again
when you press **Check for updates** in the Versions dialog. It asks
GitHub's public releases API for the official repository
(`github.com/ibanks42/rc2fx`), without signing in, and considers only
published app releases: drafts, pre-releases and mixer software releases are
ignored. It also reads the RC2FX update feed for the release notes and the
newest published mixer version. The check at startup stays quiet if it fails,
for example when you're offline.

When a newer app is out, the app shows its version and the notes of every app
version since yours, newest first. The full histories are at
[App release history](https://ibanks42.github.io/rc2fx/updates/app) and
[Mixer software history](https://ibanks42.github.io/rc2fx/updates/mixer). If
the feed can't be loaded, the app falls back to GitHub's release notes and
says that some notes may be missing. The app never downloads or installs
anything itself: to update, download the new app from the release page.
Updating the app doesn't change the mixer; see above for updating the mixer
software.

## Linux

### Device access

Linux only lets a program open the mixer's control interface if a udev rule
allows it. The `.deb` and `.rpm` packages install this rule
(`/usr/lib/udev/rules.d/70-rc2fx.rules`) and reload udev, so with them there is
nothing to set up. With the AppImage, the first time access is refused, the app explains what it needs:
the file `/etc/udev/rules.d/70-rc2fx.rules`, which it can show in full, and
how to remove it later. **Allow access** installs the rule through your
system's password prompt. The password goes to the system prompt, and the app
does not see it.

To install the rule by hand, save
[`config/udev/70-rc2fx.rules`](../../config/udev/70-rc2fx.rules) from the
repository, then run this from the folder you saved it in:

```bash
sudo cp 70-rc2fx.rules /etc/udev/rules.d/
sudo udevadm control --reload-rules
sudo udevadm trigger --action=add --subsystem-match=hidraw
```

The rule matches RØDECaster mixers only and grants access to the user logged
in at the computer. It takes effect immediately, without replugging the
mixer. If you get a permission error, fix the rule. Running the app as root is
not supported.

### Administrator access for installing

Switching the mixer into update mode and writing firmware need administrator
rights. The installer asks for your password once, when you press **Grant
access**. The app itself keeps running as your user. On some minimal
desktops, the password prompt only appears if a polkit authentication agent
is running.

### Graphics

The app needs a graphical desktop with working Vulkan drivers.

## Connect the mixer

Connect the computer to the mixer's **USB 1** port. USB 2 does not carry the
control connection. The app finds the mixer automatically and reconnects when
the cable or the mixer comes back. It does not use Wi-Fi or Ethernet.

What the app shows next depends on the mixer:

- If RC2FX is installed, the app opens on the Overview. Continue with
  [Using the app](using-the-app.md).
- If the mixer runs stock firmware, the app reports that RC2FX is not
  installed and offers **Install RC2FX**. Continue with
  [Installing RC2FX](installing.md).

RC2FX shares USB 1 with RØDE's software. The RØDECaster app and RØDE's
Windows virtual audio devices keep working while RC2FX is running.
