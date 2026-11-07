# Commands

Every task is a cargo command under `xtask/` and works the same on Windows,
Linux and macOS. `just` wraps the common ones. `just --list` shows the
recipes and `cargo xtask help` lists the underlying commands.

## Build and test

| Command | Description |
| --- | --- |
| `just setup` | One-time setup: the mixer's Rust target, cargo-zigbuild, a zig check |
| `just app` / `just desktop` | Run / build the desktop app with signed mixer downloads. No mixer cross-build is needed. |
| `just app-mock` | Run the desktop app against a simulated mixer, with no hardware. See [below](#without-a-mixer). |
| `just app-embedded` / `just desktop-embedded` | Cross-build the mixer runtime, then run / build an app with the optional embedded payload. |
| `just mixer-build` | Cross-build the mixer runtime into `target/mixer` |
| `just test` | Workspace tests, then Clippy with warnings as errors |
| `just test-mixer` | Native library tests, plus the launcher, recorder and screen-module tests (Linux or macOS) |
| `just package` | Build a release package for this computer into `target/package` |
| `just package-check` | Build the Linux packages in a container from the working tree, then test installing them (x86-64 Linux with Podman or Docker) |
| `just docs` / `just docs-serve` | Build / serve this documentation site |
| `cargo xtask check-elf FILE [--shared]` | Check a mixer binary against the mixer's ABI (glibc 2.35, GCC 11) |

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

`just app-mock` runs a development build of the app with the `mock-mixer`
feature, which swaps USB 1 for a simulated mixer inside the app process. The
app, its validation and its `api/v1` requests are the real ones.

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
system.

A release build with this feature does not compile. `RC2FX_DEV_PAGE` works with
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
| `just bench recording.wav [FLAGS]` | Benchmark every suppression model on the mixer over a 48 kHz recording and download the denoised audio. See `cargo xtask help` for flags. |

## Over USB 1

| Command | Description |
| --- | --- |
| `cargo xtask mixer api METHOD PATH [BODY\|@file]` | Send one [controller API](../reference/api.md) request |
| `cargo run --release -p rc2fx-transport --example usb_probe` | Read-only probe. `--extended` also reads inputs, presets and diagnostics. |
