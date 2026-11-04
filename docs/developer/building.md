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
just app           # build the mixer runtime, then run the desktop app
just test          # workspace tests, then Clippy with warnings as errors
just test-mixer    # mixer-side tests (Linux or macOS, with python3 and a C compiler)
```

`just app`, `just test` and the other desktop recipes run `just mixer-build`
first. It cross-builds the mixer runtime into `target/mixer` on your computer
and does not contact the mixer. Files that haven't changed are left in place,
so repeat runs are quick.

The desktop build embeds that runtime, the firmware catalog, the mixer release
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
`target/package`. On Windows and macOS that's a zip holding the app, with its
licence notices built in, and a `SHA256SUMS`. On Linux it's a single AppImage.
Releases publish them as `rc2fx-VERSION-OS-ARCH.EXT` (see
[Releases](releases.md#release-contents)). The documentation isn't packaged.

Packaging needs `cargo-about` (`cargo install --locked cargo-about`). On Linux
it also needs `linuxdeploy`, `appimagetool` and the AppImage type 2 runtime,
given by absolute path in `RC2FX_LINUXDEPLOY`, `RC2FX_APPIMAGETOOL` and
`RC2FX_APPIMAGE_RUNTIME`. Each platform is packaged on its own machine;
[Releases](releases.md#per-platform-build-runners) explains why.

## Next steps

- [Working on a mixer](mixer-workflow.md) covers testing changes on a mixer
  without reflashing.
- [Commands](commands.md) lists every recipe.
