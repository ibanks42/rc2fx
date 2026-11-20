# Noise suppression

Noise suppression removes background noise (fans, PC hum, traffic, keyboards,
mouse clicks) from your voice using a neural network trained to separate speech
from noise.

Turn it on from the **Noise suppression** effect in a channel's chain.

![Noise suppression on Mic 4: lookahead, blend, and the voice gate, with live readings from the mixer](/screenshots/mic-noise-suppression.png)

## Where you can use it

You can run noise suppression on one microphone or on Chat Mix Out at a time,
since the mixer's CPU has headroom for one instance.

To move it to a different channel, turn it off on the current channel and click
**Apply** when prompted, then turn it on where you want it. If the channel that
had suppression is no longer in your RØDE show, open Noise suppression on the
new channel and click **Release suppression slot** first.

### On Chat Mix Out

Chat Mix Out is the feed going to your computer over the Chat USB device (what
people hear on Discord, Zoom, or Teams). Putting suppression here cleans up
every microphone routed to Chat at once, so multiple mics in the same room all
get denoised.

Only microphones are denoised. Smart Pads, music, and other playback skip both
suppression and the voice gate and keep their full stereo sound, delayed by the
same small amount so everything stays in sync. RØDE's faders, mutes, and
mix-minus routing work normally.

Other effects on Chat Mix Out (like EQ or compression) apply to the combined
stereo mix, and noise suppression stays at the front of the chain.

## Model and delay

RC2FX runs DeepFilterNet3, retrained with extra keyboard and mouse-click
samples so those sounds don't slip through.

On a microphone channel, suppression adds about 16.5 ms of latency (about 2 ms
less on Chat Mix Out). The app shows the exact latency for the channel you're
editing. That delay is unnoticeable on calls, streams, and recordings, though
you'll likely hear it if you monitor your own processed mic in headphones.

### Lookahead

By default, **Lookahead** is off. Turning it on lets the model look 5 ms ahead
before deciding what to keep, which can make the start of words sound cleaner
at the cost of 5 ms of extra latency.

### Blend

**Blend** mixes the denoised signal with your dry voice. At 100% you hear only
the processed audio. Dial it back slightly if your voice ever sounds
over-processed.

## Voice gate

The voice gate ducks your mic between phrases when no speech is detected,
cleaning up any faint noise left behind. Because it follows the model's speech
detector instead of raw volume, loud non-speech sounds like keyboard clacks or
a door slam won't open it. It adds zero extra latency.

- **Simple**: one **Strength** slider (0% is off). Higher values close the gate
  more tightly and cut more level between phrases (about 20 dB at 50%, 30 dB at
  100%).
- **Custom**: exposes **Threshold**, **Reduction**, **Attack**, **Hold**, and
  **Release**, starting from your current Strength setting.

If the starts of your words get clipped, back off the strength (or lower the
threshold / raise the hold time in Custom mode).

## If the mixer falls behind

Suppression runs in parallel with the mixer's main audio loop. If a block ever
misses its deadline, dry audio plays through instead so you don't get clicks or
dropouts.
