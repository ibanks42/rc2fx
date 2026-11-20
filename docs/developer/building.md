# Building from source

These instructions cover building both the desktop app and the mixer runtime on
Windows, Linux, or macOS. Every build step runs through `./xtask` (the Go tool
in `desktop/cmd/xtask`), with `just` providing short wrappers, so the workflow
is identical across platforms. The desktop app is written in Go; the mixer
runtime is written in Rust.

All Rust crates target Linux only: AArch64 for the mixer itself, and Linux
hosts for running the Rust test suite. On Windows and macOS, `just mixer-build`
still cross-compiles the mixer runtime for the mixer's Linux environment, but
the Rust tests won't run locally.

## 1. Install the tools

- [Go](https://go.dev/dl/) at the version listed in `desktop/go.mod`. `./xtask`
  runs via `go run`, so every task depends on it.
- Rust via [rustup](https://rustup.rs), needed for the mixer runtime and for
  collecting mixer crate license notices during `just package`. The toolchain
  version is pinned in `rust-toolchain.toml` and installs automatically on
  first use.
- [just](https://just.systems): install via `cargo install --locked just`,
  `winget install Casey.Just`, `brew install just`, or your system package
  manager.
- [zig](https://ziglang.org/download/) 0.13 or newer: `winget install zig.zig`,
  `brew install zig`, `pip install ziglang`, or your package manager. Zig acts
  as the C compiler and linker for the mixer build, targeting the mixer's
  glibc 2.35 directly with no separate sysroot.
- A C compiler for the desktop app's cgo dependencies (the UI and storage
  repair libraries):
  - **Windows:** MinGW-w64 gcc on `PATH` (for example via
    [MSYS2](https://www.msys2.org/) or
    `winget install BrechtSanders.WinLibs.POSIX.UCRT`), which also provides
    `windres` for embedding the `rc2fx.exe` icon.
  - **Linux:** see [Linux setup](linux.md).
  - **macOS:** Xcode command-line tools.

Once those are installed, run the following from the repository root:

```sh
just setup
desktop/scripts/fetch-gpuikit.sh
```

`just setup` installs the mixer's Rust target (`aarch64-unknown-linux-gnu`),
installs `cargo-zigbuild`, verifies `zig` is on `PATH`, and activates the
pre-commit hook in `.githooks`.

`fetch-gpuikit.sh` downloads the pinned gpuikit static library (GPUI-kit with
its Go bindings) for your platform, verifies its SHA-256, and unpacks it to
`desktop/third_party/gpuikit` (gitignored). Because `lib/libgpuikit_static.a`
is linked statically into the app, nothing ships alongside the executable. Set
`GPUIKIT_TOKEN` to a GitHub token if the bindings repository is private to you.
If you're working with a local build of the bindings, symlink it instead:
`ln -sfn /path/to/gpuikit-VERSION-OS-ARCH desktop/third_party/gpuikit`. Only
the UI packages need gpuikit; tests for every other package run without it.

## 2. Build and test

```sh
just app           # run the desktop app (development build; `just app-release` for release)
just test          # Go vet and tests for the desktop app, then (on Linux) the Rust workspace tests and Clippy
just test-mixer    # mixer-side tests (Linux, with python3 and a C compiler)
```

`just app` doesn't need a mixer cross-build because normal builds download
signed mixer releases. It invokes `./xtask app`, which compiles a development
build (using the `rc2fxdev` Go build tag, analogous to a Rust debug build) into
`target/debug` and launches it.

Every build of the app passes a linker flag to strip unused gpuikit component
families (`-extldflags=-Wl,--gc-sections`, or `-Wl,-dead_strip` on macOS). On
Windows this requires LLVM's linker, so release builds link with `-fuse-ld=lld`
(requiring `ld.lld` on `PATH` alongside MinGW's gcc) and
`-Xlink=-force:multipleres` so the application manifest embedded by package
`gk` wins over MinGW's default one. `./xtask` sets these flags automatically; a
raw `go build` still works without them, just with every component family
linked in.

`just test` and the `*-embedded` recipes run `just mixer-build` first, which
cross-compiles the mixer runtime into `target/mixer` locally without touching a
mixer. Unchanged outputs are preserved, so incremental runs are fast.

An embedded build (built with the `embedded` Go tag) compiles the mixer
runtime, firmware catalog, mixer release manifest, and installer scripts right
into the executable. Before compiling, `desktop/cmd/stage-payload` stages them
into `desktop/installer/embed/payload` and fails if any file is missing or
corrupt. `target/mixer` holds a copy of `config/mixer-release.json` and the
build fails if it drifts from the repository copy, so re-run `just mixer-build`
after bumping the mixer version. Always rebuild the app after modifying any of
these inputs; dropping new files next to an existing binary has no effect on
its installer.

There is a single mixer build configuration, targeting the Pro II's Cortex-A53
and matching the configuration measured on hardware. Installing and running a
source build works the same as described in the
[user guide](../user/installing.md).

`just test` fails on any Clippy warning, `go vet` issue, or unformatted Go
file, even if all tests pass. On Linux it also lints the `eq-fma` and
`bench-kernels` features; on Windows and macOS it runs only the Go checks
(`just test-rust` runs the Rust checks by themselves on Linux). The native
library and benchmark crates are separate Cargo workspaces checked by
`just test-mixer` alongside the Python and C test suites.

## 3. Package it

```sh
just package
```

This builds self-contained release packages for the current OS into
`target/package`, with third-party license notices bundled inside:

| System | Files |
| --- | --- |
| Windows | `rc2fx-windows-x64-setup.exe` (installer), `rc2fx-windows-x64.zip` (portable), `io.github.ibanks42.rc2fx-VERSION-full.nupkg` and `releases.win.json` (update package and feed) |
| macOS | `rc2fx-macos-universal.pkg` (installer), `rc2fx-macos-universal.zip` (portable), `io.github.ibanks42.rc2fx-VERSION-osx-full.nupkg` and `releases.osx.json` (update package and feed), all universal (Apple silicon and Intel) |
| Linux | `rc2fx-linux-x64.AppImage`, `io.github.ibanks42.rc2fx-VERSION-linux-full.nupkg` and `releases.linux.json` (update package and feed) |

All packages are produced by Velopack's `vpk`. Installers, portable zips, and
the AppImage omit the version number from their filenames so latest-release
URLs never change and AppImage self-updates can replace the file in place. The
`.nupkg` files and feeds keep vpk's naming because the feed references the
package by name; packaging verifies that the feed lists that exact package and
SHA-256. The unpacked app remains in `target/package/rc2fx-OS-ARCH/rc2fx`. See
[Releases](releases.md#release-contents) for what gets published. Documentation
is not included in the packages.

On macOS, the app ships as a universal binary targeting macOS 13 (the oldest
version supported by Go 1.27) and newer. `./xtask package` compiles `cmd/rc2fx`
twice with cgo for `arm64` and `amd64` (passing `-arch` and
`-mmacosx-version-min=13.0` via `CC`, `CGO_CFLAGS`, and `CGO_LDFLAGS`) against
the universal gpuikit library and each architecture's static `velopack_libc`,
then merges them with `lipo -create` (overridable via `RC2FX_LIPO`). It then
verifies in Go that both the app binary and vpk's `UpdateMac` contain exactly
`arm64` and `x86_64` slices targeting no newer macOS than the bundle's
`LSMinimumSystemVersion`, that all linked libraries are either system libraries
or bundled alongside the executable, and that the `.pkg`'s `hostArchitectures`
lists both. Payload, license notice, and build-tag checks run independently on
each slice. `vpk` packs with `--runtime osx-arm64`, since it refuses a runtime
without an architecture. On macOS the RID is only metadata in the package: vpk
bundles the same `UpdateMac` either way, and the channel (`osx`) and feed names
stay the same, so users on earlier Apple silicon-only builds update
seamlessly.

Packaging requires the gpuikit download, Velopack's C library, and
`cargo-about` (`cargo install --locked cargo-about`):

- Run `desktop/scripts/fetch-velopack.sh` to download and verify Velopack's C
  library into `desktop/third_party/velopack`. Packaged builds enable the
  `velopack` build tag so self-updates work (and `embedded` as well when
  `--embedded` is passed).
- `cargo-about` gathers licenses for the mixer's Rust crates, which are merged
  with the Go module and gpuikit notices.
- Every platform needs .NET and `vpk` `1.3.39-g8d2844f`:
  `dotnet tool install -g vpk --version 1.3.39-g8d2844f`. Put `vpk` on `PATH`
  or point `RC2FX_VPK` at it. CI release builds install a checksummed copy via
  `.github/scripts/setup-vpk.sh`. On Windows, release builds
  (`RC2FX_RELEASE_BUILD=1`) also require MinGW-w64's `windres`
  (`RC2FX_WINDRES`, or `x86_64-w64-mingw32-windres` / `windres` on `PATH`) to
  embed the icon and version metadata in `rc2fx.exe`; non-release builds skip
  resources if `windres` is absent.
- Because packages ship Velopack's launcher, updater, and installer, the app's
  license notices also pull in Velopack's dependencies from
  `licenses/velopack-VERSION.json`. Whenever you bump vpk (keeping
  `VPKVersion` in `desktop/internal/xtask/licenses/licenses.go`,
  `.github/scripts/setup-vpk.sh`, and `version`/`sha` in
  `desktop/scripts/fetch-velopack.sh` in sync), regenerate that report with
  `.github/scripts/velopack-licenses.py VERSION`; packaging rejects mismatched
  reports.
- Linux additionally requires `mksquashfs` (from `squashfs-tools`) on `PATH`,
  plus absolute paths to `linuxdeploy` and `patchelf` in `RC2FX_LINUXDEPLOY`
  and `RC2FX_PATCHELF`. Running
  `bash .github/scripts/setup-release-tools.sh appimage` downloads and verifies
  pinned copies into `~/.cache/rc2fx-release-tools/appimage`. Since `vpk`
  prepends its own AppImage runtime, you don't need a separate runtime or
  `appimagetool`. Before packaging on Linux, run
  `python3 .github/scripts/prepare-appimage-notices.py` to generate the
  runtime's license notices (required by packaging) and its source archive (see
  [AppImage runtime sources](releases.md#appimage-runtime-sources)).
- An AppImage requires at least the glibc version it was built against. Release
  builds run inside the AlmaLinux 8 container described
  [below](#check-the-linux-packages-before-a-release) (glibc 2.28). Packaging
  directly on a newer Linux host produces an AppImage tied to that host's
  glibc.

Each platform is packaged natively on its own OS; see
[Releases](releases.md#per-platform-build-runners) for why.

Local Windows and macOS packages are unsigned unless you configure the signing
variables documented in [Signing](releases.md#signing); Linux AppImages are not
signed by vpk. `./xtask package --build-only` stops after compilation, and
`--skip-build` packages a previously built release binary (the Windows release
job uses these two flags to authenticate to Azure in between).

### Check the Linux packages before a release

```sh
just package-check
```

This builds the AppImage the same way a release does, then extracts and tests
it inside clean containers for AlmaLinux 8, Ubuntu 22.04, Debian 12, Ubuntu
24.04, Debian 13, and Fedora 44. It requires x86-64 Linux with Podman or Docker
and network access, with no mixer or GitHub Actions run needed. Set
`CONTAINER_ENGINE` to `podman` or `docker` to force one; otherwise it defaults
to Podman when available.

Because the container checks are headless, they don't exercise a graphical
session, FUSE mounting, or USB mixer communication; test those manually.

The build runs against a snapshot of your working tree (including uncommitted
and untracked files, excluding gitignored ones) inside the AlmaLinux 8
container defined by `.github/scripts/package-check.Containerfile`. Release
builds use the same container, and the **Check** workflow runs lints and tests
in it. AlmaLinux 8 provides glibc 2.28, keeping the AppImage compatible with
Debian 10, Ubuntu 20.04, RHEL 8, Fedora 29, and newer. Because the AppImage
doesn't bundle the mixer runtime, no mixer cross-build runs here. Local runs
build and update the container image locally, while GitHub Actions pulls the
prebuilt image published by the **Build image** workflow to
`ghcr.io/ibanks42/rc2fx-build`.

At the end of the run, the output prints the commit and a digest of the checked
source tree. Modifying files in your working tree mid-run causes the check to
fail so the result always reflects an exact snapshot.

Build artifacts, downloads, and output packages are cached in
`target/package-check` (with finished packages under
`target/package-check/target/package`). The first run builds everything from
scratch; subsequent runs reuse the cache. Delete `target/package-check` for a
clean rebuild.

## Next steps

- [Working on a mixer](mixer-workflow.md) explains how to deploy and test
  changes on a mixer without reflashing.
- [Commands](commands.md) lists every available recipe.
