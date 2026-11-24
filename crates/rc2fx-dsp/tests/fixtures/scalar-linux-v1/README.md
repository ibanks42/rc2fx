# Frozen scalar DSP oracle, version 1

These golden fixtures were generated on 2026-10-23 by the independent C scalar DSP before its test driver was retired. They were **not** produced by the Rust implementation. A clean rebuild and two fresh renders matched the earlier oracle output byte for byte, with an identical executable hash.

The Rust test suite embeds these binary files directly and compares against them during `cargo test`.

## Qualified targets

These fixtures are qualified for **Linux x86_64 and aarch64, little-endian**. ARM64 was added after an isolated Cortex-A53 QEMU run against the Pro II stock runtime matched both mono and stereo fixtures with zero differing `f32` samples and zero max audio error, without loosening any audio bytes or meter tolerances. The probe lives in `.local/arm-scalar-probe-20261023`, and its log is in `.local/rust-all-sources-arm64.zs5gaIGp/build/checks/arm-scalar-probe.log`.

Other architectures still fail to build the oracle test until their numerical behavior is verified. Windows behavior tests are separate, and because the ARM64 check ran under emulation, it only confirms functional agreement, not real-time device timing, models, or recovery.

## Files

| Fixture | Bytes | SHA256 |
| --- | ---: | --- |
| `mono.bin` | 181760 | `20c4a1cd67b1249e9fa79e7a2fefe269df21541bede7632c95312926ae0a39ef` |
| `stereo.bin` | 354880 | `cd356d1f66e8a83b07d21c3e24e3c226bdf1a65315b20e4d1da6fffceeb9f347` |

`provenance.json` pins the eight original source, header, and test files along with the GCC driver, `cc1`, assembler, linker, `libc`, `libm`, dynamic loader, and oracle binary.

- Compiler: GCC `13.3.0-6ubuntu2~24.04.1`, target `x86_64-linux-gnu`.
- Flags: `-std=c11 -O2 -ffp-contract=off -Wall -Wextra -Wno-misleading-indentation -Werror`, the captured header directory, and `-lm`.
- Generator: retired `crates/rc2fx-dsp/tests/c_oracle.c`, SHA256 `bcd32505bc298039870044bf9ffd28f6324f6208e228fbd77b89d101937ab126`.
- Scalar `device/audio/dsp.c`, SHA256 `1f5da2e5545184ec7e4064565fff241e3b7bd21f325b943ff1224f4cbc53c43e`.

Copies of the original generator, sources, build/runtime metadata, and raw output are archived in `.local/rust-dsp-golden.daIS4jmt`, with earlier evidence in `.local/rust-dsp-oracle-1792768055044714121`. Normal test runs don't need those directories.

If you ever need to regenerate these fixtures, run an independently reviewed oracle and update `provenance.json`. Never overwrite them with Rust output just to make a failing test pass. The production C source is still archived elsewhere until it's retired more broadly.

## Scenario and record format

Both fixtures run at 48 kHz for 180 blocks, totaling 43,280 samples per channel. Block sizes cycle through `[112, 7, 480, 1, 240, 63, 1024, 13]` with a partial final cycle. Each block record contains, in order:

1. `n` little-endian IEEE `f32` samples per channel (left, then right).
2. Four little-endian IEEE `f64` meters: input peak, output peak, sum of squares, and gain reduction.
3. Two little-endian `u64` counters: samples processed and clipped samples.

Audio samples are compared **bit for bit** and counters must match exactly. Floating-point meters must stay within `abs(actual-expected) <= 1e-10 * max(abs(expected), 1)`. Tests also check the exact fixture byte length and verify every record is consumed. Embedded FNV64 fingerprints catch accidental edits during normal test runs without pulling a crypto crate into the DSP, while `SHA256SUMS` provides full cryptographic hashes for external verification.

### Input

Input samples are generated with unsigned 32-bit arithmetic: `q = (((frame*1664525 + 1013904223 + channel*97) >> 8) & 65535) - 32768`, converted to `f32` as `q/65536`. Three overrides apply in order to exercise the nonfinite guards and headroom handling: frames divisible by 997 become `NaN`, multiples of 991 become `+inf`, and multiples of 983 become `-20`.

### Chain

The chain starts in the default effect order with suppression off (the generator aborts if suppression is constructed or invoked). Active stages are:

- High-pass at 80 Hz.
- Compressor: threshold -18 dB, ratio 3, attack 10 ms, release 120 ms, makeup 2 dB.
- Gain: -3 dB.
- Limiter: ceiling -1 dB, release 100 ms.
- EQ: input -4 dB, output 2 dB.

Left EQ has seven bands, one of each supported type. Stereo adds six independent right-channel bands with offset parameters. Band `i` uses frequency `75+401*i+73*right`, gain `(-1 if i odd else 1)*(1.5+i)`, Q `.41+.23*i`, alternating APO and RLC modes, slope 1, and width 1. The Rust scenario code is the exact code compared against the oracle, with its band, config, and sample source span pinned in `provenance.json`.

### Changes during the run

| Block boundary | Persistent change unless a range is stated |
| --- | --- |
| 20 | High-pass 140 Hz, gain -9 dB, band 1 frequency 510 Hz / gain -7 dB, EQ input -2 dB |
| 40 | Swap left bands 1 and 5 |
| 41 | Solo left band 2 |
| 42 | Truncate left EQ to five bands during the structural transition |
| 60 | Remove independent right EQ; use shared settings |
| 80–89 | Disable EQ |
| 100–119 | Bypass the chain |
| 130–139 | Disable compressor and limiter |
| 150 | 32-band cascade, frequency `30+700*i`, alternating ±12 dB; EQ input and output both -36 dB |

Together these hit coefficient smoothing, band ID and history preservation, rapid structural crossfades, linked stereo dynamics, bypass smoothing, and irregular block sizes. Leave this fixture untouched and add any new scenarios as separate fixtures.
