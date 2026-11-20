# Developer guide

These pages cover building RC2FX from source, running development builds on a
mixer, and navigating the codebase. You don't need any of this just to use
RC2FX.

## Overview

RC2FX is split into a mixer side and a desktop side.

The mixer side consists of three components:

- `librc2fx_native.so`, a shared library loaded by RØDE's audio engine at
  startup to run the DSP effects.
- `rc2fxd`, a controller daemon that persists settings and handles requests
  from the desktop app.
- `mixer/scripts/rc2fx.sh`, a shell script that starts and stops the other two,
  watched by `rc2fx-supervisor` while they run.

The library and controller are written in Rust and cross-compiled with zig for
the mixer's ARM processor.

The desktop side is a Go app under `desktop/`, built on GPUI-kit via the
gpuikit Go bindings. It edits settings over USB 1 and includes the built-in
installer that patches and flashes RC2FX firmware. See
[The desktop app in Go](desktop-go.md) for its package structure and the rules
that keep it aligned with the mixer's Rust code.

See [Architecture](architecture.md) for how all the pieces fit together.

## Repository layout

| Path | Contents |
| --- | --- |
| `desktop/` | The desktop app, a Go module (`github.com/ibanks42/rc2fx/desktop`). See [The desktop app in Go](desktop-go.md). |
| `desktop/app`, `desktop/cmd/rc2fx` | Desktop app and its installer page |
| `desktop/transport` | The app's USB 1 control client |
| `desktop/installer` | Installer engine: firmware download and patching, USB update mode, and the USB control client. `desktop/ext2fs` holds the vendored libext2fs used by Repair mixer. |
| `desktop/packaging` | AppImage launcher, desktop entry, icons, and macOS entitlements |
| `crates/rc2fx-usb` | USB control framing for `rc2fxd`; the app's counterpart is `desktop/usbproto` |
| `crates/rc2fx-core` | Settings model, control API messages, and EQ coefficients; the app's counterpart, `desktop/core`, also holds editor logic and EasyEffects / APO import and export |
| `crates/rc2fx-delta` | Delta transport for in-place payload uploads: the mixer's decoder plus a test-only encoder behind the `encode` feature; the app's encoder is `desktop/delta` |
| `crates/rc2fx-dsp` | Effect chain |
| `crates/rc2fx-suppression` | Noise-suppression worker and models |
| `crates/rc2fx-dfn` | RC2FX's DeepFilterNet3 engine and weights |
| `crates/rc2fx-native` | `librc2fx_native.so`, the library loaded into the mixer |
| `crates/rc2fx-native-control` | Protocol between `librc2fx_native.so` and `rc2fxd` |
| `crates/rc2fx-device` | `rc2fxd`: controller, control API, and settings store |
| `crates/rc2fx-supervisor` | `rc2fx-supervisor`: monitors the audio engine and `rc2fxd`, handing audio back to stock if either fails |
| `crates/rc2fx-firmware` | Firmware catalog validation, mixer-side profile selection, and offline compatibility checks; the app's counterpart, `desktop/firmware`, reads the catalog and inspects images |
| `crates/rc2fx-bench` | Benchmarks suppression models directly on the mixer |
| `mixer/gui` | `librc2fx_gui.so` (C), loaded into RØDE's screen program to convert only changed screen regions and reduce audio interference from redraws |
| `mixer/jackerr` | `librc2fx_jackerr.so` and `rc2fx_jackerr_snapshot` (C), the `jackd` error recorder and snapshot reader |
| `xtask`, `desktop/cmd/xtask` | Build, deploy, and packaging tasks. `./xtask` at the root invokes the Go tool in `desktop/cmd/xtask` and `desktop/internal/xtask`. |
| `mixer/scripts` | Launcher, shims, and installer scripts that run on the mixer, with tests in `mixer/scripts/tests` |
| `fixtures/` | Shared accept/reject test cases read by both Rust and Go tests to keep both implementations in sync |
| `config/firmware-profiles.json` | [Firmware catalog](../reference/firmware.md) |
| `tools/firmware_audit` | Audits a new RØDE firmware against the hooks RC2FX relies on |
| `tools/src_fast_check` | Compares the library's sample-rate converter (`src_fast_core.rs`) against real libsamplerate on AArch64 or under qemu |
| `tools/xrun_trace` | Xrun flight recorder: bench runs and kernel scheduler traces on a mixer |
| `experiments/` | Unfinished work outside the main build (currently the on-screen panel) |
| `docs/` | This documentation |

`rc2fx-native` and `rc2fx-bench` are separate Cargo workspaces cross-compiled
for the mixer. All Rust crates target Linux only. Only the mixer side uses
Rust; the desktop app lives in the Go module under `desktop/`.

## Pages in this guide

- [Building from source](building.md): required tools, builds, and tests on
  Windows, Linux, and macOS (Rust tests run on Linux only).
- [Linux setup](linux.md): system packages for Debian/Ubuntu, Fedora, and Arch.
- [Working on a mixer](mixer-workflow.md): enabling SSH from the app and
  deploying builds to a mixer over SSH without reflashing.
- [Commands](commands.md): reference for all `just` recipes and `./xtask`
  commands.
- [Architecture](architecture.md): startup flow, fallback to stock, and how
  settings are applied.
- [The desktop app in Go](desktop-go.md): Go packages, naming conventions,
  strict JSON handling, and fixtures shared with Rust.
- [Releases](releases.md): how releases are cut and signed.
- [Signed mixer releases](mixer-releases.md): standalone mixer releases,
  compatibility declarations, and in-place updates.
- [Update histories and the feed](update-history.md): generated app and mixer
  histories and `updates.json`.

The [technical reference](../reference/index.md) covers protocols, file
formats, firmware support, and per-crate notes.

## Rules for changes

Because this code runs inside someone's audio hardware, a few strict rules
apply everywhere:

- RC2FX only adds files to the mixer. Never modify RØDE's files; put mixer-side
  behavior in `mixer/scripts/rc2fx.sh`.
- The audio callback must never lock, allocate, or wait. Push slow work onto
  another thread.
- Whenever a check fails or state is uncertain, fall back to stock and let
  RØDE's code run untouched.
- Firmware support is pinned to file hashes. See
  [Firmware support](../reference/firmware.md).
- Leave the mixer alone during its first boot after an update, and only restart
  audio when the user expects it.

## Building the documentation

```sh
just docs         # build into target/docs
just docs-serve   # serve on http://localhost:5173/rc2fx/ and rebuild on change
```

Both commands require [Node.js](https://nodejs.org/). The site uses
[VitePress](https://vitepress.dev/), configured in `docs/.vitepress/config.mjs`
(which also defines the sidebar). It reads `docs/` in place, and each crate or
package page includes that crate's or package's `README.md`. Links pointing
outside `docs/` open on GitHub. The **Docs** workflow builds and deploys the
site after a release is published (from that release's tag) or when triggered
manually from `main`; normal pushes don't deploy it. Update histories and
`updates.json` are generated during the build (see
[Update histories and the feed](update-history.md)). User docs live in
`docs/user/`, developer docs in `docs/developer/`, and technical reference
pages in `docs/reference/`.
