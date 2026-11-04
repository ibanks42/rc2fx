# DeepFilterNet3 weights provenance

## Training data

Both embedded weight sets, LA0 (run `a960s360d64-la0-b20`) and LA1 (run
`a960s360d64-la1-b20`), were trained on the same data: each run loaded the
same dataset cache, uploaded once with SHA-256 checks. No dataset audio is
included in RC2FX; only the trained weights are.

Licence decision: the RC2FX project releases these weights under the MIT
licence, as it does its own code. That is the project's decision, not a legal
conclusion about the training data. It does not relicense any dataset, and the
credits below do not replace any dataset's own terms, which still apply to
that dataset.

A shorter list of these credits, shown in the app, is
[licenses/TRAINING-CREDITS.txt](../../../licenses/TRAINING-CREDITS.txt).

Each licence below says where it comes from: the training kit's own records,
or the dataset's upstream page as checked on 2026-11-04. Where it says "see
the dataset's terms", the licence was not verified and is not stated here.

Clip counts are the training split.

1. DNS Challenge 4 (ICASSP 2022), fullband clean read speech, lowest DNSMOS
   bin skipped: 310,962 clips. Microsoft DNS Challenge. Licence: see the
   dataset's terms, https://github.com/microsoft/DNS-Challenge
2. CSTR VCTK Corpus, from the DNS Challenge 4 fullband archives: 80,260
   clips. Centre for Speech Technology Research (CSTR), University of
   Edinburgh. Licence: see the dataset's terms,
   https://datashare.ed.ac.uk/handle/10283/3443
3. DNS Challenge 4 fullband noise (its AudioSet 000-006 and Freesound 000-001
   parts): 61,271 clips. Licence: see the dataset's terms,
   https://github.com/microsoft/DNS-Challenge . AudioSet:
   https://research.google.com/audioset/ . Freesound: https://freesound.org
4. DNS Challenge 4 room impulse responses (OpenSLR SLR26 simulated rooms at
   48 kHz, SLR28 real rooms): 58,334 impulse responses. Licence: Apache 2.0,
   per the OpenSLR pages, https://www.openslr.org/26/ and
   https://www.openslr.org/28/
5. Keyboard and click sounds selected from item 3: 1,176 Freesound clips and
   810 AudioSet clips. Licence: as item 3.
6. FSD50K keyboard and click sounds: 855 clips. The training kit excluded
   clips under non-commercial and Sampling+ licences; that filter has not been
   re-verified. Licence: each FSD50K clip has its own Creative Commons
   licence, per its Zenodo record, https://zenodo.org/records/4060432
7. ESC-50 mouse_click and keyboard_typing classes: 66 clips (plus 9 in
   validation and 5 in test). Karol J. Piczak. Licence: Creative Commons
   Attribution-NonCommercial 3.0 (CC BY-NC 3.0) per the training kit; the
   upstream README also states a Creative Commons Attribution Non-Commercial
   licence. This is a non-commercial licence.
   https://github.com/karolpiczak/ESC-50
8. About 14 further typing clips, probably the OpenGameArt keyboard pack
   (CC0 per the training kit); not matched to that source. Licence: unverified.
9. Freesound keyboard and mouse sounds, downloaded directly: 770 + 904
   training clips (keyboard + mouse). Across all splits these come from 952
   Freesound sounds: 796 under CC0 1.0 and 156 under CC BY 4.0, per the
   training kit's per-sound records. Each CC BY 4.0 sound is credited by
   author and page in [licenses/TRAINING-CREDITS.txt](../../../licenses/TRAINING-CREDITS.txt).
   https://freesound.org
10. SKAID keystroke dataset. Benjamin Quattrone and Youakim Badr. CC BY 4.0
    per its Zenodo record (its README separately describes academic and
    research use). https://zenodo.org/records/17282184
11. Keystrokes dataset. Pedro Pinto, Diogo Rafael Gonçalves Rodrigues,
    Gonçalo da Costa Macedo and Silvestre Malta. CC BY 4.0 per the training
    kit. https://zenodo.org/records/19453177
12. Keyboard and click recordings (Zenodo record 16564409) by Yoshiyuki
    Kobayashi / Sony Group Corporation. MIT per the training kit; the record's
    copyright line has not been verified. https://zenodo.org/records/16564409
13. 29 clips of the project author's own keyboard and mouse recordings.

Items 10 to 12 together contribute 209 training clips. The keyboard and click
sets (items 5 to 13) make up 25% of the noise drawn during training; item 13
alone is 3% of all noise draws.

## LA0 export

Exported 2026-10-30. The checkpoint was not retrained, and its lookahead was
not changed.

- Run: `.deps/dfn/runs/a960s360d64-la0-b20` (also `~/dfn/runs/...`).
- Checkpoint: `checkpoints/model_120.ckpt.best`, the newest `.ckpt.best`.
- Epoch 120 validation loss: **0.997914731502533**. Three sources agree on
  this value: `checkpoints/.best`, the last "Saving new best checkpoint" entry
  in `train.log`, and the completed validation phase in
  `training_runtime.json`.
