# Effects

Every channel has six effects. They all start off, and you can drag them into
whatever order you want. Noise suppression is the only effect that adds
noticeable latency.

## High-pass

Cuts everything below the **Cutoff** frequency (20 to 1000 Hz, default 80 Hz).
Human voices don't have much useful energy down there, so on a mic this clears
out low rumble from traffic, AC units, or desk bumps. It's a good first effect
for almost any microphone.

## Noise suppression

Available on microphones and Chat Mix Out. See
[Noise suppression](noise-suppression.md) for details.

## EQ

![A 10-band EQ on Headphones 1](/screenshots/headphones-eq.png)

The parametric EQ supports up to 32 bands. Drag points directly on the curve,
scroll over a point to change its Q, or double-click to add a band. You can
also type exact numbers for any band.

Band controls:

- **Type**: bell, low shelf, high shelf, low-pass, high-pass, notch, or off.
- **Filter design** (APO or RLC): controls how Q maps to filter shape. Imported
  EQ files pick the right one automatically.
- **Frequency** (10 to 24000 Hz), **gain** (±36 dB), and **Q** (width).
- **On**: bypasses a band without deleting it.
- **Solo**: plays only the soloed bands so you can hear exactly what you're
  cutting or boosting.

Use **Input gain** and **Output gain** (±36 dB) to trim levels going in and out
of the EQ. On stereo channels, **Left** and **Right** let you tune each side
separately, and **Both sides** links them back together.

Common uses include a gentle cut around 200 to 400 Hz to clear up a muddy
voice, a tight notch for a hum, or an AutoEQ profile on a headphone output.

## Compressor

Evens out loud peaks so quieter speech stays easy to hear.

| Control | What it does | Range | Default |
| --- | --- | --- | --- |
| Threshold | Level where compression starts | −60 to 0 dB | −18 dB |
| Ratio | How much gain is reduced above the threshold | 1 to 20 | 3 |
| Attack | How fast it reacts when the level rises | 0.1 to 200 ms | 10 ms |
| Release | How fast it recovers after the level drops | 10 to 2000 ms | 120 ms |
| Makeup | Gain added back after compression | 0 to 24 dB | 0 dB |

## Gain

Boosts or cuts the signal from −60 to +12 dB.

## Limiter

Stops peaks from going past the **Ceiling** (−24 to 0 dBFS, default −1 dBFS)
and recovers over the **Release** time (10 to 2000 ms, default 100 ms). Put it
at the end of your chain to prevent clipping.

If your recordings will be encoded to MP3 or another lossy format, leave a bit
of extra headroom (like −1 dBFS) so inter-sample peaks don't clip during
encoding.
