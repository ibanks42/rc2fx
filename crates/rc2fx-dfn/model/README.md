# DeepFilterNet3 weights

Two DeepFilterNet3 exports are embedded in the engine at build time:
`weights.bin` here (LA0, lookahead 0) and the export in [`la1/`](la1/README.md)
(lookahead 1). `DeepFilter::for_lookahead` loads one of them. The per-path
saved setting `Suppression.lookahead` selects which, and defaults to 0.

## Model

DeepFilterNet3 with the GRUs narrowed to 128 units
(`emb_hidden_dim = df_hidden_dim = 128`, `nb_df = 64`, 601,660 parameters).
RC2FX retrained it on DNS-4 speech and noise, VCTK, room impulse responses,
and mouse-click and keyboard recordings (Freesound, FSD50K, ESC-50, AudioSet,
three Zenodo keystroke datasets and the project author's own recordings), with
the click and keyboard recordings weighted to a quarter of the noise. The
training kit is kept outside this repository. The weights are released under
MIT; [PROVENANCE.md](PROVENANCE.md#training-data) credits every training
dataset and notes the licences that still apply to them.

The weights here are the best checkpoint by validation loss from run
`a960s360d64-la0-b20` (epoch 120, 0.997914731502533). They were exported
without retraining or changing the lookahead. `config.ini` is the exact run
configuration. [PROVENANCE.md](PROVENANCE.md) has the hashes and the commands
used to generate the reference. The previous LA1 weights, reference and
configuration are kept in `la1/`, and the reference and batching-equivalence
tests still cover them.

## File format

`weights.bin` contains every float tensor of the model's PyTorch state dict,
in the format `Tensors::parse` reads (`src/net.rs`): a little-endian u32
tensor count, then for each tensor a u32 name length, the name, a u32 rank,
u32 dims and f32 data.

Four framing tensors follow the state dict and are required. Each has rank 1
and one integral f32 value:

| Tensor | Model parameter |
| --- | --- |
| `rc2fx.fft_size` | `fft_size` |
| `rc2fx.synthesis_size` | `synthesis_size` |
| `rc2fx.hop_size` | `hop_size` |
| `rc2fx.lookahead` | `df_lookahead`, which must equal `conv_lookahead` |

A file without these tensors is rejected. The engine takes its STFT framing
from them. The FFT size, which must be even, is the analysis window. The last
`synthesis_size` samples of it, between one and two hops, are resynthesised at
the hop. The lookahead must be less than the deep-filter order. The output
lags by `synthesis_size - hop_size` plus the lookahead. `src/dsp.rs` describes
the window pair.

Band counts come from the layer shapes: `nb_erb` from `enc.emb_gru.linear_in`
and `nb_df` from `enc.df_fc_emb`. This lets the same build load, for example,
a 480-point, 240-hop, lookahead-0 model or an asymmetric 960 / 360 / 240 one.
The file here is 960 / 360 / 240 / 0 (FFT, synthesis, hop, lookahead).

The engine's frame (`DeepFilter::frame`) is one hop, 240 samples. After a
frame's result is published, the worker computes the next frame's GRU
recurrent products in small steps (`DeepFilter::set_gru_prep`). The output is
the same as without this step.

## Delay

The suppression delay is 240 (frame) + 120 (synthesis overlap) + 320 (worker
lead) = 680 samples, 14.17 ms at 48 kHz. On a processed microphone the native
helper adds 112 samples, for 792 samples (16.5 ms). The 960-point analysis
window and the five causal DF taps add no delay.

## Precision

The engine quantises the weights to int8 when it loads them, with one scale
per row. The mixer's four cores share one L2 cache, and streaming 1.2 MB of
f16 weights every 5 ms hop made JACK drop buffers. int8 halves that traffic
and removed nearly all of the drops. `PRODUCTION_WORKER` in
`rc2fx-suppression` has the measurements.

## reference.wav

`reference.wav` is the output of DeepFilterNet's own runtime (libDF with
tract, every stage forced on) for the test signal in `src/tests.rs`, using the
same LA0 epoch-120 weights exported to ONNX. `matches_deepfilternet_reference`
compares the engine with it, and `preserved_la1_matches_its_own_reference`
does the same for LA1.

The engine's output differs from the reference because of weight precision and
a polynomial exp. LA0 measures −47.6 dB with the int8 weights (under 81 LSB),
on the host and on AArch64 under QEMU. LA1 measures −45.4 dB. The test limit is
128 LSB. These figures measure agreement with the reference implementation.
They don't measure how well the model removes noise.

To regenerate the reference:

1. Write the test input:

   ```sh
   RC2FX_DFN_INPUT=input.wav cargo test -p rc2fx-dfn write_reference_input -- --ignored
   ```

2. Run `input.wav` through libDF's `DfTract` with the model's ONNX export and
   `RuntimeParams::default_with_ch(1).with_thresholds(-100., 100., 100.)`,
   writing 16-bit PCM. The training kit's `dfref` tool does this:
   `dfref MODEL_onnx.tar.gz input.wav reference.wav`. `handoff.sh` runs it.

For an asymmetric framing, build `dfref` against the patched libDF
(`~/dfn/src/libDF`) and pass it as `DFREF=`. Unpatched libDF 0.5.6 ignores
`synthesis_size` and writes a symmetric reference.

## Replacing the weights

The training kit's `export/handoff.sh` takes the run's best checkpoint and:

1. writes `weights.bin` through `to_engine.py`, with every float tensor of
   `model.state_dict()` and the framing tensors from the run's config,
2. exports the model to ONNX,
3. regenerates `reference.wav` from the ONNX export,
4. copies both files here.

Then run `cargo test -p rc2fx-dfn`. If the per-frame time changed, review the
worker lead for `SuppressionAlgorithm::DeepFilterNet3` in `rc2fx-core`.
