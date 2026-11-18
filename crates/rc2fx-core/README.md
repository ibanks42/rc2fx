# rc2fx-core

The mixer's copy of RC2FX's models: the settings, the control API's messages,
and the EQ coefficients. The desktop's Go copy is `desktop/core`, which also
holds the editor logic (local edits, EQ response curves, EasyEffects and APO
import), and `fixtures/core` keeps the two in step. The crate is plain Rust with
no audio processing, network access, file I/O, or GUI code.

## Configuration and device state

`from_json` (and `from_json_limited`, which the store uses for saved state)
parses strictly, and `Config::validate` checks a schema 9 (`SCHEMA_VERSION`)
configuration. Parsing rejects:

- duplicate JSON keys at any depth,
- unknown model fields,
- excessive nesting,
- oversized input.

Model validation rejects nonfinite numbers, out-of-range controls, duplicate
target or band IDs, invalid effect order, and independent right-channel EQ on
mono targets. `Config::validate_update` also refuses to change an existing
target's kind or channel count.

Settings in any other schema are rejected; nothing is upgraded.

`Config::default` provides the Pro II's target catalog with every stage off.
`Suppression::admitted_geometry` gives DeepFilterNet3's frame size, intrinsic
delay, and worker allowance for a lookahead of 0 or 1.

Meter types require the current callback and worker counters,
`latencySamples`, `workerFailed`, and a spectrum array (`[]` when there is no
spectrum). Latency and processing activity are read from these fields, and
saved settings play no part in them.

## EQ coefficients

The `eq` module designs each supported band's 48 kHz biquad, with the RLC BT Q
mapping. The DSP crate runs these coefficients.

## Tests

From the repository root:

```text
cargo test -p rc2fx-core
cargo clippy -p rc2fx-core --all-targets -- -D warnings
```

The integration tests cover the mixer's direction of the shared cases in
`fixtures/core` (the encoding of every document, and the verdicts on the
documents the mixer reads), the RLC coefficient vectors in `fixtures/easyeffects`, and
strict parsing and validation. The tests run offline, without a mixer.
