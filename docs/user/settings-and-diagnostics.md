# Settings and diagnostics

The **Settings** page, at the bottom of the sidebar, has these sections:

- **Mixer**: the connection, the installer, and the **Processing** switch,
  which turns all RC2FX processing off or on.
- **About**: the app, RC2FX mixer software and RØDE firmware versions, whether
  an update is available, and licenses. See
  [Versions and updates](getting-started.md#versions-and-updates).
- **Presets** and **Files**: see
  [Presets and settings files](using-the-app.md#presets-and-settings-files).
- **Appearance**: the app's colors. These affect only the app.
- **Mixer display**: [screen conversion](#screen-conversion).
- **Diagnostics**: telemetry, events and diagnostics files.
- **Developer**: the **Developer mode** switch, which shows developer controls
  such as SSH access for the connected mixer. Turning it off only hides them:
  it doesn't remove access. See [Working on a
  mixer](../developer/mixer-workflow.md#1-enable-ssh-access).

## Screen conversion

The RØDECaster's touchscreen is mounted sideways, so each redraw has to be
rotated before it is shown. The display hardware normally does this, and it
shares memory bandwidth with the audio. On a busy mixer, particularly with
noise suppression on, a large redraw can delay the audio enough to cause an
xrun.

**Screen conversion** moves some of the rotation to the processor:

- **Screen hardware** (default): the display hardware rotates every redraw,
  as in stock firmware.
- **Balanced**: for large redraws, the processor rotates part of the image and
  the display hardware rotates the rest.
- **CPU for large redraws**: the processor rotates large redraws. Small
  updates stay on the display hardware.

All three produce the same image. The setting is saved with your other
settings and takes effect within a second, without a restart. Keep the
default unless you see xruns that coincide with screen activity.

Screen conversion requires a Pro II on firmware 1.7.7. On 1.7.6 the screen
always uses the stock behaviour.

## Diagnostics

**Settings › Diagnostics › Open** has three tools.

**Events** opens a window listing what the mixer has recorded, newest first.
This includes xruns (audio cycles that finished late), stalled or late
background work, missed noise-suppression deadlines, and dropped audio. Each
event has a time, a channel and a short explanation. **Details** shows its
measurements, and the chips at the top filter by event type.

**Show telemetry** adds a live view with:

- the audio engine's state, routing and timing
- each processed channel's input and output levels, added delay, and
  compressor or limiter gain reduction
- counters for clipping, suppression fallbacks to the unprocessed audio, and
  dropped audio

A counter is highlighted once it is non-zero. Hover over a warning for an
explanation.

**Reset diagnostics…** in the telemetry view clears the counters and the event
list after you confirm.

A few xruns while the mixer starts up or changes USB mode are normal. Regular
xruns during normal use are worth reporting.

**Export diagnostics…** saves a file with the mixer's state and recent timing
history. Attach it when reporting a problem. It contains no audio.
