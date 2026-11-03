# Tips and troubleshooting

## Getting good sound

- Run at 48 kHz, the mixer's native rate. On Windows, set the RØDECaster's
  playback and recording devices to 48000 Hz in the Sound control panel
  (**Advanced** tab). At 44.1 kHz Windows resamples, which can cause crackle.
- Use a good USB-C cable. A worn cable can drop USB audio, which sounds the
  same as crackle from the mixer. Try another cable first when you hear
  crackle.
- For a voice, a reasonable starting chain is a high-pass at 80 Hz,
  suppression, a gentle compressor, and a limiter last.
- Noise suppression delays its channel by about 16.5 ms. See
  [Noise suppression](noise-suppression.md#model-and-delay).

## The app can't find the mixer

- Check that the cable is in USB 1. The app doesn't use USB 2.
- On Linux, check the USB permission rule in
  [Getting started](getting-started.md#linux).
- If the app reports more than one mixer on USB, connect only one.
- If the app reports that the mixer is connected but RC2FX isn't running,
  the mixer is running stock audio. Either RC2FX isn't installed or it has
  [fallen back to stock](how-it-works.md#falling-back-to-stock). **Open
  installer** installs or repairs it.

## RC2FX stopped processing

When RC2FX detects a problem it falls back to stock until the mixer is next
switched on. Power-cycle the mixer to start it again.

If it keeps happening, use **Export diagnostics…** (in **Settings ›
Diagnostics**, or on the error notice) and attach the file to your report.

If the mixer stops responding, power-cycle it.

## Audio glitches or crackle

1. Rule out the cable and the sample rate (see above).
2. In **Settings › Diagnostics**, turn on **Show telemetry** and watch the
   counters and the events window while the problem happens. See
   [Settings and diagnostics](settings-and-diagnostics.md#diagnostics).
3. If the glitches only happen with noise suppression on, turn off
   **Lookahead**, or try another [screen conversion](settings-and-diagnostics.md#screen-conversion)
   setting.
4. Turn off **Settings › Mixer › Processing**. If the glitches stop, RC2FX is
   the cause. If they continue, the cause is elsewhere.

## After a firmware update

The first start after a firmware update, including an RC2FX install, takes
longer. The mixer updates its internal controllers and restarts once more.
Wait for it to finish before using it.

## RØDE's software

RØDE's RØDECaster app and Windows virtual device driver both work while
RC2FX is running on USB 1. These two issues also occur on stock firmware:

- If RØDE's app stays on its device list for a minute or more before you
  select the mixer, it can get stuck on "SYNCHRONIZING DEVICE". Close and
  reopen the app, then select the mixer straight away.
- If RØDE's virtual devices (Virtual A/B, Game, Music, …) disappear on
  Windows, check for a browser or other app playing to the raw **Main
  Multitrack** device. Closing that app lets RØDE's driver restore them.

## Questions

**Does RC2FX use my computer's CPU?**
No. Processing runs on the mixer. The app only edits settings.

**Do I need to keep the app open?**
No. After **Save to mixer**, the settings are stored on the mixer and persist
across restarts.

**Does it work with a console, phone or second computer?**
Yes. Any device that receives audio from the mixer gets the processed audio.

**Does a RØDE firmware update remove it?**
Yes. Updating through RØDE's app installs stock firmware, which removes
RC2FX, and RØDE's updater doesn't keep RC2FX's settings. Save a settings file
first (**Settings › Files › Save…**) and open it again after reinstalling.
The RC2FX installer saves your settings to the computer automatically.

**Can I break my mixer?**
On the Pro II, the installer uses RØDE's update process and only adds files,
and flashing stock firmware removes RC2FX completely. This is still
unofficial software with no warranty. Duo support is untested on hardware and
carries more risk.
