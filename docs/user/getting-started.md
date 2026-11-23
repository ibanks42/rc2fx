# Getting started

## Download the app

Grab the app from the [latest release](https://github.com/ibanks42/rc2fx/releases/latest).
Pick the download that matches your operating system:

| System | Download |
| --- | --- |
| Windows (x64) | `rc2fx-windows-x64-setup.exe` |
| macOS 13 or later (Apple silicon and Intel) | `rc2fx-macos-universal.pkg` |
| Linux (x64) | `rc2fx-linux-x64.AppImage` |

Because the filenames don't include a version number, a link like
`https://github.com/ibanks42/rc2fx/releases/latest/download/rc2fx-linux-x64.AppImage`
always points to the newest release. The macOS download is universal and runs
natively on both Apple silicon and Intel Macs.

The mixer installer is built right into the app and fetches official RØDE
firmware when needed. Updating the desktop app is completely separate from
updating the mixer, so the app will never flash your mixer on its own.

To get the app running:

- **Windows:** Run `rc2fx-windows-x64-setup.exe`. It installs for your user
  account without needing an administrator prompt (into
  `%LOCALAPPDATA%\io.github.ibanks42.rc2fx`), and adds RC2FX to the Start menu
  and **Settings › Apps › Installed apps**.
- **macOS:** Open the `.pkg` and follow the prompts. It installs `RC2FX.app`
  into **Applications**, or `~/Applications` if you choose **Install for me
  only**.
- **Linux:** Make the AppImage executable (`chmod +x rc2fx-linux-x64.AppImage`)
  and run it. Keep it in a folder you can write to (such as your home folder)
  so it can update itself later. Linux also needs a quick one-time setup,
  covered [below](#linux).

If there isn't a prebuilt download for your system, you can
[build from source](../developer/building.md).

### Removing the app

Uninstalling the desktop app leaves your mixer untouched. If you want to remove
RC2FX from the mixer itself, flash stock RØDE firmware first; see
[Installing RC2FX](installing.md#going-back-to-stock).

- **Windows:** Uninstall RC2FX from **Settings › Apps › Installed apps**. It
  will ask whether to delete your local RC2FX data (downloaded firmware,
  settings backups, and preferences in `%LOCALAPPDATA%\RC2FX`, plus the SSH key
  pair `~/.ssh/rc2fx_rodecaster_pro_ii` and
  `~/.ssh/rc2fx_rodecaster_pro_ii.pub`). **No** is the default in case you
  reinstall later, and if the prompt isn't answered within a minute it keeps
  your files.
- **macOS:** Drag `RC2FX.app` from **Applications** (or `~/Applications`) to
  the Trash. Your local data in `~/Library/Application Support/RC2FX` and the
  SSH key pair `~/.ssh/rc2fx_rodecaster_pro_ii` and
  `~/.ssh/rc2fx_rodecaster_pro_ii.pub` remain unless you delete them too.
- **Linux:** Delete the AppImage file. You can also remove its update cache in
  `~/.cache/rc2fx/velopack-packages` (or
  `$XDG_CACHE_HOME/rc2fx/velopack-packages`). Your downloads and backups in
  `~/.local/share/RC2FX` (`$XDG_DATA_HOME/RC2FX`), preferences in
  `~/.config/RC2FX` (`$XDG_CONFIG_HOME/RC2FX`), and SSH key pair
  `~/.ssh/rc2fx_rodecaster_pro_ii` and `~/.ssh/rc2fx_rodecaster_pro_ii.pub`
  stay on disk unless you remove them.

## Versions and updates

The desktop app, the RC2FX mixer software, and RØDE's firmware are versioned
independently. Open **Settings › Versions** to see what you're running and
what's available:

| Line | Installed | Available |
| --- | --- | --- |
| **App** | Your current desktop app version. | The newest published desktop app. |
| **Mixer software** | The RC2FX software running on your mixer. | The newest compatible signed mixer release. |
| **RØDE firmware** | The RØDE firmware on your mixer. | The newest firmware RC2FX supports for your mixer, or RØDE's newest stable release if RC2FX doesn't support it yet. |

Each row ends with a status:

- **Too old:** The mixer software doesn't speak this app's control protocol and
  needs an update.
- **Blocked:** An update is available, but this app can't install it safely.
  The reason is shown right below the table.
- **Newer from RØDE:** RØDE has published a newer firmware that RC2FX doesn't
  support yet, so the app can't install RC2FX on top of it yet.
- **Unknown:** The app couldn't read the version (for example, if the mixer is
  running very old RC2FX software, or if you're offline and can't check for
  releases). Mixer versions clear when you disconnect and refresh when you
  reconnect.

Whenever an update is available, click **Read changes** to see release notes
for every version between yours and the latest, newest first. For RØDE
firmware, this links out to RØDE's own release notes.

Because RC2FX mixer software is released separately, you can often update your
mixer without updating the desktop app. The app checks signed release metadata
from GitHub and only shows mixer versions and RØDE firmware profiles that are
compatible with your app and mixer. If a newer mixer release or firmware
requires a newer desktop app, the app tells you which version you need and
asks you to update the app first. If a new mixer release was just published and
its signed metadata isn't ready yet, **Settings › Versions** lets you know so
you can check again later.

Nothing is ever updated or flashed automatically. When an update is available
for your mixer, updates start only from **Settings › Versions**, where the
button is labeled for what changes:

- **Update mixer software** when the connected mixer accepts in-place updates
  and a newer compatible signed mixer release is available. This opens the
  in-place update directly, with no full flash needed: the app downloads and
  verifies the release, copies it through a temporary USB drive named **RC2FX**
  that the mixer attaches to RØDE's removable drive (falling back to USB 1 if
  that isn't possible), and lets the mixer check it and restart RC2FX. Audio
  goes silent for a few seconds during the restart, and it can stutter while
  transferring if the update falls back to USB 1. The in-place page also offers
  **Reinstall firmware as well…** to switch to a full flash.
- **Update RØDE firmware** when only RØDE firmware changes (full flash).
- **Update RC2FX** otherwise (full flash).

For a full flash, review the versions and confirm; choosing an older version
asks for an explicit downgrade confirmation. See
[Installing RC2FX](installing.md) for the full walkthrough.

### App updates

The app checks GitHub once at startup and whenever you click **Check for
updates** in **Settings › Versions**. It only looks at published, stable app
releases (drafts and pre-releases are ignored), and if you're offline at
startup the check fails quietly.

When a new version is out, you can read the full changelog right in the app,
or browse the full histories online at
[App release history](https://rc2fx.ibanks.dev/updates/app) and
[Mixer software history](https://rc2fx.ibanks.dev/updates/mixer).

The check only notifies you; nothing downloads or installs until you start it:

- **Installed app or AppImage:** Click **Update app** in
  **Settings › Versions**. RC2FX downloads the package, verifies its checksum
  and cryptographic signature, and then restarts on its own to install the new
  version and relaunch. Your mixer's audio isn't affected while the app updates
  or restarts. On Linux, this replaces your AppImage file in place under the
  same name.

  If the check fails or your connection drops, the download is discarded and
  you can click **Update app** to try again. (If RC2FX says the update hasn't
  been signed yet, wait a bit and try again; releases are signed shortly after
  publishing.)

  If something is holding the restart back when the download finishes (an
  active installer task, the mixer finishing a flash, unsaved edits, or a
  change still reaching the mixer), RC2FX waits until it clears and shows
  **Restart to update** in the meantime. **Restart to update** stays disabled
  while the installer is busy so an update never interrupts a flash, and checks
  for unsaved edits if you click it.

  If you close the app while a verified update is waiting, RC2FX re-verifies
  the downloaded package the next time you launch and offers **Restart to
  update** instead of installing it silently. If you're offline and it can't
  verify, it keeps the download and shows **Check again** so you can retry
  later without redownloading.
- **Builds from source:** Click **Download** to open the GitHub release page
  and update manually.

On macOS, if you installed RC2FX for all users in `/Applications`, macOS will
ask for an administrator password to replace the app. If you cancel that
prompt, the current version simply starts back up.

## Linux

RC2FX for Linux ships as an AppImage for x86-64 distributions with glibc 2.28
or newer (Debian 10+, Ubuntu 20.04+, RHEL/AlmaLinux/Rocky 8+, Fedora 29+, and
derivatives).

If you previously installed the `.deb` or `.rpm` from RC2FX 0.1.5, remove it
first with `sudo apt remove rc2fx` or `sudo dnf remove rc2fx` and switch to the
AppImage.

### Device access

Linux restricts access to the mixer's USB control interface until a udev rule
allows it. The first time access is denied, the app explains what's needed,
lets you inspect `/etc/udev/rules.d/70-rc2fx.rules`, and offers **Allow
access** to install it via your system password prompt. Your password goes
straight to the system prompt; the app never sees it.

If you'd rather install the rule manually, save
[`config/udev/70-rc2fx.rules`](../../config/udev/70-rc2fx.rules) from the
repository and run:

```bash
sudo cp 70-rc2fx.rules /etc/udev/rules.d/
sudo udevadm control --reload-rules
sudo udevadm trigger --action=add --subsystem-match=hidraw
```

The rule only matches RØDECaster mixers and grants access to the logged-in
desktop user. It takes effect right away without unplugging the mixer. Running
RC2FX as root isn't supported.

### Administrator access for installing

Putting the mixer into update mode and flashing firmware require administrator
privileges. Click **Grant access** in the installer and enter your password
once; the app itself stays running as your normal user. On minimal desktop
environments or window managers, make sure a polkit authentication agent is
running so the password prompt can appear.

### Graphics

RC2FX requires a graphical desktop with working Vulkan drivers.

## Connect the mixer

Plug your computer into the mixer's **USB 1** port. USB 2 doesn't carry the
control interface. The app detects the mixer automatically and reconnects if
you unplug the cable or reboot the mixer. It doesn't use Wi-Fi or Ethernet.

What happens next depends on your mixer:

- **RC2FX is already installed:** The app opens straight to the Overview. Head
  to [Using the app](using-the-app.md).
- **Stock RØDE firmware:** The app lets you know RC2FX isn't installed and
  offers **Install RC2FX**. Continue with [Installing RC2FX](installing.md).

RC2FX shares USB 1 cleanly with RØDE's own software. The RØDECaster App and
RØDE's Windows virtual audio devices continue working normally alongside RC2FX.
