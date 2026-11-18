# RC2FX desktop app (Go)

The desktop app for Windows, macOS, and Linux, written in Go on GPUI-kit through
the gpuikit Go bindings. The mixer side stays in Rust under `crates/`; the
scripts that run on the mixer are in `mixer/scripts`, and the files the app's
packages ship (AppImage launcher, desktop entry, icons, entitlements) are in
`packaging/`.

## Building

You need Go 1.27 (the version `go.mod` names), a C compiler (cgo, for the UI,
the storage repair, and Velopack), and the gpuikit download. Fetch it once, from
the repository root:

```sh
desktop/scripts/fetch-gpuikit.sh
```

It downloads the pinned version for this platform from the
ibanks42/gpui-kit-bindings releases, checks its SHA-256, and unpacks it into
the gitignored `third_party/gpuikit`. It is a static library, linked into the
app, so nothing ships beside it. On macOS that is the universal download
(`darwin-universal`, arm64 and x86_64 in one library): the macOS app ships
universal, built once per architecture and joined with `lipo` by `./xtask
package`. Set `GPUIKIT_TOKEN` if that repository
is private to you. With a local build of the bindings, link the folder
instead:

```sh
ln -sfn /path/to/gpuikit-0.0.1-linux-x86_64 desktop/third_party/gpuikit
```

Then use the usual entry points from the repository root:

```sh
just app              # ./xtask app: development build in target/debug, then run it
just app-mock         # ./xtask app --mock: against a simulated mixer
just test             # Go vet and tests, then the Rust workspace
./xtask package       # release packages in target/package
```

Or, from `desktop/`, the raw Go commands:

```sh
go test $(go list ./... | grep -v -e '/app$' -e '/app/uitest$' -e '/cmd/rc2fx$')   # no cgo or gpuikit needed
go test -tags mockmixer ./transport/...
CGO_LDFLAGS="-L$PWD/third_party/gpuikit/lib" \
  go build -tags rc2fxdebug,rc2fxdev -ldflags=-extldflags=-Wl,--gc-sections -o bin/rc2fx ./cmd/rc2fx
```

The `-ldflags` part drops the gpuikit component families the app does not
use; on macOS it is `-Wl,-dead_strip`, and on Windows it needs LLVM's linker
(`'-extldflags=-fuse-ld=lld -Wl,--gc-sections,-Xlink=-force:multipleres'`, as
`./xtask` passes). Without it the build works but carries every family:

```sh
CGO_LDFLAGS="-L$PWD/third_party/gpuikit/lib" go build -o bin/rc2fx ./cmd/rc2fx
```

Build tags: `rc2fxdebug` and `rc2fxdev` make a development build,
`mockmixer` swaps USB 1 for a simulated mixer, and `embedded` compiles in the
mixer payload, which `go run ./installer/embed/cmd/stage-payload -root ..`
stages from `target/mixer` first (`./xtask app --embedded` does both).

Self-update links Velopack's C library only with the `velopack` build tag.
Fetch it once with `scripts/fetch-velopack.sh` (pinned and checked; it lands
in the gitignored `third_party/velopack`), then add `-tags velopack` and
`-L$PWD/third_party/velopack/GOOS-GOARCH -lvelopack_libc` to `CGO_LDFLAGS`
(not a `#cgo` directive: its static libraries carry LLVM bitcode, which Go
refuses there; `./xtask` adds it). Release
packaging must build with the tag; a build without it cannot update itself
and ignores Velopack's install and uninstall hooks.

Packages other than `app` and `cmd/rc2fx` do not import the bindings, so they
build and test on any machine without the library.

## Layout

[The desktop app in Go](../docs/developer/desktop-go.md) describes the
packages, how they map to the mixer's Rust crates, and the naming, strict JSON,
shared fixture, and cgo rules the code follows.
