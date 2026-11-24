# RC2FX desktop app (Go)

This directory holds the RC2FX desktop app for Windows, macOS, and Linux,
written in Go on top of GPUI-kit via goui-kit. The mixer side
stays in Rust under `crates/`, mixer-side scripts live in `mixer/scripts`, and
packaging assets (AppImage launcher, desktop entry, icons, and macOS
entitlements) live in `packaging/`.

## Building

You'll need Go 1.27 (matching `go.mod`), a C compiler (cgo is used for the UI,
storage repair, and Velopack), and the goui-kit static library. Run `just setup`
from the repository root to install everything needed to build, package, and run
the app (goui-kit, Velopack's C library, the command-line tools pinned in
`mise.toml` when `mise` is installed, `cargo-about`, `vpk`, Linux packaging
setup and AppImage notices, the mixer build tools, and the pre-commit lint
hook). Running `just setup` again skips downloads and tools that are already
current. To
fetch goui-kit on its own (for CI or another platform, such as
`darwin-universal`), run:

```sh
go -C tools/xtask run . fetch-goui-kit TARGET
```

(Omit `TARGET` to fetch for the current machine.) This downloads the version
pinned in `config/goui-kit.json` from the `ibanks42/goui-kit` releases,
verifies its SHA-256, and unpacks it into `third_party/goui-kit` (gitignored).
Passing `-tags gkstatic` selects the static library so nothing needs to ship
alongside the app; without it, package `gk` looks for the shared library at
runtime. On macOS, the download is universal (`darwin-universal`, containing
both `arm64` and `x86_64` slices): the macOS app ships universal, built once
per architecture and merged with `lipo` by `just package`. If you have a
local build of goui-kit, symlink it instead (re-running `just setup` or
`go -C tools/xtask run . fetch-goui-kit` replaces a symlink with the downloaded
archive):

```sh
ln -sfn /path/to/goui-kit-0.11.0-linux-x86_64 desktop/third_party/goui-kit
```

From the repository root, use the standard entry points:

```sh
just app              # go -C tools/xtask run . app: development build in target/debug, then run it
just app-mock         # go -C tools/xtask run . app --mock: against a simulated mixer
just test             # schema check, lints, Go vet and tests, then the Rust workspace (on Linux)
just package          # go -C tools/xtask run . package: release packages in target/package
```

Or run the Go commands directly from `desktop/`:

```sh
go test $(go list ./... | grep -v -e '/app$' -e '/app/uitest$' -e '/cmd/rc2fx$')   # no cgo or goui-kit needed
go test -tags mockmixer ./transport/...
CGO_LDFLAGS="-L$PWD/third_party/goui-kit/lib" \
  go build -tags gkstatic,rc2fxdev -ldflags=-extldflags=-Wl,--gc-sections -o bin/rc2fx ./cmd/rc2fx
```

The `-ldflags` flag strips goui-kit component families the app never calls. On
macOS that flag is `-Wl,-dead_strip`, and on Windows it requires LLVM's linker
(`'-extldflags=-fuse-ld=lld -Wl,--gc-sections,-Xlink=-force:multipleres'`,
which `go -C tools/xtask run .` sets automatically). A plain build without those flags still
works, just with every component family linked in:

```sh
CGO_LDFLAGS="-L$PWD/third_party/goui-kit/lib" go build -tags gkstatic -o bin/rc2fx ./cmd/rc2fx
```

Build tags: `gkstatic` links the goui-kit static library (without it, package
`gk` looks for the shared library at runtime), `rc2fxdev` enables development
builds, `mockmixer` swaps USB 1 for an in-process simulated mixer, and
`embedded` compiles in the mixer payload staged from `target/mixer` by
`go run ./cmd/stage-payload -root ..` (`go -C tools/xtask run . app --embedded` handles both
steps).

Self-update only links Velopack's C library when built with the `velopack` tag.
`just setup` downloads and verifies it into the gitignored
`third_party/velopack` (or run `go -C tools/xtask run . fetch-velopack TARGET` in CI or for
another platform). To link it manually, add `velopack` to `-tags` (for
example `-tags gkstatic,velopack`) and add
`-L$PWD/third_party/velopack/GOOS-GOARCH -lvelopack_libc` to
`CGO_LDFLAGS`. (This has to go in `CGO_LDFLAGS` rather than a `#cgo` directive
because Velopack's static libraries contain LLVM bitcode, which Go rejects in
`#cgo`; `go -C tools/xtask run .` sets it for you.) Release packaging always requires this tag;
builds without it can't self-update and ignore Velopack's install/uninstall
hooks.

Every package outside `app` and `cmd/rc2fx` avoids importing goui-kit, so they
build and test on any machine without the UI library.

## Layout

See [The desktop app in Go](../docs/developer/desktop-go.md) for the package
layout, how each package maps to its Rust counterpart, and the naming, strict
JSON, shared fixture, and cgo rules.
