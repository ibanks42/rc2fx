# How it works

Here's a quick overview of how RC2FX runs on the mixer. For low-level details,
see [Architecture](../developer/architecture.md).

## Running inside the audio engine

Inside the RØDECaster Pro II is a quad-core ARM processor running Linux. All
audio passes through RØDE's audio engine in 2.3 ms blocks (about 430 times a
second).

RC2FX loads into that engine as a shared library when the mixer starts. On each
block, it runs your enabled effects and passes every other channel through.
Because it runs directly inside RØDE's engine, there are no extra virtual
routing hops and zero CPU load on your computer.

Noise suppression takes more than 2.3 ms per block, so it runs on its own CPU
core a few blocks behind the main audio loop, which is where its ~16 ms latency
comes from.

## How it installs without modifying RØDE's binaries

The installer downloads an official RØDE firmware image, adds RC2FX, your
settings, and a small launcher script, and flashes it through the mixer's
standard USB update mode. RØDE's own audio engine binary isn't modified.

On every boot, RC2FX verifies the audio engine binary against the exact
firmware builds it was tested on. If anything doesn't match, RC2FX stays out of
the way and the mixer boots stock.

On Pro II firmware 1.7.7, RC2FX also optimizes how the mixer's sideways
touchscreen redraws are rotated so heavy screen updates don't compete with
audio when noise suppression is running.

## Falling back to stock

If something goes wrong on the mixer (its control service stops, startup takes
over a minute, or the engine crashes twice in one boot), RC2FX automatically
disables itself for the rest of the session and hands audio back to stock.
Power-cycling the mixer lets RC2FX try again.

RC2FX also sits out the first boot right after a firmware update while the
mixer updates its internal controllers.

## Settings and USB control

Your settings and presets are stored on the mixer itself. As you adjust sliders
in the desktop app, changes are sent over USB 1 and saved automatically. Most
updates swap in cleanly between two audio blocks with no dropout or restart.

RC2FX shares the same USB 1 control link that RØDE's app uses, in a format
RØDE's software ignores, running over the existing USB 1 cable alongside USB
audio.
