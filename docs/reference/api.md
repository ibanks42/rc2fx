# Controller API and settings

`rc2fxd` serves the desktop app over USB 1. A request is a method, an
`api/v1/...` path and an optional JSON body of at most 512 KiB, framed into
the RC2FX HID reports described in [USB control](usb-control.md). A reply is
a status code and a JSON body. `rc2fxd` has no network listener.

`cargo run -p rc2fx-transport --example usb_probe` exercises the API from a
computer connected to USB 1. `cargo xtask mixer api METHOD PATH [BODY|@file]`
sends a single request.

| Method and path | Body | Result |
| --- | --- | --- |
| `GET /api/v1/state` | | Saved settings, revision, engine and selection status |
| `GET /api/v1/events` | | The next `state`, `engine`, `meter` or `usb` event as `{"event", "data"}`, or `null` after 50 ms |
| `GET /api/v1/inputs` | | The show's input names and order, read from the stock show file |
| `GET /api/v1/diagnostics` | | State plus bounded telemetry history (callback timing, worker counters) |
| `GET /api/v1/diagnostics/events?after=SEQ` | | The diagnostic event log after sequence number `SEQ`, up to 256 events per page |
| `POST /api/v1/diagnostics/reset` | | Reset the telemetry baselines and clear the event log |
| `GET /api/v1/settings` | | The saved settings file (`state.json`: effects and presets), exactly as stored |
| `PUT /api/v1/config` | `{"expectedRevision", "targets": [...]}` | Save and apply effect settings |
| `PUT /api/v1/selection` | `{"expectedRevision", "expectedGeneration", "targetIds": [...]}` | Choose which paths are processed |
| `PUT /api/v1/routing` | `{"enabled", "expectedRevision", "expectedGeneration"}` | Turn all RC2FX processing off or back on |
| `POST /api/v1/engine/restart` | `{"expectedRevision", "expectedGeneration"}` | Restart a failed engine |
| `GET /api/v1/presets` | | `{"presets": [...]}` |
| `POST /api/v1/presets` | `{"name", "expectedRevision"}` | Save the current settings as a preset |
| `POST /api/v1/presets/{id}/apply` | `{"expectedRevision"}` | Load a preset |
| `DELETE /api/v1/presets/{id}` | | Delete a preset |

Every write includes the revision it was based on. If the revision or engine
generation is stale, the write is refused with 409. Errors are
`{"error": "message"}` with one of these statuses:

| Status | Meaning |
| --- | --- |
| 400 | Invalid settings |
| 404 | Unknown preset |
| 409 | Revision or generation conflict |
| 503 | Engine busy or stopped |

## On-device touchscreen control (in development)

`rc2fxd --ui-socket /run/rc2fx/ui.sock` serves the hardware panel over a
local Unix stream socket. The socket is mode 0600, accepts only peers with
the same UID, and allows four clients at most. The mixer supervisor enables
it only when the controller advertises the option. Older controllers are
USB-only. The socket does not add a TCP listener or change the USB 1
transport.

The protocol is compact and versioned, and is documented in
`crates/rc2fx-device/src/ui_control.rs`. It provides state and preset
summaries, applying presets, and turning processing on and off. These go
through the same API and coordinator as USB requests, so writes keep the
revision and generation checks and the same save and publish behaviour.
There is one settings store, and the panel has no direct path to the DSP.
Eligibility distinguishes presets, ON, and an explicit OFF during a fault. A
lost reply to a write does not allow the write to be replayed.

The panel is an unfinished experiment. It and the steps to run it from RAM
are described in [experiments/device-panel](../../experiments/device-panel/README.md).
Opening or closing the panel changes no settings. Apply and the processing
buttons act only when touched.

## Settings

Saved settings use schema version 8. Settings saved under schemas 5 to 7 are
upgraded when loaded:

- Schema 5 gains the voice gate, set to off.
- Schema 6's gate switch becomes a strength. An off gate becomes strength 0.
- Schema 7's EQ bands drop `width` and `slope`.

Older schemas are rejected. The desktop app and the mixer runtime must use
the same schema, so update them together.

