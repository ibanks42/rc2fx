---
layout: home
---

<HomeHero />

RC2FX adds a full set of audio effects to the RØDECaster Pro II: a 32-band
parametric EQ, AI noise suppression, a compressor, a limiter, a high-pass
filter, and a gain stage. Everything runs right on the mixer itself, and you
control it over USB from a desktop app.

## Why run effects on the mixer?

The RØDECaster's built-in DSP gives you a 3-band EQ, a basic compressor, and a
noise gate, but no real noise suppression or parametric EQ. You can run effects
on your PC with software like Equalizer APO, EasyEffects, or a VST host, but
that only works on that one computer. Plug in a second PC, a game console, or a
phone, and you lose your processing.

With RC2FX, the processing happens inside the mixer's own audio engine. Every
device connected to your RØDECaster gets the exact same processed sound, with
zero CPU load on your computer, and you only need the desktop app open when
you want to tweak something.

## What you get

- **32-band parametric EQ** on any channel, great for dialing in a microphone
  or loading an AutoEQ profile for your headphones. Imports Equalizer APO,
  AutoEQ, and EasyEffects presets.
- **AI noise suppression (DeepFilterNet3)** retrained with keyboard and mouse
  clicks in mind, paired with an optional voice gate that quiets your mic
  between phrases.
- **Chat Mix Out noise suppression**, so people on Discord or Zoom hear every
  mic in the room cleaned up while Smart Pads and music bypass suppression.
- **Compressor, limiter, high-pass filter, and gain**, which you can drag into
  any order.
- **Effects on outputs too**, including all four headphone outputs, monitors,
  and USB mix outputs.
- **Safe by default**: nothing is processed until you turn a channel on, and if
  anything ever goes wrong, RC2FX steps out of the way and lets the mixer run
  stock. Flashing stock firmware removes it completely.

## Supported channels

RC2FX works on 25 channels on the Pro II:

- **Microphones**: all four combo inputs and both wireless receivers.
- **Sources**: USB 1, USB 2, Chat, all three CallMe channels, Bluetooth, and
  the four virtual USB inputs (Game, Music, Virtual A, and Virtual B).
- **Headphones**: all four headphone outputs.
- **Outputs**: monitors, Main Mix Out, Chat Mix Out, and Secondary Mix Out.

## What you need

- A **RØDECaster Pro II** (the installer handles updating it to a supported
  RØDE firmware, 1.7.7 or 1.7.6). RØDECaster Duo support is
  [experimental and untested](user/installing.md#rødecaster-duo-experimental)
  and stays off by default.
- A **Windows, macOS, or Linux** computer plugged into the mixer's **USB 1**
  port.
- The **RC2FX desktop app** ([Getting started](user/getting-started.md)).

Everything runs over USB 1 from the desktop app.

## Getting started

1. [Getting started](user/getting-started.md): download the app and plug in
   your mixer.
2. [Installing RC2FX](user/installing.md): install RC2FX onto the mixer.
3. [Using the app](user/using-the-app.md): turn on processing and build your
   effect chains.
4. [Tips and troubleshooting](user/troubleshooting.md): quick fixes for common
   issues.

Curious how it works under the hood? Check out
[How it works](user/how-it-works.md). For building from source or diving into
internals, see the [developer guide](developer/index.md) and
[technical reference](reference/index.md).

## Disclaimer

RC2FX is open source under the MIT license (you can view all third-party and
dataset licenses inside the app under **Settings › About › Licenses**). It is
an unofficial project, not affiliated with or endorsed by RØDE, provided as-is
without warranty. Installing custom firmware may void your warranty.
