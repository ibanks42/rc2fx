# Developer guide

These pages cover building RC2FX, running development builds on a mixer, and
the layout of the code. Using RC2FX doesn't require any of it.

## Overview

RC2FX has a mixer side and a desktop side.

The mixer side is three programs:

- `librc2fx_native.so`, a library that RØDE's audio engine loads at start-up
  and that runs the effects.
- `rc2fxd`, a controller that stores the settings and answers the desktop
  app.
- `xtask/mixer/rc2fx.sh`, a shell script that starts, supervises and stops
  the other two.

The library and controller are Rust, cross-compiled for the mixer's ARM
processor with zig.

The desktop side is `rc2fx-app`, a GPUI app that edits settings over USB 1.
It also contains the installer, which builds and flashes RC2FX firmware.

[Architecture](architecture.md) describes how the pieces fit together.

## Repository layout

| Path | Contents |
| --- | --- |
| `crates/rc2fx-app` | Desktop app and its installer page |
| `crates/rc2fx-transport` | The app's USB 1 control client |
| `crates/rc2fx-usb` | USB control framing shared by the app and `rc2fxd` |
| `crates/rc2fx-installer` | Installer engine: firmware download and patching, USB update mode, the USB control client |
| `crates/rc2fx-core` | Settings model, schema upgrades, EQ maths, EasyEffects and APO import/export |
| `crates/rc2fx-dsp` | Effect chain |
| `crates/rc2fx-suppression` | Noise-suppression worker and models |
| `crates/rc2fx-dfn` | RC2FX's DeepFilterNet3 engine and weights |
| `crates/rc2fx-native` | `librc2fx_native.so`, the library loaded into the mixer |
| `crates/rc2fx-native-control` | Protocol between that library and `rc2fxd` |
| `crates/rc2fx-device` | `rc2fxd`: controller, control API, settings store |
| `crates/rc2fx-firmware` | Firmware catalog validation and the offline compatibility checker |
| `crates/rc2fx-bench` | Benchmarks the suppression models on the mixer |
| `crates/rc2fx-gui` | `librc2fx_gui.so` (C), loaded into RØDE's screen program. It converts only the changed parts of the screen, which reduces the load redraws put on audio. |
| `xtask` | Build, deploy and packaging tasks. `xtask/mixer` holds the files that run on the mixer. |
| `config/firmware-profiles.json` | [Firmware catalog](../reference/firmware.md) |
| `tools/firmware_audit` | Audits a new RØDE firmware against the hooks RC2FX depends on |
| `experiments/` | Unfinished work outside the build (currently the on-screen panel) |
| `docs/` | This documentation |

`rc2fx-native` and `rc2fx-bench` are separate Cargo workspaces. They build
only for Linux and are cross-compiled for the mixer.

## Pages in this guide

- [Building from source](building.md): tools, builds and tests on Windows,
  Linux or macOS.
- [Linux setup](linux.md): package lists for Debian/Ubuntu, Fedora and Arch.
- [Working on a mixer](mixer-workflow.md): enabling SSH from the app and
  running a build on a mixer over SSH without reflashing.
- [Commands](commands.md): all `just` recipes.
- [Architecture](architecture.md): start-up, fallback to stock, and how
  settings are applied.
- [Releases](releases.md): cutting a release.

The [technical reference](../reference/index.md) covers protocols, file
formats, firmware support and per-crate notes.

## Rules for changes

This code runs inside someone's audio hardware, so a few rules apply
everywhere:

- RC2FX only adds files to the mixer. Leave RØDE's files as they are, and put
  mixer-side behaviour in `xtask/mixer/rc2fx.sh`.
- The audio callback must not lock, allocate or wait. Move slow work to
  another thread.
- When a check fails or something is uncertain, fall back to stock and let
  RØDE's code run unchanged.
- Firmware support is pinned to file hashes. See
  [Firmware support](../reference/firmware.md).
- Leave a mixer alone during its first boot after an update, and restart
  audio only when the person using it expects it.

## Building the documentation

```sh
just docs         # build into target/docs
just docs-serve   # serve on http://localhost:5173/rc2fx/ and rebuild on change
```

Both need [Node.js](https://nodejs.org/). The site is built with
[VitePress](https://vitepress.dev/), configured in `docs/.vitepress/config.mjs`,
which also holds the sidebar. It reads `docs/` in place, and each crate page
includes that crate's `README.md`. Links to files outside `docs/` open on
GitHub. Pushing a change under `docs/` to `main` publishes the site to GitHub
Pages. User pages are in
`docs/user/`, developer pages in `docs/developer/` and the technical
reference in `docs/reference/`.
