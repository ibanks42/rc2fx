# Getting started

## Download the app

There is one download per operating system:
[`rc2fx-windows-x64.zip`](https://github.com/ibanks42/rc2fx/releases/latest/download/rc2fx-windows-x64.zip),
[`rc2fx-macos-arm64.zip`](https://github.com/ibanks42/rc2fx/releases/latest/download/rc2fx-macos-arm64.zip) or
[`rc2fx-linux-x64.AppImage`](https://github.com/ibanks42/rc2fx/releases/latest/download/rc2fx-linux-x64.AppImage).
The installer is built into the app, and it downloads official RØDE firmware
when needed. Updating the app is separate from updating the mixer: the app
never flashes the mixer by itself.

To start the app:

- Windows: extract the zip and run `rc2fx.exe`.
- macOS: extract the zip and run `rc2fx`. The download is still a ZIP, and
  Apple signing and notarization are still deferred, so macOS may ask you to
  allow the unsigned build under **System Settings › Privacy & Security**. The
  macOS build has not been tested on a real Mac yet.
- Linux: make the file executable (`chmod +x rc2fx-linux-x64.AppImage`) and
  run it. Linux needs some one-time setup, described [below](#linux).

If there is no download for your platform, you can
[build it from source](../developer/building.md).

## Versions and updates

The app, the RC2FX mixer software and RØDE's firmware have separate versions.
**Settings › Versions**, below the **Mixer** section, shows them:

| Label | Shows |
| --- | --- |
| **App** | The desktop app's version. |
| **RC2FX mixer software (installed)** | The RC2FX mixer software on the mixer now. |
| **RC2FX mixer software bundled with this app** | The mixer software this app installs. |
| **RØDE firmware (installed)** | The RØDE firmware on the mixer now. |
| **Newest supported RØDE firmware** | The newest firmware this app installs for your mixer model. |
| **Works with this app** | **Yes** when the mixer software speaks this app's control protocol, **No** when it doesn't, **Unknown** when the mixer doesn't say. |

A version the app can't read shows **Unknown**. That includes a mixer running
RC2FX mixer software too old to report its version. The mixer's versions are
cleared when it disconnects and read again when it reconnects.

The bundled mixer software is the one included in the app you downloaded. An
app update can include newer mixer software, or keep the same one when only the
app changed. The newest supported firmware is the newest version this app has
been checked against for your model, not necessarily the newest one RØDE has
published. The mixer software and firmware versions are compared only with
what this app bundles; the app doesn't look online for them.

The installer flashes the mixer software and the firmware together as one
image, so the app compares both:

- If the mixer software is older than this app supports, the app offers the
  installer, as long as the installed mixer software isn't newer than the
  bundled one and the installed firmware is known and not newer than the newest
  supported one. The notice says *Installing the bundled mixer software flashes
  the mixer, which then restarts*. If the firmware is newer or unknown, the
  notice says this app can't update the mixer safely and offers nothing.
- If the mixer software was made for a newer app, the notice says
  compatibility is unknown and asks you to update the app before changing
  anything on the mixer. It doesn't offer the installer.
- If both are known, neither is newer than what this app installs, and at
  least one is older, the app shows a notice such as *Newer RC2FX mixer
  software is available. Installing flashes the mixer, which then restarts;
  audio stops until it is back. Your drafts are kept.* and offers to open the
  installer.
- If either is newer than what this app installs, the notice says installing
  would replace it with older versions and ends with *Update the app first.* It
  doesn't offer the installer.
- Otherwise there is no notice and no offer to install: for example when both
  are current, when a version or whether the mixer software works with this
  app is unknown, or when a version can't be compared (such as a pre-release).

Nothing is flashed automatically. To update, open the installer from the notice
or from **Settings › Mixer › Open installer** and confirm there. Downloading a
new app doesn't flash the mixer, and an app update with the same bundled mixer
software leaves the mixer alone. See [Installing RC2FX](installing.md).

### App updates

The app checks GitHub for a newer RC2FX app once when it starts, and again
when you press **Check for updates** in **Settings › Versions**. It asks
GitHub's public releases API for the official repository
(`github.com/ibanks42/rc2fx`), without signing in, and considers only
published app releases: drafts, pre-releases and mixer software releases are
ignored. The check at startup stays quiet if it fails, for example when you're
offline.

When a newer app is out, the app shows its version, the release notes of
every app version since yours, newest first, and a **View release** link to
its GitHub page. If some notes couldn't be loaded, it says so and links to
all releases. The app never downloads or installs anything itself: to update,
download the new app from the release page. Updating the app doesn't change
the mixer; see above for updating the mixer software.

## Linux

### Device access

Linux only lets a program open the mixer's control interface if a udev rule
allows it. The first time access is refused, the app explains what it needs:
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
