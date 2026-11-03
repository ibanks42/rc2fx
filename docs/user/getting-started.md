# Getting started

## Download the app

There is one download per operating system: `rc2fx-windows-x64.zip`,
`rc2fx-macos-arm64.zip` or `rc2fx-linux-x64.tar.gz`. Extract the whole
archive. The installer is built into the app, and it downloads official RØDE
firmware when needed.

To start the app:

- Windows: run `rc2fx.exe`.
- macOS: run `rc2fx`. The build is unsigned, so macOS may ask you to allow it
  under **System Settings › Privacy & Security**. The macOS build has not been
  tested on a real Mac yet.
- Linux: run `rc2fx`. Linux needs some one-time setup, described
  [below](#linux).

If there is no download for your platform, you can
[build it from source](../developer/building.md).

## Linux

### Device access

Linux only lets a program open the mixer's control interface if a udev rule
allows it. The first time access is refused, the app explains what it needs:
the file `/etc/udev/rules.d/70-rc2fx.rules`, which it can show in full, and
how to remove it later. **Allow access** installs the rule through your
system's password prompt. The password goes to the system prompt, and the app
does not see it.

To install the rule by hand, run this from the extracted `rc2fx` folder:

```bash
sudo cp config/udev/70-rc2fx.rules /etc/udev/rules.d/
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
