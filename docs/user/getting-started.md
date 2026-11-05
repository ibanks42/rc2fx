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
| **Mixer software** | The RC2FX mixer software on the mixer now. | The newest compatible signed mixer release, or an optional bundled version. |
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

RC2FX mixer software has its own releases. The app checks signed update metadata
and offers only payloads compatible with its installer format, control protocol,
and the qualified RØDE firmware for your mixer. A new mixer release can therefore
be installed without updating the desktop app. Releases that require a newer app
cannot be selected by an older one.

Normal builds download the selected mixer payload from GitHub. Builds made with
`--features embedded` also include a bundled payload; the installer offers it as
an explicit choice. It is never substituted automatically after a network or
verification failure. If no authenticated release information is available, the
available version stays **Unknown** rather than claiming that you are current.

The newest supported RØDE firmware comes from the signed firmware catalog on
GitHub, not a list compiled into normal app builds. It is the newest qualified
version compatible with your app and mixer, not necessarily the newest one RØDE
has published. Both mixer releases and individual firmware profiles declare a
minimum app version. If yours is too old, the app shows the required version and
asks you to update the app first; that combination cannot be installed.

The installer flashes RC2FX and RØDE firmware together as one image. Review both
versions in its confirmation dialog. Older versions require an explicit downgrade
confirmation; an unknown installed version is identified before proceeding.

Nothing is flashed automatically. Open the installer from the Versions dialog
or **Settings › Mixer › Open installer**, choose the versions, and confirm.
Downloading a new desktop app does not change the mixer. See
[Installing RC2FX](installing.md).

### App updates

The app checks GitHub for a newer RC2FX app once when it starts, and again
when you press **Check for updates** in the Versions dialog. It asks
GitHub's public releases API for the official repository
(`github.com/ibanks42/rc2fx`), without signing in, and considers only
published app releases: drafts, pre-releases and mixer software releases are
ignored. A separate signed check discovers compatible mixer releases. The RC2FX
update feed supplies release notes, not permission to install a mixer payload. The check at startup stays quiet if it fails,
for example when you're offline.

When a newer app is out, the app shows its version and the notes of every app
version since yours, newest first. The full histories are at
[App release history](https://ibanks42.github.io/rc2fx/updates/app) and
[Mixer software history](https://ibanks42.github.io/rc2fx/updates/mixer). If
the feed can't be loaded, the app falls back to GitHub's release notes and
says that some notes may be missing. The desktop app does not update itself: download a new desktop version from
the release page. Mixer payloads are downloaded through the installer.
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
