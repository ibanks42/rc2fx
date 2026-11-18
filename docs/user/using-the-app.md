# Using the app

## Overview

After connecting, the app opens on the **Overview**. It shows one strip for
each fader in your current RØDE show, except SMART Pads, with a live level
meter. Headphones and outputs are listed below the faders. Processed channels
show the effects they run. Click a strip to edit that channel. **Overview**
at the top of the sidebar, or Esc, returns to this page.

## Channels

The sidebar lists the channels you are processing first. The remaining
channels follow in groups: **Microphones**, **Sources**, **Headphones** and
**Outputs**. Sources appear only if they are assigned to a fader in the
current show. RC2FX does not list or process SMART Pads, and the mixer's pad
playback works as before. **Settings** is at the bottom of the sidebar.

Selecting a channel opens its editor. Audio passes through a channel
unchanged until you turn on the **Processing** switch at the top right of its
page. The switch takes effect on the mixer immediately. Turning it off stops
processing and keeps the channel's effect settings. You can also set up
effects before turning processing on.

### Changing the show

Processing settings are tied to a source and follow it between faders.

- Removing an input from the show turns its processing off and keeps its
  effect settings.
- Moving an input to another fader keeps its processing and effects with that
  input. The input that takes over the old fader keeps its own settings.
- Adding a removed input back leaves processing off. Turn it on again when
  you want it.
- Headphones and outputs are unaffected by show changes.

RC2FX acts on a show change only after it reads two matching, complete copies
of the show, including at startup. Failed or partial reads change nothing.
Once the show is confirmed, processing is turned off for any saved input that
is absent, and enabling an absent input is undone. Until the show is
confirmed, an input that is still processed may appear under **PROCESSED,
NOT IN THIS SHOW** on the Overview.

## Building a chain

A processed channel is shown as a signal chain, read left to right. Effects
start switched off.

- Click an effect to open its controls and switch it on.
- Drag an effect onto another to change the order. For example, placing the
  compressor before or after the EQ changes how it responds.

| Effect | Use |
| --- | --- |
| High-pass | Cuts rumble, desk thumps and handling noise |
| Noise suppression | Removes background noise from a voice (microphones and Chat Mix Out only) |
| EQ | Shapes the tone, with up to 32 bands |
| Compressor | Evens out loud and quiet passages |
| Gain | Raises or lowers the level |
| Limiter | Keeps peaks below a ceiling |

[Effects](effects.md) describes each control. Noise suppression has its own
page: [Noise suppression](noise-suppression.md).

## Saving changes

There is no save button. Each change is heard at once, and the app saves it
to the mixer about two seconds after you stop editing. If a save fails, the
line under the channel in the chain says why, and the app tries again on its
own. Settings are stored on the mixer, so processing continues after you close
the app or switch the computer off.

To take changes back, use the undo and redo buttons on that line, or Ctrl+Z
and Ctrl+Shift+Z (Cmd+Z and Cmd+Shift+Z on macOS). While a channel differs
from how it was before your current round of edits, the line also shows
**Revert**, which puts it back.

Saving does not interrupt audio, with one exception: switching noise
suppression or its lookahead, or reordering effects while suppression is on,
restarts the mixer's audio engine, and processed channels go quiet for a
moment. The app asks first: **Apply** saves the change, **Undo the change**
puts the channel back.

If the mixer's settings change from another source while you are editing,
such as a second computer, the app keeps your edits and saves them on top. If
they no longer fit the mixer, for example because they name a channel it no
longer has, it offers **Reload mixer settings**. If you close the app before
your edits are saved, it asks for confirmation first.

## Presets and settings files

**Settings**, at the bottom of the sidebar, contains:

- **Presets**, stored on the mixer. You can save the current settings as a
  preset, load one or delete one, for example to switch between a podcast
  setup and a gaming setup.
- **Settings files** on your computer, under **Files**. **Open…** loads a
  file into your edits. **Save…** writes the current settings to a file, for
  a backup or to copy them to another mixer.

## Importing and exporting EQ

The EQ toolbar has **Import…**, **Paste** and **Export…**. Import and Paste
accept:

- Equalizer APO and AutoEQ parametric text. AutoEQ publishes corrections for
  thousands of headphones, which you can paste onto a headphone channel.
- EasyEffects presets.

**Export…** saves the channel's EQ as an EasyEffects preset.

Importing replaces only the selected channel's EQ. If a file contains more
than one equalizer, or a stereo EQ is imported onto a mono channel, the app
asks which one to use. EasyEffects Width and Slope fields are ignored on
import and left out on export. Steeper slopes (x2–x4) are imported as a
single, gentler filter section. Other unsupported settings are rejected with
a short explanation.

The EQ curve in the editor is calculated from the settings. It does not
reflect the live audio.

## Turning RC2FX off

- **Settings › Mixer › Processing** turns all RC2FX processing off or on
  again. Your settings are kept.
- The **Processing** switch on a channel turns off that channel only.

To remove RC2FX from the mixer, see
[Going back to stock](installing.md#going-back-to-stock).

## The status bar

The bar at the bottom of the window shows how many channels are processed.
When something needs attention, such as unsaved changes or a lost
connection, the bar shows that instead. Results of actions such as saving,
importing or exporting appear as notifications.
