# rc2fx-core

This crate holds the mixer's copy of RC2FX's data models, covering settings, control API messages, and EQ coefficients. Wire structs, enums, and constants are generated for both Rust and Go (`desktop/core`) from `schema/core.go` via `./xtask schema`, and live Rust <-> Go integration tests keep both sides in sync. Everything here is plain Rust with no audio processing, networking, file I/O, or GUI code.

## Configuration and device state

`from_json` and `from_json_limited` (which the store uses for saved state) parse strictly, and `Config::validate` checks a schema 9 (`SCHEMA_VERSION`) configuration. Parsing rejects:

- duplicate JSON keys at any depth,
- unknown model fields,
- excessive nesting,
- oversized input.

Model validation catches nonfinite numbers, out-of-range controls, duplicate target or band IDs, invalid effect orders, and independent right-channel EQ on mono targets. `Config::validate_update` also prevents changing an existing target's kind or channel count.

Settings from any other schema version are rejected outright; there is no automatic migration.

`Config::default` returns the Pro II's target catalog with every stage turned off. `Suppression::admitted_geometry` gives DeepFilterNet3's frame size, intrinsic delay, and worker allowance for lookahead 0 or 1.

Meter payloads must include the current callback and worker counters, `latencySamples`, `workerFailed`, and a spectrum array (`[]` when no spectrum is present). Latency and processing activity come straight from those fields, independent of saved settings.

## EQ coefficients

The `eq` module designs the 48 kHz biquad for each supported band using the RLC BT Q mapping. The DSP crate then runs those coefficients directly.

## Tests

From the repository root:

```text
cargo test -p rc2fx-core
cargo clippy -p rc2fx-core --all-targets -- -D warnings
```

The integration tests verify live Rust <-> Go wire parity (via `desktop/internal/interop`), EQ/gate agreement, RLC coefficient vectors in `desktop/core/testdata/easyeffects`, and strict parsing/validation rules. Everything runs offline with no mixer required.
