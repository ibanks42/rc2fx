# Building from source

These instructions cover building both the desktop app and the mixer runtime on
Windows, Linux, or macOS. Every build step runs through the Go tool in
`tools/xtask` (`go -C tools/xtask run . <command>`), with `just` providing
short wrappers, so the workflow is identical across platforms. Because
`tools/xtask` is its own Go module, tasks run on a fresh clone before goui-kit
is downloaded, and the desktop module under `desktop/` builds on its own. The
desktop app is written in Go; the mixer runtime is written in Rust.

All Rust crates target Linux only: AArch64 for the mixer itself, and Linux
hosts for running the Rust test suite. On Windows and macOS, `just mixer-build`
still cross-compiles the mixer runtime for the mixer's Linux environment, but
the Rust tests won't run locally.

## 1. Install the tools

- [Go](https://go.dev/dl/) at the version listed in `desktop/go.mod`. Project
  tasks run via `go -C tools/xtask run .`, so every task depends on it.
- Rust via [rustup](https://rustup.rs), needed for the mixer runtime and for
  collecting mixer crate license notices during `just package`. The toolchain
  version is pinned in `rust-toolchain.toml` and installs automatically on
  first use.
- [mise](https://mise.jdx.dev) (optional, recommended): `mise.toml` and
  `mise.lock` pin the repository's command-line tools with checksums for
  Linux (x64), macOS (arm64 and x64), and Windows (x64): `just`, `zig`, `ruff`,
  `shellcheck`, `shfmt`, `actionlint`, `clang-format` (from conda-forge), and
  on Linux `linuxdeploy` and `patchelf`. Run `mise install` (or let `just setup`
  run it) to install those exact versions. `mise` is optional: `xtask` looks for
  each tool on `PATH` first, falls back to `mise which` when `mise` is
  installed without shell activation, and prints a warning if a tool's version
  differs from the pin in `mise.toml`.
- Without `mise`, install the command-line tools you need on `PATH` yourself:
  - [just](https://just.systems): `cargo install --locked just`,
    `winget install Casey.Just`, `brew install just`, or your system package
    manager.
  - [zig](https://ziglang.org/download/) matching the version pinned in
    `mise.toml` (`0.16.0`): `winget install zig.zig`, `brew install zig`,
    `pip install ziglang`, or your package manager. Zig acts as the C compiler
    and linker for the mixer build, targeting the mixer's glibc 2.35 directly
    with no separate sysroot.
  - On Linux, `linuxdeploy` and `patchelf` for packaging, plus the linters
    (`ruff`, `shellcheck`, `shfmt`, `actionlint`, and `clang-format`) for full
    `just lint` coverage.
- [.NET SDK](https://dotnet.microsoft.com/download) 8 or newer, needed by
  `just setup` and `just package` to install and run Velopack's `vpk`. Neither
  `.NET` nor `vpk` is in `mise.toml` because `mise` cannot record checksums for
  them.
- A C compiler for the desktop app's cgo dependencies (the UI and storage
  repair libraries):
  - **Windows:** MinGW-w64 gcc on `PATH` (for example via
    [MSYS2](https://www.msys2.org/) or
    `winget install BrechtSanders.WinLibs.POSIX.UCRT`), which also provides
    `windres` for embedding the `rc2fx.exe` icon.
  - **Linux:** see [Linux setup](linux.md).
  - **macOS:** Xcode command-line tools.

Once those are installed, run the following from the repository root (if you
installed `mise` instead of a global `just`, run `mise install` first or invoke
`go -C tools/xtask run . setup` and `git config core.hooksPath .githooks`):

```sh
just setup
```

`just setup` installs everything needed to build, package, and run the desktop
app and to build the mixer runtime: it downloads the pinned goui-kit static
library and Velopack's C library for your platform, adds the mixer's Rust
target (`aarch64-unknown-linux-gnu`), runs `mise install` when `mise` is
installed (or lists any missing pinned tools and how to install them without
`mise`), installs `cargo-zigbuild` and `cargo-about` 0.8.4, checks for `zig`,
installs the pinned `vpk` into `desktop/third_party/tools/rc2fx-vpk`, and
activates the pre-commit lint hook in `.githooks`. On Linux, it also verifies
that `linuxdeploy` and `patchelf` are available, installs `mksquashfs` via your
system package manager (with `sudo`) when missing, and generates the AppImage
runtime notices and source archive. Running `just setup` again skips downloads
and tools that are already current.

For CI or fetching another platform's build, run
`go -C tools/xtask run . fetch-goui-kit TARGET` (for example
`go -C tools/xtask run . fetch-goui-kit darwin-universal`; omit `TARGET` for the
current machine). It downloads the goui-kit static library (Go bindings for
GPUI-kit) pinned in `config/goui-kit.json`, verifies its SHA-256, and unpacks
it to `desktop/third_party/goui-kit` (gitignored). Building with `-tags gkstatic`
selects `lib/libgouikit_static.a` so nothing ships alongside the executable;
without it, package `gk` looks for the shared library at runtime. If you're
working with a local build of goui-kit, symlink it instead:
`ln -sfn /path/to/goui-kit-VERSION-OS-ARCH desktop/third_party/goui-kit`
(re-running `just setup` or `go -C tools/xtask run . fetch-goui-kit` replaces a
symlink with the downloaded archive). Only the UI packages need goui-kit; tests
for every other package run without it.

## 2. Build and test

```sh
just app           # run the desktop app (development build; `just app-release` for release)
just lint          # check formatting and lints across every language
just test          # schema check, lints, Go vet and tests, then (on Linux) the Rust workspace tests and Clippy
just test-mixer    # mixer-side tests (Linux, with python3 and a C compiler)
```

`just app` doesn't need a mixer cross-build because normal builds download
signed mixer releases. It invokes `go -C tools/xtask run . app`, which compiles a
development build (using the `rc2fxdev` Go build tag, analogous to a Rust debug
build) into `target/debug` and launches it.

Every build of the app passes a linker flag to strip unused goui-kit component
families (`-extldflags=-Wl,--gc-sections`, or `-Wl,-dead_strip` on macOS). On
Windows this requires LLVM's linker, so release builds link with `-fuse-ld=lld`
(requiring `ld.lld` on `PATH` alongside MinGW's gcc) and
`-Xlink=-force:multipleres` so the application manifest embedded by package
`gk` wins over MinGW's default one. `go -C tools/xtask run .` sets these flags
automatically; a raw `go build -tags gkstatic` still works without them, just
with every component family linked in.

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

`just test` runs `schema --check` and `just lint`, checks `desktop/` and
`tools/xtask/`, and fails on any formatting or lint error, Clippy warning, or
`go vet` issue, even if all tests pass. On Linux it also lints the `bench-kernels`
feature; on Windows and macOS it runs only the Go checks
(`just test-rust` runs the Rust checks by themselves on Linux). The Rust
workspace tests invoke `go run ./internal/interop` to verify Rust/Go wire
parity, so Go must be on `PATH` when running `cargo test --workspace`. The
native library and benchmark crates are separate Cargo workspaces checked by
`just test-mixer` alongside the Python and C test suites.

### Formatting and lints

```sh
just lint   # go -C tools/xtask run . lint
just fmt    # go -C tools/xtask run . lint --fix
```

`just lint` checks every language in the repository in one pass: Go (`gofmt`),
Rust (`cargo fmt` across all three Rust workspaces: `Cargo.toml`,
`crates/rc2fx-bench`, and `tools/src_fast_check`), Python (`ruff format` and
`ruff check`), C (`clang-format`), shell (`shfmt` and `shellcheck`), and GitHub
workflows (`actionlint`, passing `shellcheck` for `run:` scripts when
available). `just fmt` formats every language and applies safe `ruff` lint
fixes.

Every language uses its tool's standard style: `.clang-format` is plain LLVM
style, and `shfmt` and `ruff` use their defaults (`ruff.toml` exists only so a
personal ruff config outside the repository does not apply).

`gofmt` and `rustfmt` come from your Go and Rust toolchains; the rest come from
`mise` (pinned in `mise.toml` and `mise.lock`) or `PATH`. When a tool on `PATH`
differs from the pinned version, `xtask` prints a warning that its output may
differ from CI. Locally, any missing tool is skipped with a warning; in CI
(when the `CI` environment variable is set), a missing tool fails the run.

The pre-commit hook in `.githooks/pre-commit` (enabled by `just setup`) runs
`go -C tools/xtask run . lint --staged` on staged files before each commit.
Pass `git commit --no-verify` to skip it once. `just test` also runs `lint`
across the whole repository.

## 3. Package it

```sh
just package
```

This builds self-contained release packages for the current OS into
`target/package`, with third-party license notices bundled inside:

| System | Files |
| --- | --- |
| Windows | `rc2fx-windows-x64-setup.exe` (installer), `io.github.ibanks42.rc2fx-VERSION-full.nupkg` and `releases.win.json` (update package and feed) |
| macOS | `rc2fx-macos-universal.pkg` (installer), `io.github.ibanks42.rc2fx-VERSION-osx-full.nupkg` and `releases.osx.json` (update package and feed), all universal (Apple silicon and Intel) |
| Linux | `rc2fx-linux-x64.AppImage`, `io.github.ibanks42.rc2fx-VERSION-linux-full.nupkg` and `releases.linux.json` (update package and feed) |

All packages are produced by Velopack's `vpk`. Installers and the AppImage omit
the version number from their filenames so latest-release URLs never change and
AppImage self-updates can replace the file in place. The
`.nupkg` files and feeds keep vpk's naming because the feed references the
package by name; packaging verifies that the feed lists that exact package and
SHA-256. The unpacked app remains in `target/package/rc2fx-OS-ARCH/rc2fx`. See
[Releases](releases.md#release-contents) for what gets published. Documentation
is not included in the packages.

On macOS, the app ships as a universal binary targeting macOS 13 (the oldest
version supported by Go 1.27) and newer. `go -C tools/xtask run . package` compiles
`cmd/rc2fx` twice with cgo for `arm64` and `amd64` (passing `-arch` and
`-mmacosx-version-min=13.0` via `CC`, `CGO_CFLAGS`, and `CGO_LDFLAGS`) against
the universal goui-kit library and each architecture's static `velopack_libc`,
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

`just setup` installs all of the libraries and tools packaging needs, so you do
not have to install them by hand:

- **Libraries:** `just setup` downloads and verifies goui-kit and Velopack's C
  library into `desktop/third_party/goui-kit` and
  `desktop/third_party/velopack` (for CI or another platform, run
  `go -C tools/xtask run . fetch-goui-kit TARGET` or
  `go -C tools/xtask run . fetch-velopack TARGET`). Packaged builds enable the
  `velopack` build tag so self-updates work (and `embedded` as well when
  `--embedded` is passed).
- **License notices:** `just setup` installs `cargo-about` 0.8.4, which gathers
  licenses for the mixer's Rust crates to merge with the Go module and goui-kit
  notices. Because packages ship Velopack's launcher, updater, and installer,
  the app's notices also pull in Velopack's dependencies from
  `licenses/velopack-VERSION.json`. Whenever you bump Velopack in
  `config/velopack.json` (shared by `go -C tools/xtask run . fetch-vpk`,
  `go -C tools/xtask run . fetch-velopack`, and packaging), regenerate that report
  with `.github/scripts/velopack-licenses.py VERSION`; packaging rejects
  mismatched reports.
- **Velopack (`vpk`):** `just setup` (or `go -C tools/xtask run . fetch-vpk` on its
  own) installs the pinned `vpk` (`1.3.39-g8d2844f`, requiring the .NET SDK)
  into `desktop/third_party/tools/rc2fx-vpk`, where packaging finds it
  automatically when `RC2FX_VPK` is unset. (`vpk` and the .NET SDK stay outside
  `mise.toml` because `mise` cannot record checksums for them.) On Windows, release builds
  (`RC2FX_RELEASE_BUILD=1`) also require MinGW-w64's `windres`
  (`RC2FX_WINDRES`, or `x86_64-w64-mingw32-windres` / `windres` on `PATH`) to
  embed the icon and version metadata in `rc2fx.exe`; non-release builds skip
  resources if `windres` is absent.
- **Linux AppImage tools and notices:** On Linux, `linuxdeploy` and `patchelf`
  are pinned in `mise.toml` and installed by `mise` during `just setup` (or
  found on `PATH` without `mise`). `just setup` verifies both are available,
  installs `mksquashfs` via your system package manager if it is missing, and
  runs `.github/scripts/prepare-appimage-notices.py` to generate the AppImage
  runtime's license notices and source archive (see
  [AppImage runtime sources](releases.md#appimage-runtime-sources)). Packaging
  finds `linuxdeploy` and `patchelf` on `PATH` or through `mise` automatically
  when `RC2FX_LINUXDEPLOY` and `RC2FX_PATCHELF` are unset. Since `vpk` prepends
  its own AppImage runtime, you don't need a separate runtime or `appimagetool`.
- **glibc compatibility:** An AppImage requires at least the glibc version it
  was built against. Release builds run inside the AlmaLinux 8 container
  described [below](#check-the-linux-packages-before-a-release) (glibc 2.28).
  Packaging directly on a newer Linux host produces an AppImage tied to that
  host's glibc.

Each platform is packaged natively on its own OS; see
[Releases](releases.md#per-platform-build-runners) for why.

Local Windows and macOS packages are unsigned unless you configure the signing
variables documented in [Signing](releases.md#signing); Linux AppImages are not
signed by vpk. `go -C tools/xtask run . package --build-only` stops after
compilation, and `--skip-build` packages a previously built release binary (the
Windows release job uses these two flags to authenticate to Azure in between).

### Check the Linux packages before a release

```sh
just package-check
```

This builds the AppImage the same way a release does, then extracts and tests
it inside clean containers for AlmaLinux 8, Ubuntu 22.04, Debian 12, Ubuntu
24.04, Debian 13, and Fedora 44. It runs locally on x86-64 Linux with Podman or
Docker and network access. Set `CONTAINER_ENGINE` to `podman` or `docker` to
force one; otherwise it defaults to Podman when available.

Because the container checks are headless, they don't exercise a graphical
session, FUSE mounting, or USB mixer communication; test those manually.

The build runs against a snapshot of your working tree (including uncommitted
and untracked files, excluding gitignored ones) inside the AlmaLinux 8
container defined by `.github/scripts/package-check.Containerfile`, which
includes checksum-verified `mise` and runs `mise install --locked` to fetch
`linuxdeploy` and `patchelf` at the pins in `mise.lock`. Release
builds use the same container, and the **Check** workflow runs Rust lints and tests
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
