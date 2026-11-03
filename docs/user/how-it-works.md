# How it works

This page describes how RC2FX runs on the mixer. The
[architecture](../developer/architecture.md) page has the technical detail.

## The audio engine

The RØDECaster Pro II contains a four-core ARM processor running Linux. All
of its audio passes through RØDE's audio engine, which processes a block of
audio every 2.3 ms, about 430 times per second.

RC2FX is a library that the audio engine loads at startup. In each 2.3 ms
block it runs the effects on the channels you have enabled and leaves the
other channels untouched. Because it runs inside the engine, it adds no extra
audio connections and has no fixed limit on how many channels it can process.

Noise suppression takes longer than 2.3 ms per block, so it runs on its own
core a few blocks behind the engine. That is the source of its delay.

## How RC2FX is loaded

The installer starts from RØDE's official firmware release and checks that it
is a version RC2FX supports. It then adds three things: RC2FX itself, your
settings, and a small launcher. RØDE's system starts the audio engine by name,
and the launcher is found first under that name, so the engine starts with
RC2FX loaded. A second launcher does the same for the screen program, which is
how [screen conversion](settings-and-diagnostics.md#screen-conversion) works.
RØDE's own programs are left unmodified.

At startup RC2FX compares the audio engine byte for byte with the builds it
has been tested against. If there is no match, it doesn't load, and the mixer
runs stock firmware behaviour.

## Falling back to stock

RC2FX switches itself off for the rest of the session, leaving the mixer
running as stock, if any of these happen:

- its control program stops
- the mixer doesn't finish starting within a minute
- the mixer stops unexpectedly twice in one session

Power-cycling the mixer gives RC2FX another try.

RC2FX doesn't run during the first start after a firmware update. The mixer
uses that start to update its internal controllers.

## Settings storage

Settings and presets are stored on the mixer. When you press **Save to
mixer**, the app sends the settings over USB. RC2FX validates them, saves
them, and switches to them between two audio blocks, so there is no gap or
restart. The app only needs to be open while you are changing settings.

## USB control

The app talks to the mixer over USB 1, the same connection RØDE's app uses.
That connection has a control channel for RØDE's app, and RC2FX sends its own
messages on it in a form RØDE's software ignores. It needs no network, IP
address or extra drivers, and USB audio is unaffected.

## Installing by flashing firmware

The installer adds RC2FX during a firmware update. At that point nothing is
running, and RØDE's updater writes the files. The installer prepares a
modified copy of the firmware on your computer, and the mixer installs it
through its normal update mode. If the update fails before the final step,
switching the mixer off for 30 seconds restores the previous firmware.
