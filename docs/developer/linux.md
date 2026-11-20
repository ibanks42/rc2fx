# Linux setup

Here are the system packages you need to build and run the desktop app on
Debian/Ubuntu, Fedora, or Arch. The app is written in Go with a C compiler for
cgo; its UI library, gpuikit, comes prebuilt (see
[Building from source](building.md)) and links straight into the binary, which
pulls in the graphics and windowing libraries below. All audio DSP runs on the
mixer itself, so you don't need any audio dev packages on your PC.

Development happens on Fedora. The Debian/Ubuntu and Arch lists haven't been
tested end-to-end, and package names drift across releases.

## System packages

### Debian / Ubuntu

```bash
sudo apt update
sudo apt install git curl build-essential golang-go \
  libxkbcommon0 libxkbcommon-x11-0 libxcb1 libfreetype6 libwayland-client0 \
  libvulkan1 mesa-vulkan-drivers policykit-1
```

On some releases `policykit-1` is named `pkexec` instead. Install `just` and
`zig` from your distro repos if they're new enough, or grab them upstream.

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

You'll need the Go version specified in `desktop/go.mod`. If your distro ships
an older release, install Go directly from [go.dev](https://go.dev/dl/).

Make sure you also have the Vulkan driver for your GPU (`vulkan-radeon`,
`vulkan-intel`, or NVIDIA's driver packages).

## Notes

- The UI (gpuikit over GPUI) requires a graphical session and a working Vulkan
  driver.
- The installer elevates via `pkexec`. If you're on a minimal desktop or window
  manager, make sure a polkit authentication agent is running.
- Fedora's zig 0.16 with cargo-zigbuild 0.24 is known to work for the mixer
  cross-build. Other pairings probably work fine, they just haven't all been
  verified.

From here, continue with [Building from source](building.md).

## USB permission

The desktop app needs read/write access to the mixer's `hidraw` device. The udev
rule in the repo grants access to the locally logged-in desktop user for both
the Pro II and Duo in their normal USB modes:

```bash
sudo cp config/udev/70-rc2fx.rules /etc/udev/rules.d/
sudo udevadm control --reload-rules
sudo udevadm trigger --action=add --subsystem-match=hidraw
```

This rule doesn't cover USB update mode. In that mode, only the installer's
root helper talks to the mixer.

Because access is granted through the desktop seat (`logind`), an SSH-only
session might not get it. If the app says `USB permission required: Permission denied`,
double-check that the udev rule is installed and that you're logged in locally.
The app retries automatically, so you don't need to restart it. Don't run the
app as root.
