# Getting started

## Download the app

Download the app from the [latest release](https://github.com/ibanks42/rc2fx/releases/latest).
Downloads have the system and processor in their names:

| System | Installer | Portable |
| --- | --- | --- |
| Windows (x64) | `rc2fx-windows-x64-setup.exe` | `rc2fx-windows-x64.zip` |
| macOS (Apple silicon) | `rc2fx-macos-arm64.pkg` | `rc2fx-macos-arm64.zip` |
| Linux (x64) | — | `rc2fx-linux-x64.AppImage` |

The names have no version, so a link such as
`https://github.com/ibanks42/rc2fx/releases/latest/download/rc2fx-linux-x64.AppImage`
always downloads the newest one.

The release also has other files: `.nupkg` and `releases.*.json` files, which
the app updates itself from, the AppImage runtime's sources and
`SHA256SUMS`. You don't need to download them.

The installer is built into the app, and it downloads official RØDE firmware
when needed. Updating the app is separate from updating the mixer: the app
never flashes the mixer by itself.

To install and start the app:

- Windows: run the `-setup.exe`. It installs RC2FX for your user only, without
  an administrator prompt, into `%LOCALAPPDATA%\io.github.ibanks42.rc2fx`. It
  adds RC2FX to the Start menu and to **Settings › Apps › Installed apps**.
  For the portable copy, extract the zip to a folder you can write to and run
  `RC2FX.exe`.
- macOS: open the `.pkg` and follow its steps. It installs `RC2FX.app` into
  **Applications**, or into the **Applications** folder in your home folder
  (`~/Applications`) if you choose **Install for me only**. For the portable
  copy, extract the zip, move `RC2FX.app` to **Applications** and open it.
- Linux: make the downloaded AppImage executable
  (`chmod +x rc2fx-linux-x64.AppImage`) and run it. Keep it in a folder you
  can write to, such as your home folder, so that it can update itself. Linux
  needs some one-time setup, described [below](#linux).

If there is no download for your platform, you can
[build it from source](../developer/building.md).

### Removing the app

Removing the app doesn't change the mixer. To remove RC2FX from the mixer,
flash stock RØDE firmware with the installer first; see
[Installing RC2FX](installing.md).

- Windows: uninstall RC2FX from **Settings › Apps › Installed apps**. It asks
  whether to also delete RC2FX's data on this computer: downloaded firmware,
  the backups of your mixer's RC2FX settings and your preferences, in
  `%LOCALAPPDATA%\RC2FX`, and the SSH key pair
  `~/.ssh/rc2fx_rodecaster_pro_ii` and `~/.ssh/rc2fx_rodecaster_pro_ii.pub`.
  **No** is the default and keeps them, in case you install RC2FX again. An
  uninstall that nobody answers keeps them after a minute. For the portable
  copy, delete its folder.
- macOS: drag `RC2FX.app` from **Applications** (or `~/Applications`) to the
  Trash. RC2FX's data in `~/Library/Application Support/RC2FX` and the SSH
  key pair `~/.ssh/rc2fx_rodecaster_pro_ii` and
  `~/.ssh/rc2fx_rodecaster_pro_ii.pub` stay; delete them too if you don't
  want them.
- Linux: delete the AppImage. Its update downloads are in
  `~/.cache/rc2fx/velopack-packages`
  (`$XDG_CACHE_HOME/rc2fx/velopack-packages` if that is set); delete the
  folder too. RC2FX's data stays: downloads and backups in
  `~/.local/share/RC2FX` (`$XDG_DATA_HOME/RC2FX` if that is set), preferences
  in `~/.config/RC2FX` (`$XDG_CONFIG_HOME/RC2FX` if that is set), and the SSH
  key pair `~/.ssh/rc2fx_rodecaster_pro_ii` and
  `~/.ssh/rc2fx_rodecaster_pro_ii.pub`.

## Versions and updates

The app, the RC2FX mixer software and RØDE's firmware have separate versions.
The **Versions** section of **Settings** shows each part:

| Line | Installed | Available |
| --- | --- | --- |
| **App** | The desktop app's version. | The newest published app. |
| **Mixer software** | The RC2FX mixer software on the mixer now. | The newest compatible signed mixer release, or an optional bundled version. |
| **RØDE firmware** | The RØDE firmware on the mixer now. | The newest firmware this app installs for your mixer model, or RØDE's newest stable release when RC2FX doesn't support it yet. |

Each line ends with a status. **Too old** means the mixer software doesn't speak
this app's control protocol and needs updating. **Blocked** means an update
exists but this app can't install it safely, and the reason appears below the
table. **Newer from RØDE** means RØDE has released firmware that RC2FX doesn't
support yet, so the app can't install it with RC2FX. The app reads RØDE's
stable releases from RØDE's update server when it checks for updates.

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

Nothing is flashed automatically. Open the installer from **Settings ›
Versions** or **Settings › Mixer › Open installer**, choose the versions, and confirm.
Downloading a new desktop app does not change the mixer. See
[Installing RC2FX](installing.md).

### App updates

The app checks GitHub for a newer RC2FX app once when it starts, and again
when you press **Check for updates** in **Settings › Versions**. It asks
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
says that some notes may be missing.

The check only tells you about a new version. Nothing downloads or installs
until you ask. How you update depends on how you installed the app:

- Windows and macOS, installed with the `-setup.exe` or the `.pkg` or run from
  the portable zip, and the Linux AppImage: **Settings › Versions** shows
  **Download update**. It downloads the new version from the latest GitHub
  release and shows its progress. The button then reads **Checking update**
  while the download is checked twice: against the SHA-256 that the release
  lists for it, and against RC2FX's signed release information, which only
  the project's release keys can produce. The signed check runs before the
  download is put in place, so a download that fails it, or that can't be
  checked, for example because the connection dropped, is never used; press
  **Download update** to try again. If RC2FX says the update hasn't been
  signed yet, try again later: each release is signed shortly after it's
  published. Once the check passes, press **Restart to update**:
  RC2FX closes, installs the new version and starts again. The AppImage
  update replaces the AppImage file you started, under the same name.

  An update you downloaded but haven't restarted for is checked again the
  next time RC2FX starts, before **Restart to update** appears. If that
  check can't be made, for example because you're offline, RC2FX keeps the
  download, says what went wrong, and the button reads **Check again**,
  which checks it again without downloading it.
- Builds from source: **Download** opens the release page. Download and
  install the new version yourself.

**Restart to update** works like closing the window: if you have unsaved
edits, RC2FX asks about them first. It is unavailable while the installer
has a run going, including while it waits for the mixer to finish updating
after a flash, so an update never interrupts a flash. A downloaded update stays ready
until you restart for it. If it has been removed from your computer since,
RC2FX stays open and asks you to download it again. If a download fails,
**Release page** opens the release instead. On macOS, if the `.pkg`
installed RC2FX for all users (into
`/Applications`), macOS asks for an administrator's name and password to
replace the app; without them the update isn't installed and the current
version starts again.

Mixer payloads are downloaded through the installer. Updating the app doesn't
change the mixer; see above for updating the mixer software.

## Linux

RC2FX for Linux is the AppImage. It runs on x86-64 distributions with glibc
2.28 or newer, such as Debian 10, Ubuntu 20.04, RHEL, AlmaLinux and Rocky
Linux 8, Fedora 29 and later releases of each.

RC2FX 0.1.5 also came as a `.deb` and an `.rpm`. If you installed one of
them, remove it with `sudo apt remove rc2fx` or `sudo dnf remove rc2fx` and
download the AppImage instead.

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
