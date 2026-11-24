# Commands

Every task is implemented in its own Go module at `tools/xtask` so workflows
stay identical across Windows, Linux, and macOS. Run tasks from the repository
root as `go -C tools/xtask run . <command>` (this works even before goui-kit is
downloaded). From a subfolder, point `-C` at `tools/xtask` relative to where
you are, for example `go -C ../tools/xtask run . <command>` from `desktop/`.
Because xtask switches to the repository root when it starts, relative path
arguments always resolve from the repository root regardless of where you ran
the command. `just` wraps the most common ones.
Run `just --list` for the recipes or `go -C tools/xtask run . help` for the
underlying commands.

## Build and test

| Command | Description |
| --- | --- |
| `just setup` | One-time setup for building, packaging, and running the app and building the mixer: downloads the pinned goui-kit and Velopack libraries, adds the mixer's Rust target, installs `cargo-zigbuild` and `cargo-about` 0.8.4, checks for `zig`, installs the pinned `vpk` (needs the .NET SDK), installs `linuxdeploy`, `patchelf`, `mksquashfs`, and the AppImage runtime notices on Linux, and enables the pre-commit formatting hook |
| `just app` / `just desktop` | Run a development build / compile a release build of the desktop app using signed mixer downloads. No mixer cross-build needed. Both require goui-kit in `desktop/third_party/goui-kit` ([Building](building.md)). |
| `just app-release` | Run the optimized release build as shipped. Slower to compile; handy for verifying changes before a release. |
| `just app-mock` | Run the desktop app against an in-process simulated mixer with no hardware. See [below](#without-a-mixer). |
| `just app-embedded` / `just desktop-embedded` | Cross-build the mixer runtime, then run a development build / compile a release build of the app with the optional embedded payload. |
| `just app-embedded-release` | Cross-build the mixer runtime, then run the release build with the optional embedded payload. |
| `just mixer-build` | Cross-build the mixer runtime into `target/mixer` |
| `just test` | Check Go formatting and run `go vet` and tests for the desktop app (including `transport` with the `mockmixer` tag) and `tools/xtask`, then on Linux run `just test-rust` |
| `just test-rust` | Run Rust workspace tests and Clippy with warnings as errors, including the `bench-kernels` feature (Linux only, matching all Rust crates here) |
| `just test-mixer` | Run native library tests and Clippy, a benchmark build check, plus launcher, recorder, and screen-module tests (Linux) |
| `just package` | Build a release package for the current OS into `target/package` |
| `just package-check` | Build the AppImage from your working tree inside the release container (AlmaLinux 8, glibc 2.28), then verify it in clean containers across several Linux distributions (x86-64 Linux with Podman or Docker) |
| `just docs` / `just docs-serve` | Build / serve this documentation site |
| `go -C tools/xtask run . app [--release] [--mock] [--embedded] [--build-only]` | Underlying command for the app recipes: builds into `target/debug`, `target/release`, or `target/mock`, and launches it unless `--build-only` is passed |
| `go -C tools/xtask run . test-linux-packages [DIR] [--artifacts DIR] [--version VERSION] [--runtime auto\|podman\|docker]` | Verify built Linux release artifacts and inspect the AppImage inside throwaway distribution containers |
| `go -C tools/xtask run . check-elf FILE [--shared]` | Verify a mixer binary against the mixer's ABI (glibc 2.35, GCC 11) |

Both `just docs` and `just docs-serve` generate the update history first, so
they need network access to GitHub's releases API and a checkout with release
tags (`GH_TOKEN` can be used for authentication). Neither command deploys the
site.

To edit docs offline (once npm dependencies are installed), generate the
history from a saved releases API JSON file and launch VitePress directly,
skipping the networked `predev` hook:

```sh
python3 .github/scripts/update_history.py --published-json /path/to/releases.json
cd docs
npm exec -- vitepress dev
```

## Without a mixer

`just app-mock` builds and runs a development build with the `mockmixer` build
tag, swapping USB 1 for an in-process simulated mixer. The UI, validation, and
`api/v1` requests are all real.

- Every launch starts from the same demo setup. Settings and presets persist
  until the app exits, and clicking Disconnect preserves them.
- Live meters are synthetic and respond to controls, though no actual audio is
  processed. The channel editor marks live readings as "Simulated", and a
  simulated xrun fires in diagnostics events roughly every 47 seconds.
- Set `RC2FX_MOCK_SILENT=1` to silence all channels when measuring idle UI
  overhead.
- USB is never opened, so any real mixer plugged into the machine is left
  alone. The installer is disabled (since it talks to USB directly), and SSH
  reports as unsupported.

`just desktop-mock` compiles the same build without debug info into
`target/mock/rc2fx`, ready to copy to another machine running the same OS.
Because goui-kit is statically linked, no extra files are needed alongside it.

`go -C tools/xtask run . app` rejects combining `--mock` with `--release`, and
packaging rejects any binary built with the `mockmixer` or `rc2fxdev` tags.
`RC2FX_DEV_PAGE` works with mock builds too, for example
`RC2FX_DEV_PAGE=channel:mic\ 4:eq just app-mock`.

## On the mixer over SSH

These commands require the [SSH setup](mixer-workflow.md). Starting or stopping
RC2FX restarts the mixer's audio engine.

| Command | Description |
| --- | --- |
| `just mixer-run` | Build and run on the mixer from RAM. Rebooting returns to the installed build or stock. |
| `just mixer-install` | Build and install to `/opt/rc2fx` so it starts automatically on boot |
| `just mixer-stop` | Stop RC2FX and hand audio back to stock |
| `just mixer-status` / `just mixer-logs` | Show running status / print recent logs |
| `just mixer-xruns [SECONDS]` | Print JACK error recorder entries over `SECONDS` (default 60; `0` prints everything since JACK started) |
| `just mixer-disable` / `just mixer-enable` | Disable / re-enable starting at boot |
| `just mixer-uninstall [--purge]` | Remove RC2FX from the mixer (`--purge` also deletes saved settings) |
| `just bench recording.wav [FLAGS]` | Benchmark every suppression model on the mixer using a 48 kHz WAV file and download the denoised outputs. Run `go -C tools/xtask run . help` for flags. |

## Over USB 1

| Command | Description |
| --- | --- |
| `go -C tools/xtask run . mixer api METHOD PATH [BODY\|@file]` | Send a single [controller API](../reference/api.md) request |
| `go -C desktop run ./cmd/usb-probe` | Read-only USB probe (`--extended` also fetches inputs, presets, and diagnostics) |
