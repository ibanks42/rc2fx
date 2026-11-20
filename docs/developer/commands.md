# Commands

Every task is an `./xtask` command (the Go tool in `desktop/cmd/xtask`) and works the same on Windows,
Linux, and macOS. `just` wraps the common ones. `just --list` shows the
recipes and `./xtask help` lists the underlying commands.

## Build and test

| Command | Description |
| --- | --- |
| `just setup` | One-time setup: the mixer's Rust target, cargo-zigbuild, a zig check, and a pre-commit hook that refuses unformatted Rust or Go code |
| `just app` / `just desktop` | Run a development build / build a release build of the desktop app with signed mixer downloads. No mixer cross-build is needed. Both need the gpuikit download in `desktop/third_party/gpuikit` ([Building](building.md)). |
| `just app-release` | Run the optimized release build, as shipped. Slower to build; use it to check a change before a release. |
| `just app-mock` | Run the desktop app against a simulated mixer, with no hardware. See [below](#without-a-mixer). |
| `just app-embedded` / `just desktop-embedded` | Cross-build the mixer runtime, then run a development build / build a release build of the app with the optional embedded payload. |
| `just app-embedded-release` | Cross-build the mixer runtime, then run the release build with the optional embedded payload. |
| `just mixer-build` | Cross-build the mixer runtime into `target/mixer` |
| `just test` | Go formatting, `go vet`, and tests for the desktop app (the transport also with the `mockmixer` tag), then, on Linux, `just test-rust` |
| `just test-rust` | The Rust workspace tests and Clippy with warnings as errors, also for the `eq-fma` and `bench-kernels` features (Linux only, like every Rust crate here) |
| `just test-mixer` | Native library tests and Clippy, a benchmark build check, plus the launcher, recorder, screen-module, and xrun flight recorder tests (Linux) |
| `just package` | Build a release package for this computer into `target/package` |
| `just package-check` | Build the AppImage from the working tree in the release container (AlmaLinux 8, glibc 2.28), then check it in clean containers of several distributions (x86-64 Linux with Podman or Docker) |
| `just docs` / `just docs-serve` | Build / serve this documentation site |
| `./xtask app [--release] [--mock] [--embedded] [--build-only]` | What the app recipes run: build the desktop app for this computer into `target/debug`, `target/release`, or `target/mock`, and start it unless `--build-only` |
| `./xtask check-elf FILE [--shared]` | Check a mixer binary against the mixer's ABI (glibc 2.35, GCC 11) |

The docs build and development server generate update history first. They need
network access to GitHub's releases API and a checkout containing the release
tags; `GH_TOKEN` can authenticate the requests. These commands do not deploy
the site.

For offline editing, with npm dependencies already installed, generate history
from a saved releases API list and start VitePress directly, bypassing the
networked `predev` hook:

```sh
python3 .github/scripts/update_history.py --published-json /path/to/releases.json
cd docs
npm exec -- vitepress dev
```

## Without a mixer

`just app-mock` runs a development build of the app with the `mockmixer`
build tag, which swaps USB 1 for a simulated mixer inside the app process. The
app, its validation, and its `api/v1` requests are the real ones.

- It starts from the same demo setup every time. Settings and presets last until
  the app quits, and Disconnect keeps them.
- Meters are generated, and move with the controls, but no audio exists. The
  channel editor labels its live readings "Simulated", and a simulated xrun
  appears in the diagnostics events about every 47 seconds.
- `RC2FX_MOCK_SILENT=1` silences every channel, to see what the app costs with
  nothing to show.
- It never opens USB, so a real mixer plugged in is left alone. The installer is
  off, because it finds mixers on USB by itself, and SSH shows as unsupported.

`just desktop-mock` builds the same thing without debug information, as
`target/mock/rc2fx`, for copying to another computer with the same operating
system. gpuikit is linked into it, so it needs nothing beside it.

`./xtask app` refuses `--mock` with `--release`, and packaging refuses an
executable built with the `mockmixer` or `rc2fxdev` tag. `RC2FX_DEV_PAGE` works with
it, for example `RC2FX_DEV_PAGE=channel:mic\ 4:eq just app-mock`.

## On the mixer over SSH

These need the [SSH setup](mixer-workflow.md). Starting or stopping RC2FX
restarts the mixer's audio engine.

| Command | Description |
| --- | --- |
| `just mixer-run` | Build and run on the mixer from RAM. A reboot returns to the installed version, or to stock. |
| `just mixer-install` | Build and install to `/opt/rc2fx`, starting at every boot |
| `just mixer-stop` | Stop RC2FX and hand audio back to stock |
| `just mixer-status` / `just mixer-logs` | Show what is running / print recent logs |
| `just mixer-xruns [SECONDS]` | Show what the JACK error recorder logs over SECONDS (default 60; `0` shows everything since JACK started) |
| `just mixer-disable` / `just mixer-enable` | Skip / allow starting at boot |
| `just mixer-uninstall [--purge]` | Remove RC2FX. `--purge` also deletes its settings. |
| `just bench recording.wav [FLAGS]` | Benchmark every suppression model on the mixer over a 48 kHz recording and download the denoised audio. See `./xtask help` for flags. |

## Over USB 1

| Command | Description |
| --- | --- |
| `./xtask mixer api METHOD PATH [BODY\|@file]` | Send one [controller API](../reference/api.md) request |
| `go -C desktop run ./cmd/usb-probe` | Read-only probe. `--extended` also reads inputs, presets, and diagnostics. |
