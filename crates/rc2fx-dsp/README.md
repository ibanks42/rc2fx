# rc2fx-dsp

The audio effects library used by the device service. It has no dependencies on an audio server, HTTP, the filesystem, thread spawning, or vendor FFI, and the legacy C DSP is no longer linked or called.

## Model

On the control thread, a validated `rc2fx_core::Target` is compiled into a 48 kHz `Prepared` snapshot alongside a `Dsp` state and any suppression worker endpoints. Audio processing borrows an immutable `Prepared` and a unique mutable `Dsp`. Neither processing entry point allocates, locks, logs, retains pointers to the snapshot, or calls back into the service. Once the audio callback is done with old objects, the control side drops them.

## Features

- High-pass, suppression boundary, EQ, compressor, gain, and limiter, executed in whatever order is configured.
- EQ coefficients from `rc2fx-core` supporting APO and RLC modes, independent right-channel bands, enable/solo states, and up to 32 bands per side.
- Per-sample smoothing for coefficients and gains that automatically stops once values settle. Bands that keep the same ID, type, and mode keep their filter history.
- Structural EQ edits crossfade between two filter banks over 240 samples, coalescing any additional edits that arrive mid-fade.
- Live edits never produce a step in the audio. Toggling the high-pass, compressor, or limiter fades the stage in or out over 480 samples (10 ms), and a bypassed stage does no work once faded out. Compressor and limiter parameters smooth just like gain. Re-enabling the high-pass starts from rest, whereas a re-enabled compressor or limiter picks up from the detector history it had when turned off. Changing effect order crossfades over 240 samples from the old chain (running on a copy of the stage state) to the new one.
- Replacing a processor is also click-free: `hand_off` fades the old processor to silence or to its dry signal over 480 samples, while the replacement either starts silent and fades in once its delay has elapsed (`start_silent`) or fades in from dry (`start_dry`).
- Linked stereo detection for the compressor and limiter, with bounded finite output and meters.
- Dry and wet paths share the worker's reported latency so toggling bypass never shifts timing; bypass transitions themselves are smoothed.
- Arbitrary frame counts (including zero-length blocks) plus an in-place processing entry point.

You only need a fresh `Dsp` state when changing a target's channel count, kind, algorithm, or resolved worker geometry, or when reordering effects while a suppression provider is attached. Incompatible snapshots and mismatched buffer lengths are rejected before any state or output is touched. Everything else can be updated live by passing a new `Prepared`.

## Suppression boundary

`Suppression` defines the Rust worker endpoint interface. Its `process_sample` implementation must never block or allocate; inference, queueing policy, fault telemetry, and worker teardown all live in the worker layer.

The delay reported at construction stays fixed for the endpoint's entire lifetime, even when disabled or after a worker failure. Delays over 1296 samples are rejected to stay within the buffer limit.

Construction verifies the provider's core algorithm and all four resolved geometry fields (sample rate, frame size, intrinsic delay, and worker allowance). Two models with the same total delay but different algorithms or frame geometry are treated as incompatible. `Prepared::new` always verifies that the target is admitted.

After the audio callback has stopped, control code can call `Dsp::drain_suppression` with a single overall timeout. That invokes each provider's control-side drain hook and permanently retires the DSP, even if the drain times out. Any subsequent call to either processing entry point returns `DspError::Retired` without touching the audio buffers. Draining adds zero overhead to the callback path (no drain checks, clock reads, locks, or receipt publishing). If a `Dsp` is dropped without draining, teardown falls back to the provider's `Drop` implementation.

Microphone states always require a real suppression provider (suppression only runs on mono mics) even when suppression is switched off, because the mic path holds the suppression delay constant. Missing or mismatched providers fail construction. This crate contains no inference code and no fake denoiser, only a test-only delay endpoint to exercise the contract, which says nothing about model quality or real-time performance. The real worker endpoint lives in `rc2fx-suppression`.

## Tests

`cargo test -p rc2fx-dsp --locked` runs the behavior and zero-allocation tests.

On Linux x86_64 it also checks 43,280 samples per channel (mono and stereo) against frozen binary fixtures from an independent scalar oracle. The test embeds those fixtures directly and never invokes a C compiler, child process, or legacy DSP source. On other Linux architectures the build intentionally fails with a message noting that the fixtures haven't been qualified for that architecture yet.

The oracle suite covers every EQ type and mode, independent right-channel bands, coefficient updates, band reordering, soloing and removal, rapid structural edits, bypass, EQ and dynamics toggles, compensated 32-band cascades, arbitrary chunk sizes, NaNs, infinities, and headroom guards.

The initial native comparison was bit-identical across every `f32` output sample with exact meter counts. Floating-point meters are checked within `1e-10 * max(abs(expected), 1)`, while `f32` audio samples and integer counters remain exact. See [`tests/fixtures/scalar-linux-v1/README.md`](tests/fixtures/scalar-linux-v1/README.md) for SHA-256 pins, record layout, scenario schedule, and provenance.

The C oracle driver was removed once a fresh build matched the original oracle output byte for byte. Any future fixture updates must come from an independently reviewed oracle. Never regenerate them from the Rust implementation.

Keep in mind these tests only verify numerical behavior and ownership on a desktop host. ARM timing, callback deadlines, full service integration, and vendor models are tested separately.
