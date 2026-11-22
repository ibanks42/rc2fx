# The desktop app in Go

The desktop app is a single Go module (`github.com/ibanks42/rc2fx/desktop`)
under `desktop/`. Its UI is built on GPUI-kit through the gpuikit Go bindings
(package `gpuikit.dev/gk`). The mixer side stays in Rust under `crates/`.
Because the desktop app and the mixer have to agree on schemas, framing, and
validation, some logic is mirrored on both sides. This page explains how the Go
packages map to the Rust crates and the conventions that keep both copies in
sync.

## Packages

| Go package (under `desktop/`) | Rust counterpart | Notes |
| --- | --- | --- |
| `core` | `crates/rc2fx-core` | Desktop copy of the settings schema, presets, EQ math, and strict JSON. The Rust crate is the mixer's copy. |
| `usbproto` | `crates/rc2fx-usb` | HID report framing and control protocol constants |
| `delta` | `crates/rc2fx-delta` | Encoder used by the app, plus a decoder for tests |
| `firmware` | `crates/rc2fx-firmware` | Models, profiles, and offline checks used by the installer |
| `installer` (and `installer/platform`, `installer/embed`, `installer/control`, and others) | none | Installer engine: firmware downloads and patching, USB update mode, and the USB control client |
| `ext2fs` | none | Vendored libext2fs from e2fsprogs via cgo, used by Repair mixer. Builds as a stub without cgo. |
| `transport` | none | USB 1 control client for the app, plus the simulated mixer behind the `mockmixer` tag |
| `app` (and subpackages), `cmd/rc2fx` | none | The UI. The only packages that import `gpuikit.dev/gk`. |
| `cmd/usb-probe` | none | Probe tool for the controller API over USB 1 |
| `cmd/xtask`, `internal/xtask/...` | none | Development and release automation invoked via `./xtask` |

Packaging assets live in `desktop/packaging`: the AppImage launcher, desktop
entry, icons, and macOS entitlements. Scripts that run on the mixer live in
`mixer/scripts`.

## Shared fixtures

Everything both sides must agree on (configuration JSON, presets, USB framing,
delta payloads, release metadata, image check lists, payload file lists, and
gate response shapes) has test cases in `fixtures/` at the repository root.
Both Go and Rust tests read the exact same files: the Rust side in
`crates/*/tests/shared_fixtures.rs`,
`crates/rc2fx-core/tests/image_check_fixtures.rs`,
`crates/rc2fx-core/tests/gate_fixtures.rs`,
`crates/rc2fx-device/tests/xtask_tuf_fixtures.rs`, and the payload list test in
`crates/rc2fx-device/src/mixer_update/tests.rs`, and the Go side in each
matching package's tests.

The gate shape matches bit-for-bit except where a value uses a power function:
Go's `math.Pow` differs slightly from glibc's `pow`, so those values are
allowed up to two ulps of difference. Each side verifies its own direction:
Rust validates what the mixer writes and its verdicts on what it reads, while
Go does the same for the desktop app, leaving one-sided cases to the side that
reads them. Whenever you change shared behavior, update both implementations
and add a case to `fixtures/` so Rust and Go cannot drift apart.

## Naming

Go identifiers follow a consistent mapping from their Rust counterparts so you
can jump between the two easily:

- A Rust item `rc2fx_<crate>::<module>::<name>` maps to `<package>.<GoName>`.
  Submodules inside a crate flatten into the Go package unless the table above
  calls out a subpackage.
- Functions and types keep their names in exported Go CamelCase:
  `compare_versions` becomes `CompareVersions`, `MixerRelease::parse` becomes
  `ParseMixerRelease`, `Model::from_board` becomes `ModelFromBoard`, and
  `valid_id` becomes `ValidID`. Initialisms follow Go conventions (`ID`, `USB`,
  `JSON`, `HTTP`, `SSH`, `EQ`, `HPF`, `TUF`, `URL`).
- Constants map similarly: `MIXER_PROTOCOL_VERSION` becomes
  `MixerProtocolVersion`.
- Fieldless enums become a named integer or string type with constants named
  `<Type><Variant>` (for example, `Model::ProIi` is `firmware.ModelProIi`).
  Enums carrying data become either a sealed interface or a struct with a
  `Kind` field, depending on which reads cleaner at call sites.
- `Result<T, E>` maps to `(T, error)`. `Option<T>` maps to a pointer or
  `(T, bool)`.

## Behavior

- Both copies must behave identically, including limits, bounds, and ordering
  rules. Both copies must accept and reject the same documents, while the
  wording of an error belongs to each side.
