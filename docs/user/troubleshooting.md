# Tips and troubleshooting

## Getting good sound

- **Use 48 kHz** (the mixer's native sample rate). On Windows, open the Sound
  control panel (**Advanced** tab) and set both the RØDECaster's playback and
  recording devices to 48000 Hz. Running at 44.1 kHz forces Windows to
  resample, which can cause crackle.
- **Use a good USB-C cable.** A flaky cable causes USB dropouts that sound
  identical to mixer crackle, so try swapping cables first.
- **Good starting chain for a mic**: high-pass at 80 Hz, noise suppression, a
  gentle compressor, and a limiter at the end.
- Keep in mind that noise suppression adds roughly 16.5 ms of latency on its
  channel. See [Noise suppression](noise-suppression.md#model-and-delay).

## The app can't find the mixer

- Make sure you're plugged into **USB 1** (USB 2 doesn't carry control data).
- On Linux, check that the udev rule from [Getting started](getting-started.md#linux)
  is installed.
- If more than one RØDECaster is plugged in, unplug the extra one.
- If the mixer connects but shows as running stock audio, either RC2FX isn't
  installed yet or it [fell back to stock](how-it-works.md#falling-back-to-stock).
  Click **Open installer** to install or repair it.

## RC2FX stopped processing

If RC2FX hits a serious error, it steps aside and falls back to stock audio
until the next reboot. Power-cycle the mixer to bring it back.

If it keeps happening, click **Export diagnostics…** (under **Settings ›
Diagnostics**, or on the error banner) and attach that file to a bug report.

If the mixer itself freezes, power-cycle it.

## Audio glitches or crackle

1. Check your USB-C cable and make sure your OS sample rate is 48 kHz.
2. Open **Settings › Diagnostics**, turn on **Show telemetry**, and watch the
   counters and event log while the glitch happens. See
   [Settings and diagnostics](settings-and-diagnostics.md#diagnostics).
3. If glitches only happen with noise suppression on, try turning off
   **Lookahead**.
4. Turn off **Settings › Mixer › Processing**. If the crackle goes away, RC2FX
   is causing it; if it stays, the issue is elsewhere in your chain.

## After a firmware update

The first boot after any firmware update (including installing RC2FX) takes
longer than usual while the mixer updates its internal controllers and reboots
once more. Let it finish before using it.

## The mixer forgets its setup on reboot

If the mixer walks you through initial setup on every boot, forgets Wi-Fi, and
shows an all-zeros serial number in RØDE's app, its settings partition didn't
mount. **Don't redo your setup while it's in this state**, since anything you
enter will disappear on reboot and your real setup is usually still intact.

- Right after an RC2FX install, if the installer says it restored damaged
  files, power-cycle the mixer and your setup should come back. If it reports
  damaged files it couldn't restore, run the install again.
- If the installer refuses because of the placeholder serial number, see the
  [recovery guide](../developer/mixer-workflow.md#repairing-a-mixer-that-forgets-its-setup).

Note: **Reset Device** in RØDE's app wipes your data partition, including SMART
Pad sounds, so back those up first.

## Quirks with RØDE's software

RØDE's desktop app and Windows virtual audio devices work alongside RC2FX,
though two stock RØDE quirks can still pop up:

- Leaving RØDE's app sitting on its device list for a minute before clicking
  your mixer can hang on "SYNCHRONIZING DEVICE". Reopen RØDE's app and click
  the mixer right away.
- If RØDE's virtual audio devices (Virtual A/B, Game, Music) disappear on
  Windows, check if a browser or another app is playing directly to the raw
  **Main Multitrack** device and close it.

## FAQ

**Does RC2FX use my computer's CPU?**
No. All audio processing runs on the mixer. The desktop app is only for
changing settings.

**Do I need to keep the app open?**
No. Changes save to the mixer automatically and survive reboots.

**Does it work with a console, phone, or second computer?**
Yes. Every device connected to the mixer hears the processed audio.

**Does a RØDE firmware update remove RC2FX?**
Yes. Updating through RØDE's app flashes stock firmware and wipes RC2FX's
settings. Save a backup first (**Settings › Files › Save…**) so you can reload
it after reinstalling. (Updating through RC2FX's own installer backs up your
settings automatically.)

**Can I break my mixer?**
On the Pro II, the installer uses RØDE's standard update flow and flashing
stock firmware removes RC2FX cleanly. It's still unofficial software with no
warranty, and Duo support hasn't been tested on hardware yet.
