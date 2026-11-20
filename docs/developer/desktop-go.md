# The desktop app in Go

The desktop app is one Go module, `github.com/ibanks42/rc2fx/desktop`, in
`desktop/`. Its UI is GPUI-kit through the gpuikit Go bindings (package
`gpuikit.dev/gk`). The mixer side stays Rust under `crates/`. Some code exists
on both sides, because the app and the mixer must agree on it; this page
describes how the Go packages map to the Rust crates and the rules that keep
the two copies in step.

## Packages

| Go package (under `desktop/`) | Rust counterpart | Notes |
| --- | --- | --- |
| `core` | `crates/rc2fx-core` | The desktop's copy of the settings schema, presets, EQ math, and strict JSON. The Rust crate is the mixer's. |
| `usbproto` | `crates/rc2fx-usb` | HID report framing and the control protocol constants |
| `delta` | `crates/rc2fx-delta` | The encoder the app uses, and a decoder for tests |
| `firmware` | `crates/rc2fx-firmware` | Models, profiles, and the offline checks the installer needs |
| `installer` (and `installer/platform`, `installer/embed`, `installer/control`, and others) | none | Installer engine: firmware download and patching, USB update mode, the USB control client |
| `ext2fs` | none | libext2fs from the vendored e2fsprogs, through cgo, for Repair mixer. Without cgo it builds as a stub. |
| `transport` | none | The app's USB 1 control client, and the simulated mixer behind the `mockmixer` tag |
| `app` (and subpackages), `cmd/rc2fx` | none | The UI. The only packages that import `gpuikit.dev/gk`. |
| `cmd/usb-probe` | none | A probe of the controller API over USB 1 |
| `cmd/xtask`, `internal/xtask/...` | none | The repository's development and release tool, run by `./xtask` |

The files the packages ship are in `desktop/packaging`: the AppImage launcher,
the desktop entry, the icons, and the macOS entitlements. The scripts that run
on the mixer are in `mixer/scripts`.

## Shared fixtures

Anything both sides must agree on (configuration JSON, presets, the USB
framing, delta payloads, release metadata, the image check list, the
payload's file lists, the gate's shape) has cases in `fixtures/` at the
repository root. A Go test and a Rust test read the same files: the Rust side
in `crates/*/tests/shared_fixtures.rs`,
`crates/rc2fx-core/tests/image_check_fixtures.rs`,
`crates/rc2fx-core/tests/gate_fixtures.rs`,
`crates/rc2fx-device/tests/xtask_tuf_fixtures.rs`, and the payload list test in
`crates/rc2fx-device/src/mixer_update/tests.rs`, the Go side in the matching
package's tests. The gate's shape is exact except where a value takes a power:
Go's `math.Pow` is not glibc's `pow`, so those may be up to two ulps apart. Each side checks its own direction: Rust checks what the mixer
writes and the verdicts on what it reads, and Go does the same for the
desktop, so a case only the other side reads is left to that side. When you
change shared behavior, change both copies and add the case to `fixtures/`, so
the mixer's Rust and the desktop's Go cannot drift apart.

## Naming

Go names follow fixed rules from the Rust names, so a reader can find one copy
from the other:

- A Rust item `rc2fx_<crate>::<module>::<name>` becomes `<package>.<GoName>`.
  Modules inside a crate flatten into the package unless the table above gives
  a subpackage.
- Functions and types keep their names in exported Go CamelCase:
  `compare_versions` becomes `CompareVersions`, `MixerRelease::parse`
  `ParseMixerRelease`, `Model::from_board` `ModelFromBoard`, and `valid_id`
  `ValidID`. Initialisms follow Go style (`ID`, `USB`, `JSON`, `HTTP`, `SSH`,
  `EQ`, `HPF`, `TUF`, `URL`).
- Constants: `MIXER_PROTOCOL_VERSION` becomes `MixerProtocolVersion`.
- A fieldless enum is a named integer or string type with one constant per
  variant, `<Type><Variant>`: `Model::ProIi` is `firmware.ModelProIi`. An enum
  with data is a sealed interface or a struct with a `Kind` field, whichever
  reads better where it is used.