- Architecture: DeepFilterNet3, 128-unit GRUs, 64 DF bins, 601,660 parameters.
- `config.ini` is byte-identical to the run config and to the config in the
  ONNX export: FFT 960, synthesis 360, hop 240,
  `df_lookahead = conv_lookahead = 0`.
- Only that checkpoint and config were copied into the private export run,
  `target/la0-export/run`. The original training mirror was left unchanged.

### Artifacts (SHA256)

| Artifact | SHA256 |
| --- | --- |
| `model_120.ckpt.best` | `22eda3db2868ebc0e1a04a18289ed30057cb7c8e0790025c2135c76f2f8cedc6` |
| `config.ini` | `ff3ed3dd0e0a1465a8078c424a4469c3bf005daa0654be1edfa7b4433e8d4226` |
| `weights.bin` | `0038a5643e210a704960698cce699767aa23c8c283acadd514bc9bbf448a5152` |
| `reference.wav` | `60b2c4fd01d32dcf8006d7c63d62576082c6b1e54a9e9d654685b0b0ba4a1fa8` |
| `target/la0-export/input.wav` | `c97400b9caef756c29baf96c75a7d47a840e3fb20ad5421482366768cc2039fc` |
| `target/la0-export/a960s360d64-la0-b20_onnx.tar.gz` | `49a68d9d67ea378a713c759e9f47587e4ed328217bbfbcfe807f77c99d6e19ec` |

The ONNX archive hash identifies this particular export. Archive timestamps can
change if it is exported again. `weights.bin` stores the original f32 tensors
plus the framing tensors. The engine quantises the weights when it loads them.
The export itself is unquantised.

### Conversion and reference

The existing `target/dfn-training/export/to_engine.py` and `export_onnx.py`
were run with `~/dfn/venv-rocm/bin/python` against the private run above. The
ONNX export's built-in checks passed. No other converter was used.

The test input was generated from `src/tests.rs::signal` with:

```sh
RC2FX_DFN_INPUT=target/la0-export/input.wav \
  cargo test --locked -p rc2fx-dfn write_reference_input -- --ignored
```

The older `export/reference_input.wav` differs from it in two samples, by one
16-bit LSB each. It was **not** used for the final LA0 reference. The LA1
reference was left unchanged.

The existing `tools/dfref/src/main.rs` was built in `target/la0-export/dfref`,
with its `deep_filter` dependency pointing at the patched asymmetric-window
libDF in `~/dfn/src/libDF`. Unpatched libDF 0.5.6 ignores `synthesis_size`.
`dfref` renders every stage through libDF's tract runtime, which is separate
from the RC2FX engine, with `with_thresholds(-100., 100., 100.)`:

```sh
target/la0-export/dfref-build/release/dfref \
  target/la0-export/a960s360d64-la0-b20_onnx.tar.gz \
  target/la0-export/input.wav target/la0-export/reference.wav
```

The reference therefore comes from the **same LA0 epoch-120 checkpoint**. It
was produced by libDF from that checkpoint's ONNX export, without engine
output, LA1 weights or any time shift. Matching it shows the engine's
inference agrees with libDF. Suppression quality has to be judged separately,
on a device and by listening.

Source hashes:

- `to_engine.py`: `3203ef01f26d64091dad0bad9b0eb20101de593ebaa00c86453f9278d0b494ba`
- `export_onnx.py`: `499efa9834b9a11f93f87e828fe144d75d0f3a62554d37400aec341637cf7a7f`
- Patched libDF `src/lib.rs`: `c316946c14ca13adf61c573d530149eda09f7c1b636a12f73936e39c81fcbd0b`
- Patched libDF `src/tract.rs`: `76aad90917888f4b9e8fbd1418cd414314c9f470a4c9a8921868b7c5b9cb2805`

### Timing

At 48 kHz the engine processes one 240-sample hop per call. The intrinsic delay
is **360 - 240 + 0*240 = 120 samples**, or 2.5 ms. With the 320-sample worker
lead, the worker plays a result `frame + lead = 240 + 320 = 560` samples after
the frame's first input sample. Adding the intrinsic delay gives
**680 samples = 14.17 ms**. On a processed microphone the native helper's
112-sample block brings it to **792 samples = 16.5 ms**.

The model adds no lookahead of its own. Both lookaheads are zero, and the
encoder histories and the five DF taps use only past and current spectra. The
960-point analysis window extends into the past only. DSP reconstruction tests
verify the synthesis-overlap delay. The suppression timing test verifies that
no extra queued frame is inserted between collecting a frame and playing it.
These figures cover the processing path only. ADC, DAC, host driver and other
hardware latency are not included.

### LA1

`la1/` holds the LA1 weights, reference and configuration (epoch 102,
validation loss about 0.9706). Tests check the LA1 engine against its own
reference.

- LA1 weights: `55dec3128a987340686b0110d928a2f655c7425d804e99fcca3a4e0c4b8b36db`
- LA1 reference: `990af56117eb24f37df77909ddf55649026943b7863fd1dd3feb51cadf98d3f4`