```json
{
  "schemaVersion": 8,
  "revision": 1,
  "selectedTargets": ["mic-1"],
  "screenConversion": "hardware",
  "targets": [{
    "id": "mic-1", "name": "Mic 1", "kind": "mic", "channels": 1,
    "bypass": false,
    "order": ["highpass", "suppression", "eq", "compressor", "gain", "limiter"],
    "highpass": {"enabled": false, "frequency": 80},
    "suppression": {
      "enabled": false, "wet": 1, "algorithm": "deepfilternet3-h128", "lookahead": 0,
      "gate": {
        "strength": 0, "advanced": false,
        "threshold": 0.3, "reduction": 0, "attack": 5, "hold": 350, "release": 300
      }
    },
    "eq": {
      "enabled": false, "inputGainDb": 0, "outputGainDb": 0,
      "bands": [{
        "id": "band-1", "enabled": true, "type": "bell", "mode": "apo-dr",
        "solo": false, "frequency": 1000, "gain": 0, "q": 1
      }]
    },
    "compressor": {"enabled": false, "threshold": -18, "ratio": 3, "attack": 10, "release": 120, "makeup": 0},
    "gain": {"enabled": false, "db": 0},
    "limiter": {"enabled": false, "ceiling": -1, "release": 100}
  }]
}
```

### Targets

- `kind` is `mic`, `playback` or `output`. `channels` is 1 or 2.
- IDs use lowercase letters, digits and hyphens, up to 48 characters.
- `order` is a permutation of all six stages. A disabled stage keeps its
  position.
- Each channel has 0–32 EQ bands. On a stereo target, `rightBands` gives the
  right channel its own bands. Without it, both channels share `bands`. The
  EQ fields and EasyEffects conversion are described in
  [EQ format](eq-format.md).

### Suppression

- Suppression can be enabled on one target at a time: a mono microphone or
  the Chat voice mix (`send-chat`, or `duo-send-chat` on a Duo). The limit
  covers all saved targets, including bypassed and deselected ones. To move
  it, disable and apply the old target first. Other outputs and playback
  inputs reject suppression.
- `algorithm` is `deepfilternet3-h128`. `wet` blends the result with the
  dry signal.
- `lookahead` is 0 or 1. A value of 1 adds one 240-sample hop of delay. If
  omitted it is 0.
- On Chat, one mono denoiser processes the microphone contribution, captured
  from the qualified vendor mixing path. Pads and playback skip suppression
  and the voice gate, stay stereo, and get the same delay. Other output
  effects run on the recombined stereo signal. Suppression on Chat runs
  before that sum regardless of its position in `order`, and the other
  stages keep their relative order. If the microphone stem is missing or
  invalid, it is an error and the finished Chat mix is not denoised. See
  [Chat Mix Out](suppression.md#chat-mix-out).

### Voice gate

`gate` turns the microphone down while the suppression model hears no voice.
It works only while suppression is on. The voice score and the open, hold
and close behaviour are described in
[Voice gate](suppression.md#voice-gate). A gate with zero reduction is off.

When `advanced` is false, `strength` (0 off to 1 strict) sets threshold,
reduction, attack, hold and release, and the stored advanced values are kept
but unused. When the user switches advanced on, the app fills the advanced
values from the current strength.

Meter events report two values per target. `voice` is the highest voice
score in the window. `gateOpen` is how far open the gate was at its most
open, from 0 (closed) to 1 (open, or gate off). Both are 0 when suppression
is off.

### Selection

`selectedTargets` lists the paths that are processed. Deselected paths keep
their settings.

When stock-show discovery is configured, a watcher in the daemon disables
selected input paths that are no longer assigned in the confirmed show. It
uses the same revision- and generation-checked selection transaction as the
app, and runs without the desktop app. Rules:

- Two consecutive complete assignment reads are needed before the watcher
  acts. Failed or partial reads cause no change.
- Moving a source to another fader keeps its physical path.
- Linked mics keep both underlying mono paths.
- Outputs are unaffected.
- A source that returns is not re-enabled automatically.

`GET /inputs` is read-only.

### Other fields

`screenConversion` is the
[screen conversion](../user/settings-and-diagnostics.md#screen-conversion)
setting: `hardware` (the default, also used when the field is omitted),
`balanced` or `cpu-large`. It affects only the screen.

Unknown, duplicate, missing or out-of-range fields fail validation.

### Ranges

| Parameter | Range |
| --- | --- |
| High-pass frequency | 20–1000 Hz |
| Suppression blend | 0–1 |
| Voice gate strength / threshold | 0–1 / 0–1 |
| Voice gate reduction / attack | 0–60 dB / 0.5–50 ms |
| Voice gate hold / release | 0–2000 ms / 30–2000 ms |
| Compressor threshold / ratio | −60–0 dB / 1–20 |
| Compressor attack / release / makeup | 0.1–200 ms / 10–2000 ms / 0–24 dB |
| Gain | −60–12 dB |
| Limiter ceiling / release | −24–0 dBFS / 10–2000 ms |
| EQ input/output gain | −36–36 dB |
