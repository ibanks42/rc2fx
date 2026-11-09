# Building from source

These steps build the desktop app and the mixer runtime on Windows, Linux or
macOS. Every task is a cargo command under `xtask/`, and `just` wraps them, so
the steps are the same on each platform.

## 1. Install the tools

- Rust, through [rustup](https://rustup.rs). The toolchain is pinned in
  `rust-toolchain.toml`, and rustup installs it on first use.
- [just](https://just.systems): `cargo install --locked just`,
  `winget install Casey.Just`, `brew install just`, or your package manager.
- [zig](https://ziglang.org/download/) 0.13 or newer: `winget install zig.zig`,
  `brew install zig`, `pip install ziglang`, or your package manager. zig is
  the C compiler and linker for the mixer build. It targets the mixer's glibc
  (2.35) directly, so no sysroot is needed.
- The desktop app's build tools:
  - Windows: Visual Studio's C++ build tools and the Windows SDK.
  - Linux: see [Linux setup](linux.md).
  - macOS: the Xcode command-line tools.

Then run, from the repository:

```sh
just setup
```

This adds the mixer's Rust target (`aarch64-unknown-linux-gnu`), installs
`cargo-zigbuild`, and checks that zig is on your `PATH`.

## 2. Build and test

```sh
just app           # run the desktop app (debug build; `just app-release` for release)
just test          # workspace tests, then Clippy with warnings as errors
just test-mixer    # mixer-side tests (Linux or macOS, with python3 and a C compiler)
```

`just app` needs no mixer cross-build: the app downloads signed mixer
releases. `just test` and the `*-embedded` recipes run `just mixer-build`
first. It cross-builds the mixer runtime into `target/mixer` on your computer
and does not contact the mixer. Files that haven't changed are left in place,
so repeat runs are quick.

An embedded build embeds that runtime, the firmware catalog, the mixer release
manifest and the installer scripts in the executable, and fails if any of them
is missing or corrupt. `target/mixer` carries a copy of
`config/mixer-release.json`, and the build fails if the two differ, so run
`just mixer-build` after bumping the mixer version.
Rebuild the app after changing any of them. Putting new files beside an
already-built app does not change its installer.

There is one mixer build configuration, the one the Pro II was measured with,
compiled for its Cortex-A53. Installing and using a source build works as
described in the [user guide](../user/installing.md).

Any Clippy warning fails `just test`, even if every test passes. The native
library is a separate workspace, and its tests run under `just test-mixer`.

## 3. Package it

```sh
just package
```

This builds the self-contained desktop app for the current computer into
`target/package`, with its licence notices built in. What it writes depends on
the system:

| System | Files |
| --- | --- |
| Windows | `rc2fx-windows-x64-setup.exe` (installer), `rc2fx-windows-x64.zip` (portable), `io.github.ibanks42.rc2fx-VERSION-full.nupkg` and `releases.win.json` (update package and feed) |
| macOS | `rc2fx-macos-arm64.pkg` (installer), `rc2fx-macos-arm64.zip` (portable), `io.github.ibanks42.rc2fx-VERSION-osx-full.nupkg` and `releases.osx.json` (update package and feed) |
| Linux | `rc2fx-linux-x64.AppImage`, `io.github.ibanks42.rc2fx-VERSION-linux-full.nupkg` and `releases.linux.json` (update package and feed) |

Velopack's `vpk` makes all of them. The installers, portable zips and the
AppImage have no version in their names, so the newest release always has the
same download links, and an AppImage update replaces the file in place. The
update package and feed keep vpk's names, because the feed names the package;
packaging checks that the feed lists exactly that package with its SHA-256.
The unpacked app stays in `target/package/rc2fx-OS-ARCH/rc2fx`. Releases publish these
files (see [Releases](releases.md#release-contents)). The documentation isn't
packaged.

Packaging needs `cargo-about` (`cargo install --locked cargo-about`).

- Every platform also needs .NET and vpk 1.3.39-g8d2844f:
  `dotnet tool install -g vpk --version 1.3.39-g8d2844f`. Set `RC2FX_VPK` to
  its path, or put `vpk` on your `PATH`. Release builds get a checked copy
  from `.github/scripts/setup-vpk.sh`. On Windows, a release build
  (`RC2FX_RELEASE_BUILD=1`) also needs a resource compiler, from the Windows
  SDK, to give `rc2fx.exe` its icon and version details; other builds skip
  them when there is none.
- The packages include Velopack's own launcher, updater and installer, so the
  app's licence notices include their dependencies, from
  `licenses/velopack-VERSION.json`. When you change the vpk version
  (`VPK_VERSION` in `xtask/src/package/velopack.rs`, and
  `.github/scripts/setup-vpk.sh`), regenerate it with
  `.github/scripts/velopack-licenses.py VERSION`; packaging refuses a report
  for another version.
- Linux also needs `mksquashfs` (from `squashfs-tools`) on the `PATH`, which
  vpk builds the AppImage's image with, and `linuxdeploy` and `patchelf`,
  given by absolute path in `RC2FX_LINUXDEPLOY` and `RC2FX_PATCHELF`. `bash .github/scripts/setup-release-tools.sh appimage`
  downloads the pinned versions, checks them and keeps them under
  `~/.cache/rc2fx-release-tools/appimage`. vpk puts its own AppImage runtime
  at the start of the AppImage, so there is no separate runtime or
  `appimagetool` to install. Before packaging, run
  `python3 .github/scripts/prepare-appimage-notices.py`: it writes that
  runtime's licence notices, which packaging requires, and its source archive
  (see [AppImage runtime sources](releases.md#appimage-runtime-sources)).
- The AppImage runs on distributions with at least the glibc it was built
  against, so a release builds it in the AlmaLinux 8 container described
  [below](#check-the-linux-packages-before-a-release) (glibc 2.28). An
  AppImage packaged directly on a newer system needs that system's glibc or
  newer.

Each platform is packaged on its own machine;
[Releases](releases.md#per-platform-build-runners) explains why.

Local Windows and macOS packages are unsigned unless you set the signing
variables in [Signing](releases.md#signing); vpk signs nothing on Linux. `cargo xtask package --build-only`
stops after the build, and `--skip-build` packages an existing release build;
the Windows release job uses the two to sign in to Azure between them.

### Check the Linux packages before a release

```sh
just package-check
```

This builds the AppImage the way a release does, then extracts and checks it
in clean containers of AlmaLinux 8, Ubuntu 22.04, Debian 12, Ubuntu 24.04,
Debian 13 and Fedora 44. It needs x86-64 Linux with Podman or Docker, plus network access, and
no mixer or GitHub run. Set `CONTAINER_ENGINE` to `podman` or `docker` to choose
one; otherwise Podman is used if it's installed.

The checks are headless. They don't start a desktop session, mount the AppImage
with FUSE or talk to a mixer, so try those by hand.

The build runs on a copy of your working tree, including uncommitted and new
files but not ignored ones, in an AlmaLinux 8 container defined in
`.github/scripts/package-check.Containerfile`. Releases build in the same
container. AlmaLinux 8 has glibc 2.28, so the AppImage runs on Debian 10,
Ubuntu 20.04, RHEL 8, Fedora 29 and newer. The build makes the mixer runtime
from that copy, so it never reuses `target/mixer`. Each run upgrades the
container's packages first, to match what a release installs.

The last lines report the commit and a digest of the source that was checked.
If the source changes during the run, the check fails, because the result no
longer describes your working tree.

Its build output, downloads and packages stay in `target/package-check`, and
the packages are in `target/package-check/target/package`. The first run builds
everything and takes the longest; later runs reuse that directory. Delete it to
start clean.

## Next steps

- [Working on a mixer](mixer-workflow.md) covers testing changes on a mixer
  without reflashing.
- [Commands](commands.md) lists every recipe.
