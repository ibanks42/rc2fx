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
| `GET /api/v1/version` | | The installed RC2FX mixer software and RØDE firmware versions; see [Versions](#versions) |
| `GET /api/v1/ssh` | | Developer SSH access status; USB 1 only, see [Developer SSH](#developer-ssh) |
| `PUT /api/v1/ssh` | `{"enabled", "publicKey"}` | Add or remove RC2FX's developer SSH key; USB 1 only |
| `POST /api/v1/update` | `{"version", "size", "sha256", "metadata": {"root", "timestamp", "snapshot", "targets"}}` | Start a signed in-place update; USB 1 only, see [In-place updates](#in-place-updates) |
| `PUT /api/v1/update/{id}` | `{"offset", "data"}` | Append a base64 chunk to the upload; 409 carries `expectedOffset` |
| `POST /api/v1/update/{id}/install` | | Verify and install the upload, then restart and commit or roll back |
| `GET /api/v1/update/{id}` | | The session's state, progress and error, if any |
| `DELETE /api/v1/update/{id}` | | Abort the session and delete its upload |
| `GET /api/v1/update/last` | | The last in-place update's outcome, or `{}` |
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

The in-place update endpoints add 507 when RAM or disk is too full, and use
409 for an upload that conflicts with the session (see
[In-place updates](#in-place-updates)).

## Trust boundary

The computer on USB 1 is trusted to act on the mixer. Anything that can talk
to RC2FX's control reports can change settings and restart audio, and
`PUT /api/v1/ssh` gives its key root SSH access. There is no separate login:
physical access to USB 1 is the authorization.

The mixer isn't trusted by the desktop for anything beyond what it shows. The
app and installer treat every reply as untrusted data: JSON is parsed with
duplicate-key, depth and size limits, text is checked before it is displayed,
and a reply can't change what the installer flashes.

## Versions

`GET /api/v1/version` returns what the mixer has installed:

```json
{"runtime": "1.0.0", "protocol": 1, "firmware": "1.7.7", "firmwareProfile": "rodecaster-pro-ii-1.7.7"}
```

| Field | Source |
| --- | --- |
| `runtime`, `protocol` | `version` and `protocolVersion` of the installed `/opt/rc2fx/mixer-release.json` |
| `firmware`, `firmwareProfile` | The firmware profile the launcher selected after `rc2fx-firmware check-device` verified the stock mixer, looked up in the installed catalog for the mixer's own model |
| `inPlaceUpdates` | `1` when this controller installs signed releases in place over USB 1; absent otherwise |

`rc2fxd` reads these once at startup, from `--release`, `--firmware-catalog`
and `--firmware-profile`, which `rc2fx.sh` passes. A field it can't read or
check is omitted, never guessed, and `rc2fx.sh`'s controller log says why. A
mixer with nothing known returns `{}`. RC2FX mixer software from before this
endpoint returns 404; the app shows those versions as **Unknown**.

The versions are not part of `GET /api/v1/state` or its events, so the state's
strict schema is unchanged. They can't change without a restart, so the app
reads them again after each reconnect. Clients should ignore fields they
don't know.

## In-place updates

A signed RC2FX release can be installed without flashing RØDE's firmware when
that firmware stays the same. The app sends the release's signed metadata and
its payload over USB 1; the mixer verifies them itself, swaps `/opt/rc2fx` in
place with a rollback copy, and restarts only RØDE's audio engine and
`rc2fxd`. A first install and any RØDE firmware change still need a full
flash. The design is in
[signed mixer releases](../developer/mixer-releases.md#in-place-updates).

These endpoints exist only on USB 1, and only when the controller runs from
the installed `/opt/rc2fx`; the touchscreen socket and a RAM run answer 404.
`GET /api/v1/version` reports `inPlaceUpdates` when they are available.

`POST /api/v1/update` takes:

```json
{"version": "0.2.0", "size": 11534336, "sha256": "…64 hex…",
 "allowDowngrade": false,
 "metadata": {"root": ["…1.root.json…"], "timestamp": "…", "snapshot": "…", "targets": "…"}}
```

`version`, `size` and `sha256` name the RC2FX release; the metadata are the
TUF role documents the app downloaded, each sent as a string holding the
file's exact text: the snapshot and timestamp sign the bytes of the files
they list, so reformatted JSON would not verify. `root` is an array of root
documents, oldest first: one, or the chain that follows a key rotation.
`allowDowngrade` is optional and defaults to false: a release older than the
installed one is refused unless the app asks for it explicitly and the target
is signed. Starting a session replaces one that isn't installing yet (an
upload the app abandoned), freeing its upload; while an update is installing,
a new start is `409`. The reply is `201` with the session:

```json
{"id": "0123456789abcdef", "state": "accepting", "version": "0.2.0",
 "size": 11534336, "received": 0}
```

`PUT /api/v1/update/{id}` carries `{"offset": N, "data": "<base64>"}` with at
most 256 KiB of raw bytes per request. It appends at `offset`, which must be
where the upload has reached. A chunk the mixer already holds is accepted
again, so a lost reply is recovered by resending it. Any other offset is
`409` with the position to resume at:

```json
{"error": "The upload does not continue at offset 131072", "expectedOffset": 131072}
```

`POST /api/v1/update/{id}/install` verifies the upload (signature chain,
release identity, size, hash, firmware profile and version) and answers
`200` with the current state, normally `"state": "installing"`, once it is
accepted. The install, the swap and the restart then run on their own; a
repeated request returns the current state instead of starting a second one.
`GET /api/v1/update/{id}` reports it:

```json
{"id": "0123456789abcdef", "state": "restarting", "version": "0.2.0",
 "size": 11534336, "received": 11534336}
```

| State | Meaning |
| --- | --- |
| `accepting` | The session is open and the upload is incomplete |
| `complete` | Every byte is uploaded; install can start |
| `installing` | Verified; files are being staged and swapped in |
| `restarting` | The mixer is being restarted into the new release |
| `failed` | Refused or failed; `error` says why, and the upload is kept for another `install` or a `DELETE` |

The session lives in RAM, so the restart that installs the release ends it.
The app reconnects and reads `GET /api/v1/update/last` for the outcome:

```json
{"outcome": "committed", "from": "0.1.5", "to": "0.2.0",
 "guiShimNeedsReboot": false}
```

`outcome` is `committed` or `rolledBack`; `reason` explains a rollback. The
GUI shim only takes effect at the next boot, which
`guiShimNeedsReboot` records. A mixer that has never run an in-place update
answers `{}`. `DELETE /api/v1/update/{id}` aborts a session that is not
installing and deletes its upload; `409` refuses one that is.

| Status | Meaning |
| --- | --- |
| 400 | Malformed request, version or hash |
| 404 | Unknown session, or the endpoint is not available here |
| 409 | Another update or a pending install, an incomplete upload, an install already running, or a refused release |
| 507 | Not enough RAM in `/run` or space on `/opt` |
| 500 | The mixer could not stage, read or restart |

## Developer SSH
`GET /api/v1/ssh` and `PUT /api/v1/ssh` exist only on USB 1. The touchscreen
socket answers them with 404, as does `rc2fxd` from before them. `rc2fxd` runs
`rc2fx.sh developer-ssh`, which owns the behaviour, off its async executor,
one call at a time, with a ten-second limit for the script and everything it
starts, and with bounded input and output.

```json
{"managed": true, "managedFingerprint": "SHA256:…", "unmanagedKeys": 1,
 "hostKeys": ["ssh-ed25519 AAAA…"], "serverRunning": true}
```

| Field | Meaning |
| --- | --- |
| `managed`, `managedFingerprint` | RC2FX's key is in root's `authorized_keys`, and its fingerprint |
| `unmanagedKeys` | Other key lines in that file. RC2FX never changes them. |
| `hostKeys` | The SSH server's public host keys, for pinning |
| `serverRunning` | Whether `sshd` was seen running; omitted if unknown |

`managed`, `unmanagedKeys` and `hostKeys` are always present; a reply without
them is malformed, not "off". Unknown fields should be ignored.

`PUT` takes `{"enabled": true, "publicKey": "ssh-ed25519 …"}` or
`{"enabled": false}` and returns the new status. Only one plain `ssh-ed25519`
key is accepted, without options; a comment is dropped. Enabling writes it
as one line marked `rc2fx-managed-developer-key`, replacing an earlier
managed line. Disabling removes only that line, which stops new logins but
doesn't end open sessions. Every other byte of `authorized_keys` is kept, the
file is replaced atomically, and `sshd` is never restarted.

| Status | Meaning |
| --- | --- |
| 400 | Invalid request or key |
| 404 | Not available here (touchscreen socket or older `rc2fxd`) |
| 409 | First boot after an update, another SSH change in progress, or `~/.ssh` unsafe to change |
| 500 | The script failed, timed out or reported something unreadable |

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
