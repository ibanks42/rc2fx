# Commands

Every task is a cargo command under `xtask/` and works the same on Windows,
Linux and macOS. `just` wraps the common ones. `just --list` shows the
recipes and `cargo xtask help` lists the underlying commands.

## Build and test

| Command | Description |
| --- | --- |
| `just setup` | One-time setup: the mixer's Rust target, cargo-zigbuild, a zig check |
| `just app` / `just desktop` | Run / build the desktop app. Cross-builds the mixer runtime it embeds first. |
| `just mixer-build` | Cross-build the mixer runtime into `target/mixer` |
| `just test` | Workspace tests, then Clippy with warnings as errors |
| `just test-mixer` | Native library tests, plus the launcher, recorder and screen-module tests (Linux or macOS) |
| `just package` | Build a release package for this computer into `target/package` |
| `just book` / `just book-serve` | Build / serve this book |
| `cargo xtask check-elf FILE [--shared]` | Check a mixer binary against the mixer's ABI (glibc 2.35, GCC 11) |

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
