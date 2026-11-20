# Using the app

## Overview

When your mixer connects, the app opens on the **Overview**. You'll see a live
level meter for every fader in your current RØDE show (except SMART Pads), with
headphones and outputs underneath. Channels with processing turned on show
their active effects right on the strip. Click any strip to open its editor,
and click **Overview** (or press Esc) to come back.

## Channels

Channels you're actively processing sit at the top of the sidebar, with
everything else grouped below under **Microphones**, **Sources**,
**Headphones**, and **Outputs**. Sources only appear if they're assigned to a
fader in your current RØDE show. SMART Pads aren't processed, so they behave
just like stock.

Clicking a channel opens its editor. Audio passes through untouched until you
flip on the **Processing** switch at the top right. Turning it off bypasses the
chain without losing your settings, so you can set everything up before turning
it on.

### Changing your RØDE show

Your effect settings follow each input source, not the fader slot:

- Remove an input from your show and its processing pauses, while its effect
  settings stay saved.
- Move an input to a different fader and its effects move with it.
- Add a removed input back and it stays bypassed until you turn processing back
  on.
- Headphones and outputs aren't affected by show changes.

## Building an effect chain

Each channel shows its signal chain from left to right. Every effect starts
off.

- Click an effect to open its controls and switch it on.
- Drag effects onto each other to reorder them. For example, putting the
  compressor before or after the EQ changes how it responds.

| Effect | What it's for |
| --- | --- |
| High-pass | Cuts rumble, desk bumps, and AC hum |
| Noise suppression | Removes background noise from speech (mics and Chat Mix Out) |
| EQ | Shapes tone with up to 32 parametric bands |
| Compressor | Evens out loud and quiet parts |
| Gain | Raises or lowers level |
| Limiter | Stops peaks from clipping |

See [Effects](effects.md) for every control, and
[Noise suppression](noise-suppression.md) for the denoiser and voice gate.

## Saving changes

There's no save button. You hear edits immediately, and the app saves them to
the mixer automatically a couple of seconds after you stop tweaking. Because
settings live on the mixer, your effects keep running after you close the app
or turn off your computer.

Use the undo/redo buttons (or Ctrl+Z / Ctrl+Shift+Z, Cmd+Z / Cmd+Shift+Z on
macOS) to step back through edits, or click **Revert** to reset a channel back
to how it started.

Most changes apply with zero audio interruption. The one exception is turning
noise suppression on or off, toggling its lookahead, or reordering effects
around it: that briefly restarts the audio engine, so the app will ask you to
click **Apply** before doing it.

## Presets and settings files

Under **Settings** at the bottom of the sidebar:

- **Presets** are saved right on the mixer, so you can quickly switch between
  setups (like streaming vs. podcasting).
- **Files** lets you save your entire RC2FX setup to a file on your computer
  (**Save…**) or load one back (**Open…**), handy for backups or moving
  settings to another mixer.

## Importing and exporting EQ

The EQ toolbar has **Import…**, **Paste**, and **Export…**:

- **Import…** and **Paste** accept Equalizer APO text, AutoEQ profiles
  (great for loading headphone correction curves onto a headphone output), and
  EasyEffects presets.
- **Export…** saves the current channel's EQ as an EasyEffects preset.

Importing only replaces the EQ on the channel you're editing. If a file has
more than one equalizer or you import a stereo EQ onto a mono channel, the app
asks which one to use.

Note that the EQ graph shows your filter curve, not a live spectrum analyzer.

## Turning RC2FX off

- Flip off **Settings › Mixer › Processing** to bypass all RC2FX processing
  while keeping your settings.
- Flip off the **Processing** switch on an individual channel to bypass just
  that channel.

To remove RC2FX from the mixer completely, see
[Going back to stock](installing.md#going-back-to-stock).

## The status bar

The bar at the bottom shows how many channels are being processed, or flags
anything that needs your attention (like a disconnected mixer or pending save).
