# rc2fx-core

Shared models and editor logic used by the desktop app and the transport. The
crate is plain Rust with no audio processing, network access, file I/O or GUI
code.

## Configuration and session state

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

`Session` tracks local edits, the settings saved on the device, and the
observed engine state as three separate values.

- `receive` keeps local edits when the device revision changes. The caller then
  has to choose `reload_saved` or `rebase_edits`.
- `begin_apply` captures a request. `acknowledge` checks the response and keeps
  any edits made while the request was in flight.
- The state counts as applied when the revisions match, the engine is running
  with `audioAdvancing`, and there are no unsaved local changes.
- Disconnecting clears the connected flag and keeps the edits.

Meter types require the current callback and worker counters,
`latencySamples`, `workerFailed`, and a spectrum array (`[]` when there is no
spectrum). Latency and processing activity are read from these fields, and
saved settings play no part in them.

## EQ response

The `eq` module evaluates the supported 48 kHz biquads and the optional input
and output gains. It handles independent channels, bypass, mute and solo, and
the RLC BT Q mapping. APO DR slope metadata is preserved, but each filter is
evaluated as a single biquad. RLC BT supports slope 1.

`checked_response_db` returns validation errors. `response_db` returns a flat
curve for a transient invalid value while editing. Both compute the
theoretical response of the settings. They don't measure audio.

## EasyEffects interchange

`easyeffects::import` supports a tested subset of the IIR equalizer, in
current indexed `equalizer#N` presets and in the `equalizer` preset form. It
keeps independent stereo channels. It returns a selection result for the
caller to resolve when a preset contains several equalizers, or when a preset
with different left and right channels is imported into a mono target. Other
effects in the preset are counted and skipped.

Supported filters are Off, Bell, low and high shelf, low and high pass, and
Notch, in APO DR or RLC BT mode. Import fails with an error on unsupported
processing modes or filter types, decramping, nonzero balance or pitch, and
unknown equalizer fields. Only the EQ is imported. Other processing in an
EasyEffects preset is not reproduced.

`easyeffects::export` writes a complete equalizer preset. An empty EQ is
written as one neutral Off band. If independent channels have different band
counts, the shorter one is padded with neutral bands. Import removes that
padding and keeps any Off bands that carry real settings.

## Equalizer APO / AutoEQ text

`apo::import` reads the parametric text format that AutoEQ also produces for
EasyEffects' APO importer. It keeps the preamp, filter order, enabled state,
frequency, gain and Q.

Supported filters: PK/PEQ, LS/LSC and HS/HSC with a centre frequency, HP/HPQ,
LP/LPQ, and NO with Q. The imported curve is applied to both sides of a linked
stereo target. At most 32 filters are accepted.

Unsupported commands, slope and BW variants, and values the engine would clamp
fail with the line number. `Include` lines and expressions are not evaluated.
The syntax follows the
[Equalizer APO configuration reference](https://sourceforge.net/p/equalizerapo/wiki/Configuration%20reference/).
The 10-band and 15-band presets in `fixtures/apo` are test fixtures.

## Tests

From the repository root:

```text
cargo test -p rc2fx-core
cargo clippy -p rc2fx-core --all-targets -- -D warnings
```

The integration tests cover the six preset fixtures in `fixtures/easyeffects`,
the shared independent RLC coefficient vectors, strict parsing and
validation, channel and solo behaviour, import choices, and conflicting
concurrent edits. Fixture provenance and the community preset's licence are
recorded in that directory. The tests run offline, without EasyEffects or a
mixer.
