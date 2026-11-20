# Settings and diagnostics

Open **Settings** at the bottom of the sidebar to manage your mixer, presets,
and app preferences:

- **Mixer**: connection status, the firmware installer, and the master
  **Processing** switch to bypass or re-enable all RC2FX effects at once.
- **Versions**: check for updates to the desktop app, RC2FX mixer software, and
  RØDE firmware. See [Versions and updates](getting-started.md#versions-and-updates).
- **Presets** and **Files**: manage on-mixer presets or save/load backup files
  on your computer. See
  [Presets and settings files](using-the-app.md#presets-and-settings-files).
- **Appearance**: color themes for the desktop app.
- **Diagnostics**: live telemetry, event logs, and diagnostic exports.
- **About**: open-source and dataset licenses.
- **Developer**: tools for contributors working on the mixer (like toggling SSH
  access). Normal users don't need this.

## Diagnostics

If you're troubleshooting audio glitches or filing a bug report, open
**Settings › Diagnostics › Open**:

- **Events**: a timestamped log of audio issues the mixer recorded (late audio
  blocks/xruns, missed noise-suppression deadlines, or dropped audio).
- **Show telemetry**: a live view of audio engine timing, per-channel latency,
  gain reduction, and error counters. Any non-zero warning counter highlights
  so you can hover over it to see what happened.
- **Reset diagnostics…**: clears the counters and event log.
- **Export diagnostics…**: saves a snapshot of the mixer's state and timing
  history to attach to a bug report. No audio is included.

A few xruns right when the mixer boots or switches USB modes are normal.
Repeated xruns during regular use aren't, so export diagnostics and report them
if you see that happening.
