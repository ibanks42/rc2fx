# RNNoise 0.1.1 C reference

Frozen output from the original C build of RNNoise 0.1.1 (xiph/rnnoise
`6cbfd53e`). `tests/vendor.rs` compares RC2FX's RNNoise with it. RC2FX runs
RNNoise through [nnnoiseless](https://github.com/jneem/nnnoiseless), a Rust
port that carries RNNoise 0.1.1's weights byte for byte. These tests don't
build or run any C.

| File | Contents |
| --- | --- |
| `synthetic.f32` | The input: 40,013 mono samples at 48 kHz, little-endian f32 |
| `c/model2-resetfalse.f32` | The C build's output, uninterrupted |
| `c/model2-resettrue.f32` | The C build's output, reset before zero-based frame 72, 145, … |
| `SHA256SUMS` | Their SHA-256, checked on every read |
| `provenance.json`, `TOOLCHAIN.txt` | How the C outputs were produced (executables, compilers, flags) |

Each run starts from a fresh model. The final partial frame is padded with
zeros for one model call, and only the original 40,013 samples are kept. The C
build produced identical bytes on x86-64 and on Cortex-A53.

## Input

`synthetic.f32` is generated with unsigned xorshift32 (state `0x91e10da5`,
shifts `<<13`, `>>>17`, `<<5`). Each sample is
`((state & 65535) - 32768) / 32768`, with these overrides applied in order:

- Samples 0–511: zero.
- Samples 512–1023: `(sample_index - 768) / 256`.
- Samples 1024–1535: +1 at odd indices, −1 at even indices.
- Otherwise, indices with `index % 4096 < 64` are zero, and indices with
  `index % 4096 == 64` are +0.75.
- Otherwise, indices 35000–36499 are the default value multiplied by four,
  which exercises the clamp to ±1.

## Tolerance

The input is loud, clipped and noise-like. nnnoiseless's FFT and summation
order differ from the C build's, and on this input the floating-point
differences sometimes move RNNoise's pitch search to a different, equally
good period. Individual frames can then differ noticeably.

The test requires:

- the difference to be at least 8 dB below the C output's energy (measured
  −9.8 dB uninterrupted and −15.7 dB with resets),
- the output level to be within 0.5 dB of the C output (measured +0.18 and
  +0.24 dB).

On a real voice recording on the mixer, nnnoiseless and the C build differed
by about −42 dB (`target/bench/nnnoiseless-vs-rnnoise`, 2026-10-26).