- `Result<T, E>` is `(T, error)`. `Option<T>` is a pointer or `(T, bool)`.

## Behavior

- The two copies behave the same, including every limit, bound, ordering rule,
  and message the user sees.
- JSON from outside the process is read as strictly as `rc2fx-core`'s strict
  JSON and serde's `deny_unknown_fields`: unknown fields, missing required
  fields, duplicate keys, out-of-range integers, non-finite numbers, and too
  deep or too large documents are refused. Go's `encoding/json` accepts most
  of those, so use the strict decoder in `core` (`core.DecodeStrict` and
  `core.DecodeStrictLimited`) everywhere such JSON is read.
- JSON the app writes is byte-identical to what the Rust writes wherever the
  mixer or a file on disk reads it: field order, number formatting, and
  escaping.
- Dependencies: the standard library first. The module uses
  `github.com/klauspost/compress` (zstd and gzip), `golang.org/x/crypto`,
  `golang.org/x/sys`, `github.com/theupdateframework/go-tuf/v2`,
  `github.com/sigstore/sigstore`, and `howett.net/plist`. Adding another needs a
  reason.

## cgo and build tags

Everything builds without cgo except:

- `app` and `cmd/rc2fx`, which link the gpuikit library;
- `app/selfupdate/vpkc`, Velopack's C library, with the `velopack` tag only;
- `ext2fs`, which compiles the vendored libext2fs (a stub without cgo);
- the macOS platform code in `installer/platform`, which needs IOKit and
  DiskArbitration (also with a stub without cgo).

`./xtask` itself always builds with `CGO_ENABLED=0`, so it starts quickly and
needs no C compiler; the commands that build the app turn cgo on for that
build. Platform code uses file suffixes and build constraints (`_linux.go`,
`_windows.go`, `_darwin.go`) and must compile for all three:
`GOOS=windows` and `GOOS=darwin` with `CGO_ENABLED=0` check the files that
need no cgo, on every package but the UI (which needs gpuikit, and so cgo):

```sh
nocgo=$(go list ./... | grep -v -e '/app$' -e '/app/uitest$' -e '/cmd/rc2fx$')
GOOS=windows CGO_ENABLED=0 go vet $nocgo
GOOS=darwin CGO_ENABLED=0 go vet $nocgo
```

| Tag | Effect | In the Rust app (removed in 89296ea) |
| --- | --- | --- |
| `rc2fxdev` | Development builds: mixer releases from a local folder (`RC2FX_DEV_MIXER_REPOSITORY`), the telemetry preview (`rc2fx --telemetry-preview`), the flash autopilot, flow fixtures, and the storage fix's debug log | a debug build (`debug_assertions`) |
| `mockmixer` | The simulated mixer in place of USB 1 | the `mock-mixer` feature |
| `embedded` | The mixer payload staged by `cmd/stage-payload`, compiled in | the `embedded` feature |
| `velopack` | Self-update through Velopack's C library. Without it the app cannot update itself, ignores Velopack's install hooks, and accepts `RC2FX_DEV_UPDATE`. | always on |

`./xtask app` builds with `rc2fxdev` unless given `--release`, and
packaging builds with `velopack` (and `embedded` with `--embedded`) and refuses
an executable built with `mockmixer` or `rc2fxdev`.

## Tests

From `desktop/`, the packages without a UI test on any machine, without the
gpuikit library:

```sh
CGO_ENABLED=0 go vet $(go list ./... | grep -v -e '/app$' -e '/app/uitest$' -e '/cmd/rc2fx$')
go test $(go list ./... | grep -v -e '/app$' -e '/app/uitest$' -e '/cmd/rc2fx$')
go test -tags mockmixer ./transport/...
```

With the gpuikit download in `third_party/gpuikit` and
`CGO_LDFLAGS=-L$PWD/third_party/gpuikit/lib`, `go test ./...` covers the UI
too. `app/uitest` runs the offscreen UI tests when `RC2FX_GPUIKIT_DIAG` names a
folder holding gpuikit's diagnostic library, and skips them otherwise. The
downloads have none: build it from the bindings repository (`make lib` there,
then its `lib/diag`). `just test`
runs all of this, then, on Linux, the Rust workspace tests.
