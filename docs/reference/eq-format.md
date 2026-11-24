# EQ format and EasyEffects interchange

Here's how a channel's EQ is stored, how the filter coefficients are calculated, and how RC2FX converts to and from EasyEffects presets. The mixer's filter math lives in `rc2fx-core`, and the desktop app uses a direct port in `desktop/core` so the plotted curve matches what the DSP actually runs.

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
| `id` | Lowercase letters, digits, and hyphens, up to 48 characters, unique within the channel |
| `type` | `off`, `bell`, `low-shelf`, `high-shelf`, `low-pass`, `high-pass`, `notch` |
| `mode` | `apo-dr` (Equalizer APO's model) or `rlc-bt` |
| `frequency` | 10–24000 Hz |
| `gain` | −36 to +36 dB |
| `q` | 0–100 |
| `inputGainDb`, `outputGainDb` | −36 to +36 dB, applied inside the EQ |

- Band IDs stay stable across edits. Reordering or deleting a band doesn't touch the remaining bands' IDs, which lets the mixer preserve each surviving band's filter state without glitches.
- Stereo targets can include an optional `rightBands` array for split-channel EQ. If it's omitted, both channels share `bands`. An empty `rightBands` array means the right channel is independent with zero bands.
- If any band has `solo: true`, only soloed bands run. Disabled bands and `off` bands are skipped.
- Turning the EQ off bypasses its bands and both gains. An enabled EQ with zero bands still applies `inputGainDb` and `outputGainDb`.
- Fresh channels start with the EQ disabled and no bands. Adding a band defaults to an enabled bell in `apo-dr` mode at 1 kHz, Q 1, 0 dB.

Every numeric field has to be finite and the right type. Unknown fields, duplicate keys or band IDs, and out-of-range values are rejected outright; nothing gets silently clamped or fixed up.

## Filter math

Everything runs at 48 kHz. A band's frequency is saved as entered, and the filter itself is designed at `min(frequency, 0.49 × 48000)`.

`apo-dr` uses the standard RBJ cookbook formulas with `Q = max(storedQ, 0.1)`.

`rlc-bt` converts Q first and then feeds it into the same RBJ formulas. For a bell filter, with `g = 10^(gain/20)`:

```text
Q_RBJ = (1 + 2·storedQ) · √g / (2·√(1 + g²))
```

For all other filter types, `Q_RBJ = (1 + storedQ) / 2`.

Shelves compute alpha directly from Q; they don't use the shelf slope `S`. Pass and notch filters still store a `gain` field, but they ignore it. We always keep the original mode and Q on disk so changing gain later recomputes the converted Q properly. Reference formulas and independent test vectors live in `desktop/core/testdata/easyeffects/`.

Adding, removing, or reordering bands won't restart noise suppression or any other effect in the chain, and bands that stay put keep their filter state.

## EasyEffects presets

Import and export live in `desktop/core`. Importing a preset updates only the currently selected channel's EQ.

### Export

Exporting writes an EasyEffects pipeline preset containing just the equalizer:

- Uses the `input` pipeline for microphones and `output` for everything else.
- Writes `equalizer#0` with both channels and explicit values for every band field RC2FX tracks.
- Leaves out `slope` and `width` so EasyEffects falls back to its defaults (x1 and 4 octaves).
- Maps disabled bands to `mute: true`.
- Writes an empty EQ as a single neutral `off` band.
- Pads the shorter side with `off` bands if left and right are independent and have different lengths.
- Caps output at 32 bands.

### Import

The importer handles current Qt and GTK-style EasyEffects presets as well as the older unindexed `equalizer` key. Missing fields fall back to EasyEffects' defaults. Presets that include other effects are fine; RC2FX just reads the equalizer and ignores the rest.

- If a file has multiple equalizers or pipelines, the app prompts you to pick one.
- Different left and right bands import as split-channel EQ on a stereo target. On a mono target, the app asks whether to pull the left or right channel.
- `slope` and `width` are accepted and ignored. Because RC2FX runs each band as a single filter section with no width parameter, a band with a slope steeper than x1 will sound gentler than it does in EasyEffects. That's the only approximation RC2FX makes.
- Anything else RC2FX can't reproduce faithfully is rejected with a short explanation: unsupported processing modes or filter types, "decramp", non-zero balance or pitch, or malformed/out-of-range values. A rejected import leaves your current EQ alone.

The same module also parses Equalizer APO and AutoEQ parametric text files.

Presets are treated strictly as data; the importer never executes or downloads anything referenced in them.
