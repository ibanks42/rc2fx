# rc2fx-dsp

The effects library used by the device service. It has no dependencies on an
audio server, HTTP, the filesystem, thread creation, or vendor FFI. The old C
DSP is no longer linked or called.

## Model

On the control thread, a validated `rc2fx_core::Target` is turned into a
`Prepared` snapshot at 48 kHz, along with a `Dsp` state and any suppression
worker endpoints. Processing borrows an immutable `Prepared` and a unique
mutable state. Neither processing entry point allocates, locks, logs, keeps
pointers to the snapshot, or calls the service. Objects are destroyed on the
control side once the callback has released them.

## Features

- High-pass, suppression boundary, EQ, compressor, gain, and limiter, in the
  configured order.
- EQ coefficients from `rc2fx-core`: APO and RLC modes, independent right
  bands, enabled and solo states, and up to 32 bands per side.
- Per-sample smoothing of coefficients and gains, with smoothing suspended
  once values settle. Filter history is kept for bands with the same ID, type,
  and mode.
- Structural EQ changes crossfade between two banks over 240 samples. Edits
  that arrive during a fade are coalesced.
- No live edit is a step in the output. Switching the high-pass, compressor,
  or limiter on or off fades the stage in or out over 480 samples (10 ms); a
  switched-off stage costs nothing once faded out. The compressor's and
  limiter's settings are smoothed like the gain. A re-enabled high-pass
  starts from rest; a re-enabled compressor or limiter resumes the detector
  history it had when switched off. An effect order change crossfades over
  240 samples from the old order, running on a copy of the stage state, to
  the new one.
- A processor that is being replaced hands off without a step: `hand_off`
  fades its output to silence, or to its dry signal, over 480 samples. Its
  replacement starts silent and fades in once its own delay has passed
  (`start_silent`), or fades in from the dry signal (`start_dry`).
- Linked stereo detection for the compressor and limiter, with bounded finite
  output and meters.
- Dry and wet paths share the worker's reported latency, so bypass doesn't
  shift timing. Bypass changes are smoothed.
- Any frame count, including zero-length blocks, and an in-place entry point.

Changing a target's channel count, kind, algorithm, or resolved worker
geometry, or the effect order while a suppression provider is allocated,
requires a new state. Incompatible snapshots and wrong buffer
lengths are rejected before any state or output changes. Other edits are
applied by passing a new `Prepared`.

## Suppression boundary

`Suppression` is a Rust worker endpoint. Its `process_sample` must not block
or allocate. Inference, queueing policy, fault telemetry, and joining the
worker on teardown are handled by the worker layer.

The delay reported at construction is fixed for the endpoint's lifetime,
including while disabled and after a worker failure. Delays above 1296 samples
are rejected to stay within the existing buffer bound.

Construction checks the provider's core algorithm and all four resolved
geometry fields: sample rate, frame, intrinsic delay, and worker allowance. Two
models with the same total delay but different algorithms or frame geometry
are treated as incompatible. `Prepared::new` always validates that the target
is admitted.

Once the callback has stopped, control code can call `Dsp::drain_suppression`
with one overall timeout. This calls each provider's control-side drain hook
and retires the DSP permanently, also on timeout. After that, both processing
entry points return `DspError::Retired` and leave the output alone. Draining
adds nothing to the callback: no drain, clock read, lock, or receipt
publication. DSPs that are dropped without draining follow the provider's own
`Drop` behavior.

A microphone state needs one real provider (suppression runs on mono
microphones only) even when suppression is off, because the microphone path
keeps the suppression delay.
Construction fails if a provider is missing or mismatched. The crate has no
inference code and no selectable fake denoiser. A test-only delay endpoint
exercises the contract. It says nothing about model quality or real-time
behavior. The real provider is the worker endpoint in `rc2fx-suppression`.

## Tests

`cargo test -p rc2fx-dsp --locked` runs the behavior and allocation tests.

On Linux x86_64 it also compares 43,280 samples per channel, in mono and
stereo, against frozen output from an independent scalar oracle. The expected
output is embedded as binary fixtures. The test runs no compiler, child
process, or legacy DSP source. On other Linux architectures the build
fails with a message that the fixtures are unqualified for that architecture,
until its numerical behavior has been checked.

The oracle scenarios cover every EQ type and mode, separate right-channel
settings, coefficient changes, reordering, solo and removal, rapid structural
changes, bypass, EQ and dynamics toggles, compensated 32-band cascades,
arbitrary chunk sizes, NaNs, infinities, and headroom guards.

The first native comparison was bit-identical for every F32 output sample, and
meter counts matched exactly. Floating-point meter values are compared with a
tolerance of `1e-10 * max(abs(expected), 1)`. The test keeps exact checks for
F32 output and integer counters. The output SHA-256 pins, record format,
scenario schedule, and provenance are in
[`tests/fixtures/scalar-linux-v1/README.md`](tests/fixtures/scalar-linux-v1/README.md).

The C oracle driver was removed after a fresh run matched the earlier oracle
output byte for byte. New expected values must come from independently
reviewed oracle output. The test must not regenerate them from the Rust
implementation.

These checks cover numerical behavior and ownership on a desktop machine.
ARM timing, callback deadlines, full service integration, and vendor models
need separate testing.
