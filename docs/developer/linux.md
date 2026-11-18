# Linux setup

These are the packages needed to build and run the desktop app on
Debian/Ubuntu, Fedora, or Arch. The app is Go with a C compiler for cgo; its UI
library, gpuikit, comes prebuilt ([Building from source](building.md)) and is
linked into the app; it needs the libraries below to link and run. Audio processing happens on the
mixer, so no audio setup is needed on the computer.

The project is developed on Fedora. The Debian/Ubuntu and Arch lists haven't
been tested end to end, and package names change between releases.

## System packages

### Debian / Ubuntu

```bash
sudo apt update
sudo apt install git curl build-essential golang-go \
  libxkbcommon0 libxkbcommon-x11-0 libxcb1 libfreetype6 libwayland-client0 \
  libvulkan1 mesa-vulkan-drivers policykit-1
```

On some releases the polkit package is called `pkexec` instead of
`policykit-1`. Install `just` and `zig` from your distribution if it packages
them, or from upstream.

### Fedora

```bash
sudo dnf install git curl gcc golang \
  libxkbcommon libxkbcommon-x11 libxcb freetype libwayland-client \
  vulkan-loader mesa-vulkan-drivers polkit just zig
```

### Arch

```bash
sudo pacman -S --needed git curl base-devel go \
  libxkbcommon libxkbcommon-x11 libxcb freetype2 wayland \
  vulkan-icd-loader polkit just zig
```

The Go build needs the version `desktop/go.mod` names. If your distribution's
Go is older, install it from [go.dev](https://go.dev/dl/) instead.

Also install the Vulkan driver for your GPU (`vulkan-radeon`, `vulkan-intel`,
or NVIDIA's packages).

## Notes

- The app's UI (gpuikit, with GPUI underneath) needs a graphical session and
  a working Vulkan driver.
- The installer gets root through `pkexec`. On minimal desktops, make sure a
  polkit authentication agent is running.
- Fedora's zig 0.16 with cargo-zigbuild 0.24 is known to work for the mixer
  build. Other version combinations probably work but haven't all been tested.

Continue with [Building from source](building.md).

## USB permission

The app needs read/write access to the mixer's `hidraw` device. The rule in
the repository grants it to the user logged in at the desktop, for the Pro II
and Duo in their normal USB modes:

```bash
sudo cp config/udev/70-rc2fx.rules /etc/udev/rules.d/
sudo udevadm control --reload-rules
sudo udevadm trigger --action=add --subsystem-match=hidraw
```

The rule does not cover USB update mode. In that mode only the installer's
root helper talks to the mixer.

Access comes from the desktop seat (logind), so an SSH-only session may not
get it. If the app reports `USB permission required: Permission denied`,
check that the rule is installed and that you are logged in at the machine.
The app retries on its own, so nothing needs restarting. Don't run the app as
root.