- Any JSON coming from outside the process must be decoded as strictly as
  `rc2fx-core`'s strict JSON and serde's `deny_unknown_fields`: reject unknown
  fields, missing required fields, duplicate keys, out-of-range integers,
  non-finite numbers, and overly deep or large payloads. Standard
  `encoding/json` lets most of those slip through, so always use
  `core.DecodeStrict` or `core.DecodeStrictLimited`.
- JSON written by the app must decode on the mixer to the same values. Field
  order and whitespace are not part of the contract; Rust owns the canonical
  byte form, which the shared fixtures in `fixtures/core` pin on the Rust side.
  Number text is part of the contract: the app must write every number with the
  same characters as Rust. The mixer rejects `1.0` where it expects an integer,
  and its `serde_json` is built without the `float_roundtrip` feature. That
  build converts the leading digits and scales by a power-of-ten table, so the
  result is not always the nearest double (it can be one or two units in the
  last place away), and two spellings of the same number can read as different
  doubles. For the same reason the app keeps a JSON tree that preserves numbers
  exactly and reads number text the way the mixer does: do not swap the number
  parser or the float writer in `desktop/internal/sjson` for the standard
  library's.
- Keep dependencies minimal and reach for the standard library first. The
  module currently uses `github.com/klauspost/compress` (zstd and gzip),
  `golang.org/x/crypto`, `golang.org/x/sys`,
  `github.com/theupdateframework/go-tuf/v2`, `github.com/sigstore/sigstore`,
  and `howett.net/plist`. Adding a new dependency requires a clear reason.

## cgo and build tags

Everything in the module builds without cgo except:

- `app` and `cmd/rc2fx`, which link the gpuikit static library;
- `app/selfupdate/vpkc`, Velopack's C library (only with the `velopack` tag);
- `ext2fs`, which compiles vendored libext2fs (falls back to a stub without
  cgo);
- macOS platform code in `installer/platform`, which links IOKit and
  DiskArbitration (also has a stub without cgo).

`./xtask` always builds with `CGO_ENABLED=0` so it starts fast and doesn't need
a C compiler; subcommands that compile the app re-enable cgo for that build.
OS-specific code uses file suffixes and build constraints (`_linux.go`,
`_windows.go`, `_darwin.go`) and must compile across all three targets. You can
cross-check all non-UI packages with `CGO_ENABLED=0`:

```sh
nocgo=$(go list ./... | grep -v -e '/app$' -e '/app/uitest$' -e '/cmd/rc2fx$')
GOOS=windows CGO_ENABLED=0 go vet $nocgo
GOOS=darwin CGO_ENABLED=0 go vet $nocgo
```

| Tag | Effect | In the Rust app (removed in 89296ea) |
| --- | --- | --- |
| `rc2fxdev` | Development builds: local mixer release directory (`RC2FX_DEV_MIXER_REPOSITORY`), telemetry preview (`rc2fx --telemetry-preview`), flash autopilot, flow fixtures, and the storage fix debug log | a debug build (`debug_assertions`) |
| `mockmixer` | In-process simulated mixer in place of USB 1 | the `mock-mixer` feature |
| `embedded` | Compiles in the mixer payload staged by `cmd/stage-payload` | the `embedded` feature |
| `velopack` | Enables self-update via Velopack's C library. Without it, the app cannot self-update, ignores Velopack install hooks, and accepts `RC2FX_DEV_UPDATE`. | always on |

`./xtask app` enables `rc2fxdev` unless `--release` is passed. Packaging always
enables `velopack` (plus `embedded` when `--embedded` is given) and refuses any
binary built with `mockmixer` or `rc2fxdev`.

## Tests

From `desktop/`, all non-UI packages can be vetted and tested on any machine
without gpuikit:

```sh
CGO_ENABLED=0 go vet $(go list ./... | grep -v -e '/app$' -e '/app/uitest$' -e '/cmd/rc2fx$')
go test $(go list ./... | grep -v -e '/app$' -e '/app/uitest$' -e '/cmd/rc2fx$')
go test -tags mockmixer ./transport/...
```

With gpuikit unpacked in `third_party/gpuikit` and
`CGO_LDFLAGS=-L$PWD/third_party/gpuikit/lib`, `go test ./...` covers the UI
packages as well. `app/uitest` runs offscreen UI tests when
`RC2FX_GPUIKIT_DIAG` points to a directory containing gpuikit's diagnostic
library, and skips them otherwise. Prebuilt downloads don't include that
library; build it from the bindings repository (`make lib`, then its
`lib/diag`). `just test` runs the full Go suite followed by the Rust workspace
tests on Linux.
