# Noise suppression internals

## Models

| Model | Implementation | Frame |
| --- | --- | --- |
| DeepFilterNet3 (`deepfilternet3-h128`) | RC2FX's engine in `crates/rc2fx-dfn`. libDF's framing and features are reimplemented in Rust, and the networks run with NEON kernels and int8 weights. | 240 samples (one hop per call) |

The DeepFilterNet3 model has its GRUs narrowed to 128 units, about 600k
parameters. RC2FX retrained it on DNS-4 speech and noise, VCTK, room impulse
responses, and recordings of mouse clicks and keyboards, with those recordings
weighted to a quarter of the noise. The shipped weights are a zero-lookahead
(LA0) checkpoint. A one-step-lookahead (LA1) export is also embedded, and the
per-path `lookahead` setting selects between them. The
[weights chapter](../crates/rc2fx-dfn.md) covers the file format, provenance
and reference tests.

Weights are int8 with one scale per row. The mixer's four cores share one L2
cache, and streaming 1.2 MB of f16 weights every 5 ms caused JACK to drop
buffers. After publishing each result, the worker computes the next frame's
GRU recurrent products in small steps. This spreads the work so less of it
lands right after a frame's input arrives.

The training kit is kept outside the repository, in `target/dfn-training`
with data under `~/dfn`. Its `TRAINING.md` documents it.

Other models were tried and dropped, mostly because they could not keep up on
the Cortex-A53 cores without disturbing RØDE's audio. These included
full-size DeepFilterNet3, FastEnhancer, DPDFNet, DTLN and
several classic DSP suppressors.

## Worker thread

Suppression runs on a dedicated worker thread at SCHED_RR priority 5, pinned
to CPU 0. The audio callback copies each 112-sample cycle into a bounded
lock-free queue. The worker processes whole frames and returns each result a
fixed number of cycles later. That fixed offset is the worker lead.

If a result has not arrived in time, the output uses the dry signal with the
same delay. The audio stays continuous and that frame is unsuppressed.
Telemetry counts these as missed deadlines and fallbacks.

`PRODUCTION_WORKER` in `rc2fx-suppression` records the measurements behind
the priority and CPU choice. The launcher moves RØDE's CPU-0 housekeeping
threads to other cores, because the audio path waits on locks they hold.

## Delay

All figures are samples at 48 kHz.

| | Frame | Model delay | Worker lead | Suppression | With block shift |
| --- | ---: | ---: | ---: | ---: | ---: |
| DeepFilterNet3, lookahead 0 | 240 | 120 | 320 | 680 (14.17 ms) | 792 (16.5 ms) |
| DeepFilterNet3, lookahead 1 | 240 | 360 | 320 | 920 (19.17 ms) | 1032 (21.5 ms) |

The model delay is the synthesis overlap, plus one 240-sample hop at
lookahead 1. DeepFilterNet3's 960-sample analysis window and five causal deep-filter taps
add nothing further.

The block shift is one 112-sample cycle (2.33 ms). The helper thread applies
it to processed microphones (see
[Architecture](../developer/architecture.md#inside-the-audio-engine)).
Hardware and USB outputs are processed in place in the audio callback and do
not have it. A microphone suppressed with DeepFilterNet3 at lookahead 0
therefore reaches the headphones 792 samples (16.5 ms) later than it would
without RC2FX. These are RC2FX's digital delays only. RØDE's engine, JACK,
the converters and the route add their own.

The 320-sample lead is 6.67 ms. It is counted in samples against the mixer's
112-sample cycles and is not a wall-clock guarantee. A result that is still
late plays dry and is counted in telemetry.

The app shows the delay the mixer reports for each path. The other effects
add no delay beyond the block shift.

## One target at a time

Suppression can be enabled on one mono microphone or on Chat Mix Out
(`send-chat`, or `duo-send-chat` on a Duo), and only one of these at a time.
The rule is checked against the saved settings and counts bypassed and
deselected targets. To move suppression, disable it on the old target and
apply, then enable it on the new one.

## Chat Mix Out

On Chat, the native library captures only the microphone contributions to
the Chat send. It reads them from RØDE's routing functions after per-source
processing and fader gains, including main, custom, solo and mix-minus
routing. Four hash-pinned detours supply this microphone stem for the current
callback. RØDE's routing objects are read and left unmodified.

One mono model and voice gate process the stem. Smart Pads and other playback
skip both, keep their stereo image, and get the same delay. The two parts are
summed and the normal stereo output effects run on the result. Because of
this, suppression on Chat always runs first, whatever its position in
`order`. The other effects keep their relative order.

If the stem is missing or the layout is unknown, suppression on Chat fails
closed and the finished mix is passed through without denoising.

## Voice gate

The gate uses a voice score from 0 to 1 for each 5 ms frame: the model's
local SNR estimate, mapped from −15 dB (0) to +15 dB (1).

The gate opens when the score reaches the threshold and stays open until the
score drops below threshold − 0.15. It then holds for `hold` ms and closes to
−`reduction` dB with a `release` time constant. Opening uses `attack`. Gain
moves in dB. A frame with no model result opens the gate.

In simple mode one `strength` value from 0 (off) to 1 (strict) sets all of
these:

| | Strength 0 → 1 |
| --- | --- |
| Threshold | 0.3 → 0.7 |
| Reduction | 0 → 30 dB, as 30·strength^log₂1.5 (20 dB at 0.5) |
| Attack | 5 ms |
| Hold | 350 → 150 ms |
| Release | 300 → 90 ms (geometric) |

The [settings format](api.md#settings) lists the stored fields and ranges.

## Benchmarking on a mixer

```sh
just bench recording.wav
```

This runs every model on the mixer over a 48 kHz recording, using the
production scheduling policy, and copies the cleaned audio back for
listening. Flags select an unpaced run, a different policy, CPUs, priority or
weights file. See `cargo xtask help`.
