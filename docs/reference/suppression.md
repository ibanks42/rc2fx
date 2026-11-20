# Noise suppression internals

## Models

| Model | Implementation | Frame |
| --- | --- | --- |
| DeepFilterNet3 (`deepfilternet3-h128`) | RC2FX's custom engine in `crates/rc2fx-dfn`. Reimplements libDF's framing and feature extraction in Rust, running the networks with NEON kernels and int8 weights. | 240 samples (one hop per call) |

We narrowed DeepFilterNet3's GRUs down to 128 units (~600k parameters) and retrained it on DNS-4 speech and noise, VCTK, room impulse responses, and mouse-click/keyboard recordings (weighted to a quarter of the noise mix). The default weights are a zero-lookahead (LA0) checkpoint. A one-step-lookahead (LA1) checkpoint is embedded alongside it, controlled by the per-path `lookahead` setting. See the [weights chapter](../crates/rc2fx-dfn.md) for the file format, provenance, and reference tests.

Weights are quantized to int8 with one scale factor per row. Because all four Cortex-A53 cores share a single L2 cache, streaming 1.2 MB of f16 weights every 5 ms caused JACK xruns. After publishing each frame's output, the worker precomputes the next frame's GRU recurrent products in small chunks, spreading the work out so less of it piles up right when the next input frame arrives.

The training kit lives outside this repo in `target/dfn-training`, with datasets under `~/dfn`. Check its `TRAINING.md` for details.

We tested and dropped several other models (full-size DeepFilterNet3, FastEnhancer, DPDFNet, DTLN, and a handful of classic DSP suppressors), mostly because they couldn't keep up on the Cortex-A53 cores without stepping on RØDE's audio.

## Worker thread

Suppression runs on a dedicated worker thread pinned to CPU 0 at `SCHED_RR` priority 5. Each 112-sample JACK callback copies audio into a bounded lock-free queue; the worker processes full 240-sample frames and hands results back a fixed number of cycles later (the worker lead).

If a result misses its deadline, RC2FX outputs the dry signal delayed by the same amount. Audio stays glitch-free, just unsuppressed for that frame, and telemetry bumps the missed-deadline and fallback counters.

`PRODUCTION_WORKER` in `rc2fx-suppression` documents the measurements behind the CPU and priority choices. The launcher also moves RØDE's CPU-0 housekeeping threads onto other cores because the audio path waits on locks they hold.

## Delay

All numbers below are in samples at 48 kHz.

| | Frame | Model delay | Worker lead | Suppression | With block shift |
| --- | ---: | ---: | ---: | ---: | ---: |
| DeepFilterNet3, lookahead 0 | 240 | 120 | 320 | 680 (14.17 ms) | 792 (16.5 ms) |
| DeepFilterNet3, lookahead 1 | 240 | 360 | 320 | 920 (19.17 ms) | 1032 (21.5 ms) |

Model delay is just the synthesis overlap, plus one 240-sample hop when lookahead is set to 1. DeepFilterNet3's 960-sample analysis window and five causal deep-filter taps don't add any extra delay.

The block shift is one 112-sample cycle (2.33 ms), which the helper thread adds to processed microphones (see [Architecture](../developer/architecture.md#inside-the-audio-engine)). Hardware and USB outputs run in place inside the audio callback, so they skip the block shift. That puts a microphone with DeepFilterNet3 at lookahead 0 at 792 samples (16.5 ms) of extra delay in the headphones compared to stock. These figures only count RC2FX's digital delay; RØDE's engine, JACK, the converters, and the physical route add their own on top.

The 320-sample worker lead (6.67 ms) is counted in samples against the mixer's 112-sample cycles, not as a hard wall-clock guarantee. If a frame still comes back late, it plays dry and shows up in telemetry.

The desktop app displays the exact delay reported by the mixer for each path. All other effects add zero delay beyond the block shift.

## One target at a time

You can only enable suppression on a single target at once: either one mono microphone or Chat Mix Out (`send-chat`, or `duo-send-chat` on a Duo). This check runs across all saved targets, including bypassed and deselected ones. To move suppression somewhere else, turn it off on the old target and apply first, then turn it on for the new one.

## Chat Mix Out

On Chat Mix Out, the native library taps only the microphone feeds going into the Chat send. It reads them out of RØDE's routing functions after per-source processing and fader gains (covering main, custom, solo, and mix-minus routing), using four hash-pinned detours that leave RØDE's routing objects untouched.

A single mono denoiser and voice gate process that microphone stem. SMART Pads and other playback skip both, stay stereo, and get matched delay. RC2FX then sums the two halves back together and runs the regular stereo output effects on the combined signal. Because of that structure, suppression on Chat always runs first no matter where it sits in `order`; the rest of the effects keep their relative order.

If the microphone stem is missing or the layout isn't recognized, Chat suppression fails closed and passes the finished mix through dry.

## Voice gate

The gate tracks a 0-to-1 voice score for every 5 ms frame, derived from the model's local SNR estimate mapped from −15 dB (0) to +15 dB (1).

It opens when the score hits `threshold` and stays open until the score falls below `threshold − 0.15`. Once it drops, it waits `hold` ms, then closes down to `−reduction` dB with a `release` time constant. Opening uses `attack`, and gain ramps in dB. If a frame has no model result, the gate opens.

In simple mode, a single `strength` control from 0 (off) to 1 (strict) drives all five parameters:

| | Strength 0 → 1 |
| --- | --- |
| Threshold | 0.3 → 0.7 |
| Reduction | 0 → 30 dB, as 30·strength^log₂1.5 (20 dB at 0.5) |
| Attack | 5 ms |
| Hold | 350 → 150 ms |
| Release | 300 → 90 ms (geometric) |

See the [settings format](api.md#settings) for the exact stored fields and valid ranges.

## Benchmarking on a mixer

```sh
just bench recording.wav
```

This runs every model on the mixer against a 48 kHz recording under the production scheduling policy and pulls the processed audio back so you can listen to it. Flags let you run unpaced or override the scheduling policy, CPUs, priority, or weights file; check `./xtask help` for the full list.
