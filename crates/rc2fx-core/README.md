# rc2fx-core

The mixer's copy of RC2FX's models: the settings, the control API's messages
and the EQ coefficients. The desktop's Go copy is `desktop/core`, which also
holds the editor logic (local edits, EQ response curves, EasyEffects and APO
import), and `fixtures/core` keeps the two in step. The crate is plain Rust with
no audio processing, network access, file I/O or GUI code.

## Configuration and device state

`Config::from_json` and `DeviceState::from_json` parse schema 7 and validate
configuration values. Parsing rejects:

- duplicate JSON keys at any depth,
- unknown model fields,
- excessive nesting,
- oversized input.

Model validation rejects nonfinite numbers, out-of-range controls, duplicate
target or band IDs, invalid effect order, and independent right-channel EQ on
mono targets. `Config::validate_update` also refuses to change an existing
target's kind or channel count.

Older settings are upgraded on the raw JSON before strict parsing, one step
per schema change (the `schema` module). Schemas 5 and 6 can be upgraded.
Anything older is rejected.

`Config::default` provides logical editing targets with every stage off.
`SuppressionAlgorithm` accepts DeepFilterNet3 (`deepfilternet3-h128`) and
gives its frame size, intrinsic delay and worker allowance.

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

The integration tests cover the shared accept and reject cases in
`fixtures/core`, the RLC coefficient vectors in `fixtures/easyeffects`, and
strict parsing and validation. The tests run offline, without a mixer.
