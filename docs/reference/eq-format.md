# EQ format and EasyEffects interchange

This page covers how a channel's EQ is stored, how its filters are
calculated, and how it converts to and from EasyEffects presets. The filter
maths is in `rc2fx-core`, and both the app's curve display and the mixer's
processing use it, so they produce the same coefficients.

## Stored form

```json
{
  "enabled": true,
  "inputGainDb": 0,
  "outputGainDb": 0,
  "bands": [
    {
      "id": "band-1",
      "enabled": true,
      "type": "bell",
      "mode": "apo-dr",
      "solo": false,
      "frequency": 1000,
      "gain": 0,
      "q": 1
    }
  ]
}
```

| Field | Values |
| --- | --- |
| `bands` | 0–32 bands |
| `id` | Lowercase letters, digits and hyphens, up to 48 characters, unique within the channel |
| `type` | `off`, `bell`, `low-shelf`, `high-shelf`, `low-pass`, `high-pass`, `notch` |
| `mode` | `apo-dr` (Equalizer APO's model) or `rlc-bt` |
| `frequency` | 10–24000 Hz |
| `gain` | −36 to +36 dB |
| `q` | 0–100 |
| `inputGainDb`, `outputGainDb` | −36 to +36 dB, applied inside the EQ |

- Band IDs are stable. Reordering or deleting bands leaves the other bands'
  IDs unchanged, which lets the mixer keep each band's filter state across an
  edit.
- An optional `rightBands` array gives the right channel of a stereo target
  its own bands. Without it, both channels use `bands`. An empty
  `rightBands` array means the right channel is independent and has no
  bands.
- If any band has `solo: true`, only soloed bands process audio. Disabled
  bands and `off` bands do not process audio.
- A disabled EQ bypasses its bands and both gains. An enabled EQ with no
  bands still applies its gains.
- New channels start with no bands and the EQ off. A new band is an enabled
  bell in APO mode at 1 kHz, Q 1, 0 dB.

Every value must be finite and of the correct type. Unknown fields, duplicate
keys or IDs, and out-of-range values are rejected. Invalid values are not
corrected.

## Filter maths

The sample rate is 48 kHz. The frequency is stored as entered, and the
filter is designed at `min(frequency, 0.49 × 48000)`.

`apo-dr` uses the standard RBJ cookbook formulas with
`Q = max(storedQ, 0.1)`.

`rlc-bt` converts its Q and then uses the same formulas. For a bell, with
`g = 10^(gain/20)`:

```text
Q_RBJ = (1 + 2·storedQ) · √g / (2·√(1 + g²))
```

For the other types, `Q_RBJ = (1 + storedQ) / 2`.

Shelves compute alpha from Q. They do not use the shelf slope S. Pass and
notch filters store a gain but do not apply it. The stored mode and Q are
always the originals, so a gain change recomputes the converted Q
correctly. Reference formulas and independent coefficient vectors are in
`fixtures/easyeffects/`.

Adding, removing or reordering bands does not restart the other effects or
noise suppression. Bands that remain after an edit keep their filter state.

## EasyEffects presets

Import and export are a separate pure module in `rc2fx-core`. An import
changes only the selected channel's EQ.

### Export

Export writes an EasyEffects pipeline preset that contains only the
equalizer:

- The pipeline is `input` for microphones and `output` for all other
  targets.
- It contains `equalizer#0` with both channels and an explicit value for
  every band field RC2FX has.
- `slope` and `width` are omitted, so EasyEffects uses its defaults (x1 and
  4 octaves).
- A disabled band is written as `mute: true`.
- An empty EQ is written as one neutral `off` band.
- If one independent channel has fewer bands, it is padded with `off` bands.
- At most 32 bands are written.

### Import

Import accepts current Qt and GTK-style presets and the older unindexed
`equalizer` key. Omitted fields take EasyEffects' defaults. A preset with
other effects is accepted, and only its equalizer is read.

- If a file contains more than one equalizer or pipeline, the app asks which
  one to use.
- Different left and right bands become independent bands on a stereo
  target. On a mono target the app asks whether to use the left or right
  channel.
- `slope` and `width` are read and ignored. RC2FX processes each band as a
  single filter section and has no width parameter, so a band with a slope
  above x1 sounds gentler than in EasyEffects. This is the only setting
  RC2FX approximates.
- Other settings RC2FX cannot reproduce exactly cause the import to be
  refused with a short reason: unsupported processing modes or filter types,
  "decramp", non-zero balance or pitch, and malformed or out-of-range
  values. A refused import leaves the current EQ unchanged.

The same module reads Equalizer APO and AutoEQ parametric text.

Presets are treated as data. The importer does not execute or download
anything from them.
