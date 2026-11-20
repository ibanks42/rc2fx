# RC2FX desktop app (Go)

This directory holds the RC2FX desktop app for Windows, macOS, and Linux,
written in Go on top of GPUI-kit via the gpuikit Go bindings. The mixer side
stays in Rust under `crates/`, mixer-side scripts live in `mixer/scripts`, and
packaging assets (AppImage launcher, desktop entry, icons, and macOS
entitlements) live in `packaging/`.

## Building

You'll need Go 1.27 (matching `go.mod`), a C compiler (cgo is used for the UI,
storage repair, and Velopack), and the gpuikit static library. Fetch gpuikit
once from the repository root:

```sh
desktop/scripts/fetch-gpuikit.sh
```

This downloads the pinned version for your platform from the
`ibanks42/gpui-kit-bindings` releases, verifies its SHA-256, and unpacks it
into `third_party/gpuikit` (gitignored). Because it is a static library linked
into the binary, nothing needs to ship alongside the app. On macOS, the
download is universal (`darwin-universal`, containing both `arm64` and `x86_64`
slices): the macOS app ships universal, built once per architecture and merged
with `lipo` by `./xtask package`. Set `GPUIKIT_TOKEN` if the bindings
repository is private to you. If you have a local build of the bindings,
symlink it instead:

```sh
ln -sfn /path/to/gpuikit-0.0.1-linux-x86_64 desktop/third_party/gpuikit
```

From the repository root, use the standard entry points:

```sh
just app              # ./xtask app: development build in target/debug, then run it
just app-mock         # ./xtask app --mock: against a simulated mixer
just test             # Go vet and tests, then the Rust workspace (on Linux)
./xtask package       # release packages in target/package
```

Or run the Go commands directly from `desktop/`:

```sh
go test $(go list ./... | grep -v -e '/app$' -e '/app/uitest$' -e '/cmd/rc2fx$')   # no cgo or gpuikit needed
go test -tags mockmixer ./transport/...
CGO_LDFLAGS="-L$PWD/third_party/gpuikit/lib" \
  go build -tags rc2fxdev -ldflags=-extldflags=-Wl,--gc-sections -o bin/rc2fx ./cmd/rc2fx
```

The `-ldflags` flag strips gpuikit component families the app never calls. On
macOS that flag is `-Wl,-dead_strip`, and on Windows it requires LLVM's linker
(`'-extldflags=-fuse-ld=lld -Wl,--gc-sections,-Xlink=-force:multipleres'`,
which `./xtask` sets automatically). A plain build without those flags still
works, just with every component family linked in:

```sh
CGO_LDFLAGS="-L$PWD/third_party/gpuikit/lib" go build -o bin/rc2fx ./cmd/rc2fx
```

Build tags: `rc2fxdev` enables development builds, `mockmixer` swaps USB 1 for
an in-process simulated mixer, and `embedded` compiles in the mixer payload
staged from `target/mixer` by `go run ./cmd/stage-payload -root ..`
(`./xtask app --embedded` handles both steps).

Self-update only links Velopack's C library when built with the `velopack` tag.
Fetch it once with `scripts/fetch-velopack.sh` (pinned and checksummed,
unpacked into the gitignored `third_party/velopack`), then pass `-tags velopack`
and add `-L$PWD/third_party/velopack/GOOS-GOARCH -lvelopack_libc` to
`CGO_LDFLAGS`. (This has to go in `CGO_LDFLAGS` rather than a `#cgo` directive
because Velopack's static libraries contain LLVM bitcode, which Go rejects in
`#cgo`; `./xtask` sets it for you.) Release packaging always requires this tag;
builds without it can't self-update and ignore Velopack's install/uninstall
hooks.

Every package outside `app` and `cmd/rc2fx` avoids importing gpuikit, so they
build and test on any machine without the UI library.

## Layout

See [The desktop app in Go](../docs/developer/desktop-go.md) for the package
layout, how each package maps to its Rust counterpart, and the naming, strict
JSON, shared fixture, and cgo rules.
