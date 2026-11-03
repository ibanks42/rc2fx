# EasyEffects fixture provenance

Research date: 2026-10-22.

## Synthetic presets

These files were written for the tests from the documented preset format.
They are not exports captured from EasyEffects.

| File | Purpose |
| --- | --- |
| `output-linked-apo.json` | Complete current envelope, two linked RBJ-compatible Bells, EQ input −6 dB and output +1.5 dB. |
| `output-apo-common-types.json` | Bell, shelves, passes, notch, and Off. Current legal shape, proposed extended DSP subset. |
| `input-empty-as-off.json` | One Off band per channel. Legal EE representation for an internally empty EQ. |
| `input-v6-unindexed.json` | Old `equalizer` name without `#0`. Omits fields that v6 did not save. Uses typed values and left/right objects. |
| `output-split-different.json` | Complete current shape with different left/right gains. Not safely importable into linked-only DSP. |

Format sources:

- Current serializer: https://github.com/wwmm/easyeffects/blob/5783e6eb980ccd29d3aac74b7b8a032f1d55c390/src/equalizer_preset.cpp
- Current loader envelope: https://github.com/wwmm/easyeffects/blob/5783e6eb980ccd29d3aac74b7b8a032f1d55c390/src/presets_manager.cpp
- Current defaults/ranges: https://github.com/wwmm/easyeffects/blob/5783e6eb980ccd29d3aac74b7b8a032f1d55c390/src/contents/kcfg/easyeffects_db_equalizer.kcfg
- Current band defaults/ranges: https://github.com/wwmm/easyeffects/blob/5783e6eb980ccd29d3aac74b7b8a032f1d55c390/src/contents/kcfg/easyeffects_db_equalizer_channel.kcfg
- Legacy serializer: https://github.com/wwmm/easyeffects/blob/2ee3f0b76f400c70c80691f5c8018d6dfec525e9/src/equalizer_preset.cpp

The presets follow the documented format. No upstream program code or preset
content was copied into them.

## Community preset extract (MIT)

`community-loudness-eq-extract.json` contains the complete
`output["equalizer#0"]` object from the author's preset:

- Repository: `Digitalone1/EasyEffects-Presets`.
- Commit: `6fc0630f3d18f5668b11ebe4846179914b0bd24e`.
- Original file: https://github.com/Digitalone1/EasyEffects-Presets/blob/6fc0630f3d18f5668b11ebe4846179914b0bd24e/LoudnessEqualizer.json
- Original license: https://github.com/Digitalone1/EasyEffects-Presets/blob/6fc0630f3d18f5668b11ebe4846179914b0bd24e/LICENSE
- License copy: `LICENSE.community`, MIT, Copyright (c) 2020 Giusy Digital.

Changes from the original: all other effects removed, `blocklist: []` kept,
`plugins_order` changed to `["equalizer#0"]`, and the JSON reindented. No EQ
values were changed, so the file holds the EQ only and omits the rest of the
original effect chain. It has ten linked Bell bands, all `RLC (BT)`, `x1`, Q
1.6, and no decramp field.

The preset comes from its author's own repository and is not an official wwmm
fixture. It is included because it is an ordinary preset that an APO-only
importer would reject.

## Official historical fixtures (examined, not copied)

The pinned official EasyEffects repository has no serialized audio preset JSON
fixtures. Older official fixtures are at:

- https://github.com/wwmm/easyeffects/blob/bbb91bafc58757767a5aa0e76049aa76c97c76c5/data/presets/gstreamer_pop.json
- https://github.com/wwmm/easyeffects/blob/bbb91bafc58757767a5aa0e76049aa76c97c76c5/.circleci/presets/soe_preset_disable_all.json

They hold old string-valued PulseEffects/GStreamer settings, and the first one
has no pipeline envelope or separate channels. The modern loader can't read
them and they can't serve as export templates. No GPL-covered fixture or
source file is included in this directory.

## Coefficient vectors

`rlc-bt-x1-mapping-vectors.json` contains double-precision coefficients at
48 kHz, derived for these tests. Coefficients are ordered `[b0,b1,b2,a1,a2]`
with denominator `[1,a1,a2]`.

DSP references:

- RLC analog prototypes, `Filter::calc_rlc_filter`: https://github.com/lsp-plugins/lsp-dsp-units/blob/8dd2a6905ca7ccefd2c3624560235680cb8761e2/src/main/filters/Filter.cpp#L728-L1010
- Prewarped substitution, `Filter::bilinear_transform`: same file, lines 2231–2274.
- RBJ/APO coefficients, `Filter::calc_apo_filter`: same file, lines 1495–1660.
- Model/order dispatch, `para_equalizer::decode_filter`: https://github.com/lsp-plugins/lsp-plugins-para-equalizer/blob/8e256db4e77154c9cecb207ebf769a3e1e49f878/src/main/plug/para_equalizer.cpp#L134-L327

The vectors compare the algebraic prototypes with the RBJ formulas. They are not
recordings or output captured from a compiled LSP build, so runtime smoke
tests against EasyEffects and LSP are still needed. JSON syntax,
contiguous channel bands and RFC 6902 patch paths were checked locally without
Python.
