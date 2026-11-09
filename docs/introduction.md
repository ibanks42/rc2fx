---
layout: home
---

<HomeHero />

RC2FX adds audio processing to the RØDECaster Pro II: a parametric EQ, a
compressor, a limiter, a high-pass filter, a gain stage and noise
suppression. It runs on the mixer, inside RØDE's audio engine, and you
control it from a desktop app over USB.

## Background

The RØDECaster's built-in processing offers a few fixed EQ presets, a basic
compressor and a noise reduction setting that can make voices sound muffled.
Processing on the computer instead, with a plugin host, a virtual audio cable,
EasyEffects or Equalizer APO, only affects audio from that one computer. It
stops helping once a second computer or a console is connected, or when an
app opens the raw device.

RØDE's firmware updates are unsigned archives and the mixer runs Linux. RC2FX
uses this to load its processing into the mixer's own audio engine.

## Features

- Processing happens on the mixer. Every computer, console or phone
  connected to it hears the processed audio, and the desktop app only needs
  to be open while you change settings.
- A parametric EQ with up to 32 bands per channel, for correcting a
  microphone or applying an AutoEQ headphone correction. It accepts
  Equalizer APO and AutoEQ text and EasyEffects presets.
- Noise suppression with DeepFilterNet3, retrained by this project with
  keyboard and mouse-click noise in the training data. An optional voice gate
  lowers the microphone between phrases.
- Suppression on the Chat output. People on Discord or Zoom hear every
  microphone in the room with suppression applied, while Smart Pad sounds and
  music pass through unchanged.
- Processing on headphones, monitors and the USB mix outputs as well as on
  inputs.
- A fresh install processes nothing. You turn on each channel and effect
  yourself.
- If RC2FX fails, it stops processing and the mixer runs as stock. Flashing
  stock firmware removes it.

## Supported channels

RC2FX can process 25 paths on the Pro II:

- Microphones: the four combo inputs and both wireless receivers.
- Sources: USB 1, USB 2, the Chat USB input, the three CallMe channels,
  Bluetooth, and the four virtual USB inputs (Game, Music, Virtual A and
  Virtual B).
- Headphones: all four headphone outputs.
- Outputs: the monitors, Main Mix Out, Chat Mix Out and Secondary Mix Out.

Each path has its own effect chain, in any order. The
[virtual USB paths](reference/usb-paths.md) reference lists how these map to
the mixer.

## Requirements

- A RØDECaster Pro II. RC2FX supports RØDE firmware 1.7.7 and 1.7.6, and the
  installer flashes one of those versions. RØDECaster Duo support is
  [experimental and untested](user/installing.md#rødecaster-duo-experimental)
  and is off by default.
- A computer running Windows, Linux or macOS, connected to the mixer's
  **USB 1** port with a USB-C cable. The computer is needed to install RC2FX
  and to change settings.
- The RC2FX desktop app ([getting started](user/getting-started.md)).

No network connection to the mixer, SSH access or command-line tools are
needed.

## Next steps

1. [Getting started](user/getting-started.md): download the app and connect
   the mixer.
2. [Installing RC2FX](user/installing.md): flash RC2FX onto the mixer.
3. [Using the app](user/using-the-app.md): turn on processing and set up
   effects.
4. [Tips and troubleshooting](user/troubleshooting.md): fixes for common
   problems.

[How it works](user/how-it-works.md) describes what RC2FX does on the mixer.
The [developer guide](developer/index.md) covers building RC2FX, and the
[technical reference](reference/index.md) has the implementation details.

## License and disclaimer

RC2FX's own code and its trained noise suppression weights are licensed under
MIT. Third-party components keep their own licences, and the datasets the
weights were trained on keep their own terms. The app lists them all under
**Settings › About › Licenses**.

RC2FX is an unofficial project and is not affiliated with or endorsed by
RØDE. It is provided as is, without warranty. Installing custom firmware may
void your warranty.
