# Controller API and settings

`rc2fxd` serves the desktop app over USB 1. Every request carries a method, an `api/v1/...` path, and an optional JSON body up to 512 KiB, framed inside the RC2FX HID reports documented in [USB control](usb-control.md). Replies come back as a status code and a JSON body. There's no network listener.

You can poke the API from any computer plugged into USB 1 with `go -C desktop run ./cmd/usb-probe`, or fire off a single request using `go -C tools/xtask run . mixer api METHOD PATH [BODY|@file]`.

| Method and path | Body | Result |
| --- | --- | --- |
| `GET /api/v1/state` | | Saved settings, revision, engine and selection status |
| `GET /api/v1/events` | | Next `state`, `meter`, or `usb` event as `{"event", "data"}`, or `null` after 50 ms |
| `GET /api/v1/inputs` | | The show's input names and order, read from the stock show file |
| `GET /api/v1/version` | | Installed RC2FX mixer software and RØDE firmware versions; see [Versions](#versions) |
| `GET /api/v1/ssh` | | Developer SSH access status; USB 1 only, see [Developer SSH](#developer-ssh) |
| `PUT /api/v1/ssh` | `{"enabled", "publicKey"}` | Add or remove RC2FX's developer SSH key; USB 1 only |
| `POST /api/v1/update` | `{"version", "size", "sha256", "metadata": {"root", "timestamp", "snapshot", "targets"}, "encoding", "uploadSize"}` | Start a signed in-place update or delta upload; USB 1 only, see [In-place updates](#in-place-updates) |
| `PUT /api/v1/update/{id}` | `{"offset", "data"}` | Append a base64 chunk to the upload; 409 returns `expectedOffset` |
| `POST /api/v1/update/{id}/drive` | | Attach a temporary `RC2FX` FAT drive to RØDE's removable USB slot; returns `{"id", "label", "file", "size"}` |
| `POST /api/v1/update/{id}/drive/done` | | Detach the temporary drive, read the copied upload, and mark the session complete |
| `POST /api/v1/update/{id}/drive/release` | | Detach and remove the temporary drive without reading a file |
| `POST /api/v1/update/{id}/install` | | Verify and install the upload, then restart and commit or roll back |
| `GET /api/v1/update/{id}` | | Session state, progress, and any error |
| `DELETE /api/v1/update/{id}` | | Abort the session and drop its upload |
| `GET /api/v1/update/last` | | Outcome of the last in-place update, or `{}` |
| `GET /api/v1/update/installed` | | Running release and SHA-256 of each installed payload file |
| `GET /api/v1/image-check` | | On-device integrity check of the last flash; USB 1 only, see [Image check](#image-check) |
| `GET /api/v1/diagnostics` | | State plus bounded telemetry history (callback timing, worker counters) |
| `GET /api/v1/diagnostics/events?after=SEQ` | | Diagnostic event log after sequence number `SEQ`, up to 256 events per page |
| `POST /api/v1/diagnostics/reset` | | Reset telemetry baselines and clear the event log |
| `GET /api/v1/settings` | | Raw saved settings file (`state.json`: effects and presets) as stored on disk |
| `PUT /api/v1/config` | `{"expectedRevision", "targets": [...]}` | Save and apply effect settings |
| `PUT /api/v1/live` | `{"expectedRevision", "targets": [...]}` | Apply effect settings to live audio immediately without saving yet; see [Live editing](#live-editing) |
| `PUT /api/v1/selection` | `{"expectedRevision", "expectedGeneration", "targetIds": [...]}` | Pick which paths get processed |
| `PUT /api/v1/routing` | `{"enabled", "expectedRevision", "expectedGeneration"}` | Toggle all RC2FX processing off or back on |
| `POST /api/v1/engine/restart` | `{"expectedRevision", "expectedGeneration"}` | Restart a failed audio engine |
| `GET /api/v1/presets` | | `{"presets": [...]}` |
| `POST /api/v1/presets` | `{"name", "expectedRevision"}` | Save current settings as a preset |
| `POST /api/v1/presets/{id}/apply` | `{"expectedRevision"}` | Load a preset |
| `DELETE /api/v1/presets/{id}` | | Delete a preset |

All writes carry the revision they were built against; a stale revision or engine generation gets rejected with `409`. Errors return `{"error": "message"}` with one of these status codes:

| Status | Meaning |
| --- | --- |
| 400 | Invalid settings |
| 404 | Unknown preset |
| 409 | Revision or generation conflict |
| 422 | Change can't be applied in place to running audio (see [Live editing](#live-editing)) |
| 503 | Engine busy or stopped |

In-place update endpoints also use `507` when RAM or disk is too full, and `409` for upload/session conflicts (see [In-place updates](#in-place-updates)).

## Live editing

`PUT /api/v1/live` accepts the same payload as `PUT /api/v1/config` and pushes the parameters straight onto the running audio. What you hear is what you keep: while you drag a slider, the app streams live edits, then commits with `PUT /api/v1/config` once you stop. Undo, redo, and revert are just live edits back to earlier settings; nothing ever rolls back automatically. A live edit doesn't touch disk, doesn't bump the revision, and emits no state event. It replies with `{"accepted": true, "baseRevision": N}`, where `N` is the unchanged saved revision.

| Status | Meaning |
| --- | --- |
| 400 | Invalid settings (same validation as a save) |
| 404 | This engine doesn't support live edits (JACK or simulation); use `PUT /api/v1/config` |
| 409 | `expectedRevision` doesn't match the saved revision |
| 422 | `liveRequiresSave: ...`: the change would rebuild the pipeline (target kind or channel count, toggling noise suppression on/off, changing its lookahead, or reordering effects while suppression is running); save instead |
| 503 | Engine isn't running, or is still applying the previous change; retry shortly |

`422` checks compare against what's currently playing (the last live edit, or the saved settings if none). Saving the exact settings that live edits are already playing doesn't disturb audio at all: the engine stays Running, routing doesn't budge, and the applied revision immediately moves to the new one. Any save, routing toggle, or selection change replaces pending live edits with what it saves.

If the app disconnects and leaves live edits unsaved, `rc2fxd` persists them itself: either two seconds after the last `GET /api/v1/events` subscriber drops off, or when `rc2fxd` shuts down. That auto-save works just like a normal save (bumps the revision, emits a state event) without interrupting audio. If writing to disk fails, it puts the last saved settings back on the audio instead. If `rc2fxd` crashes before it can flush, the next startup restores the saved settings onto the audio, or refuses to start if doing so would require rebuilding the pipeline.

## Trust boundary

Anything plugged into USB 1 is trusted to control the mixer. Any process that can talk to RC2FX's HID reports can edit settings, restart audio, and use `PUT /api/v1/ssh` to grant itself root SSH access. There's no separate login; physical access to USB 1 is the auth boundary.

The reverse isn't true: the desktop app doesn't trust the mixer beyond what it displays. Both the app and installer parse every reply as untrusted data, enforcing duplicate-key, depth, and size limits on JSON, sanitizing text before rendering it, and never letting a mixer reply dictate what the installer flashes.

## Versions

`GET /api/v1/version` reports what's installed on the mixer:

```json
{"runtime": "1.0.0", "protocol": 1, "firmware": "1.7.7", "firmwareProfile": "rodecaster-pro-ii-1.7.7"}
```

| Field | Source |
| --- | --- |
| `runtime`, `protocol` | `version` and `protocolVersion` from `/opt/rc2fx/mixer-release.json` |
| `firmware`, `firmwareProfile` | Firmware profile selected by the launcher after `rc2fx-firmware check-device` verified the stock mixer, looked up in the installed catalog for this model |
| `inPlaceUpdates` | `2` when the controller supports signed in-place updates over USB 1 plus delta uploads; `1` for earlier in-place-only builds; omitted if unsupported. Clients should treat any value `> 0` as in-place capable. |

`rc2fxd` loads these once at startup from `--release`, `--firmware-catalog`, and `--firmware-profile` (passed by `rc2fx.sh`). Any field it can't read or verify is simply omitted (never guessed), with the reason logged in `rc2fx.sh`'s controller log. If nothing can be read, it returns `{}`. Older builds without this endpoint answer `404`, and the app displays those versions as **Unknown**.

Version info is kept out of `GET /api/v1/state` and state events so the strict state schema doesn't have to change. Since versions can't change without a restart, the app re-fetches them on every reconnect. Clients should ignore unknown fields.

## In-place updates

When RØDE's firmware stays the same, you can update RC2FX itself without doing a full firmware flash. The desktop app starts a session over USB 1, copies the signed payload through a temporary USB drive named `RC2FX` attached to RØDE's removable drive (or falls back to USB 1 control chunks when that isn't possible), and lets the mixer verify everything on-device, swap `/opt/rc2fx` in place while keeping a rollback copy, and restart RØDE's audio engine and `rc2fxd`. Audio goes silent for a few seconds during the restart, and it can stutter while transferring if the upload falls back to USB 1. First installs and RØDE firmware upgrades still require a full flash. See [signed mixer releases](../developer/mixer-releases.md#in-place-updates) for the full design.

These endpoints are only exposed over USB 1 and only when running from an installed `/opt/rc2fx`; the touchscreen socket and RAM runs return `404`. Check `inPlaceUpdates` in `GET /api/v1/version` to see if they're available.

`POST /api/v1/update` expects:

```json
{"version": "0.2.0", "size": 11534336, "sha256": "…64 hex…",
 "allowDowngrade": false,
 "metadata": {"root": ["…1.root.json…"], "timestamp": "…", "snapshot": "…", "targets": "…"}}
```

`version`, `size`, and `sha256` identify the RC2FX release. `metadata` holds the TUF role documents the app downloaded, each sent as a raw string with the file's exact bytes. Snapshot and timestamp sign those exact bytes, so re-serializing JSON would break verification. `root` is an ordered array of root documents (oldest first), either a single root or a key-rotation chain. `allowDowngrade` is optional and defaults to `false`; older versions are rejected unless explicitly requested and properly signed. Starting a new session replaces any existing session that hasn't started installing yet (e.g., an abandoned upload) and frees its temp file; if an install is already underway, it returns `409`. Success returns `201` with the new session:

```json
{"id": "0123456789abcdef", "state": "accepting", "version": "0.2.0",
 "size": 11534336, "received": 0}
```

`GET /api/v1/update/installed` lists what's currently on the mixer so the app can compute a delta against it:

```json
{"version": "0.1.5",
 "files": {"librc2fx_native.so": "…64 hex…", "rc2fxd": "…64 hex…", "…": "…"}}
```

`files` includes every regular payload file in `/opt/rc2fx` (missing or non-regular files are omitted, so the app sends them whole). Returns `404` if the running release is unknown.

To send a delta instead of a full archive, set `encoding: "delta-1"` and include `uploadSize`:

```json
{"version": "0.2.0", "size": 11534336, "sha256": "…64 hex…",
 "encoding": "delta-1", "uploadSize": 524288,
 "metadata": {"root": ["…"], "timestamp": "…", "snapshot": "…", "targets": "…"}}
```

`size`, `sha256`, and `metadata` still describe the full signed envelope that the delta rebuilds to. `uploadSize` is required whenever `encoding` is set (and rejected if `encoding` is absent), bounded to `1..67108864` bytes. With delta encoding active, `received`, completion checks, and chunks all count delta bytes, and session responses include `"encoding"` and `"uploadSize"`. Unknown encodings return `400`. At install time, the mixer reconstructs the full envelope from the live files and runs the exact same verification and swap path as a full upload; if the rebuilt envelope doesn't match the signed target, the session is marked `failed`. See [delta uploads](../developer/mixer-releases.md#delta-uploads).

To copy the upload faster than USB 1 HID control allows, the client can request a temporary removable drive right after creating a session (before sending any chunks):

- `POST /api/v1/update/{id}/drive` creates a temporary FAT disk image in RAM (up to a 48 MiB payload plus 4 MiB of filesystem slack, rounded up to whole MiB) and attaches it to RØDE's removable USB mass storage slot via `rc2fx.sh update-drive`. It returns `{"id", "label": "RC2FX", "file": "RC2FX.UPD", "size": N}`. If the slot is busy, the payload is too large, or RAM is too low, it returns `409` or `507` and the client falls back to `PUT /api/v1/update/{id}`. While the drive is attached, `PUT` chunk uploads return `409`.
- `POST /api/v1/update/{id}/drive/done` detaches the image (after the host writes `RC2FX.UPD` and ejects the drive), verifies the file size, copies it into the session, deletes the image, and moves the session to `complete`. If detaching or reading fails, the mixer drops the drive, clears any partial file, and returns `409` so the client can fall back to USB 1 chunks from offset `0`.
- `POST /api/v1/update/{id}/drive/release` detaches and deletes the temporary drive without taking a file, returning the session to `accepting` for USB 1 chunks. Aborting or replacing the session also drops the drive, and `rc2fxd` removes any stale update drive on startup.

`PUT /api/v1/update/{id}` takes `{"offset": N, "data": "<base64>"}` with up to 256 KiB of raw bytes per call, appending at the current `offset`. Resending a chunk the mixer already has is accepted idempotently (even after the final chunk moves the session to `complete`), so a lost reply is easy to recover from. Any other offset fails with `409` and tells you where to resume:

```json
{"error": "The upload does not continue at offset 131072", "expectedOffset": 131072}
```

`POST /api/v1/update/{id}/install` validates the upload (signature chain, release identity, size, hash, firmware profile, and version) and replies `200` with the session state (normally `"state": "installing"`) once accepted. Staging, swapping, and restarting then proceed asynchronously; calling `install` again just returns the current state. Poll `GET /api/v1/update/{id}` for progress:

```json
{"id": "0123456789abcdef", "state": "restarting", "version": "0.2.0",
 "size": 11534336, "received": 11534336}
```

| State | Meaning |
| --- | --- |
| `accepting` | Session is open, still receiving bytes |
| `complete` | All bytes received; ready for `install` |
| `installing` | Verified; staging files and swapping `/opt/rc2fx` |
| `restarting` | Restarting into the new release |
| `failed` | Rejected or failed; `error` explains why, and the upload stays around for another `install` attempt or `DELETE` |

Because the session lives in RAM, the restart clears it. Once the app reconnects, it checks `GET /api/v1/update/last` for the result:

```json
{"outcome": "committed", "from": "0.1.5", "to": "0.2.0",
 "guiShimNeedsReboot": false}
```

`outcome` is either `committed` or `rolledBack` (with a `reason` field on rollback). `guiShimNeedsReboot` flags whether the GUI shim changed and won't take effect until the next full reboot. A mixer that's never run an in-place update returns `{}`. Entering `installing` wipes any previous record, so you'll also get `{}` until the new update writes its outcome. You'll never accidentally read a stale result from an earlier attempt at the same version. `DELETE /api/v1/update/{id}` cancels an open session and deletes its upload (`409` if it's already installing).

| Status | Meaning |
| --- | --- |
| 400 | Malformed request, version, or hash |
| 404 | Unknown session, or endpoint unavailable here |
| 409 | Another update or pending install, incomplete upload, install already running, rejected delta, or rejected release |
| 507 | Not enough RAM in `/run` or free space on `/opt` |
| 500 | Mixer failed to stage, read, or restart |

## Image check

`GET /api/v1/image-check` returns the launcher's verification of RØDE's files against the manifest baked into the image by the installer (see [Installer internals](installer.md#image-check)):

```json
{"status": "repaired", "checked": "2026-11-15T22:10:00Z", "repaired": ["usr/bin/rode-init-update-env.sh"]}
```

`status` is one of `none` (nothing to check), `pending`, `ok`, `repaired` (reboot the mixer), or `damaged`. `repaired` and `damaged` each list up to 32 paths, with `unlisted` counting any overflow. Older mixer software without this check returns `404`.

## Developer SSH

`GET /api/v1/ssh` and `PUT /api/v1/ssh` are USB 1 only; the touchscreen socket and older `rc2fxd` builds answer `404`. `rc2fxd` runs developer SSH management directly off its async executor (enabled by `--developer-ssh`), serialized one call at a time with `O_NOFOLLOW | O_DIRECTORY` path validation and atomic `renameat` updates.

```json
{"managed": true, "managedFingerprint": "SHA256:…", "unmanagedKeys": 1,
 "hostKeys": ["ssh-ed25519 AAAA…"], "serverRunning": true}
```

| Field | Meaning |
| --- | --- |
| `managed`, `managedFingerprint` | Whether RC2FX's key is in root's `authorized_keys`, plus its fingerprint |
| `unmanagedKeys` | Count of other key lines in that file (RC2FX never touches them) |
| `hostKeys` | SSH server's public host keys, for host-key pinning |
| `serverRunning` | Whether `sshd` was seen running; omitted if unknown |

`managed`, `unmanagedKeys`, and `hostKeys` are always present; a reply missing any of them is malformed, not "disabled". Ignore unknown fields.

`PUT` accepts `{"enabled": true, "publicKey": "ssh-ed25519 …"}` or `{"enabled": false}` and returns the updated status. Only a single plain `ssh-ed25519` key with no key options is allowed (any trailing comment is stripped). Enabling writes it as one line tagged `rc2fx-managed-developer-key`, replacing any previous managed line. Disabling deletes only that line, blocking new logins without killing active sessions. Every other byte of `authorized_keys` stays intact, writes are atomic, and `sshd` is never restarted.

| Status | Meaning |
| --- | --- |
| 400 | Invalid request or key |
| 404 | Unavailable here (touchscreen socket or older `rc2fxd`) |
| 409 | First boot after an update, another SSH change in flight, or `~/.ssh` isn't safe to modify |
| 500 | Filesystem or system error while reading or writing SSH state |

## On-device touchscreen control (in development)

`rc2fxd --ui-socket /run/rc2fx/ui.sock` serves the hardware touchscreen panel over a local Unix stream socket (`0600`, same-UID peers only, max 4 clients). The mixer supervisor only turns it on if the controller advertises the flag; older controllers stay USB-only. It doesn't open a TCP port or change anything about USB 1.

The wire protocol is compact, versioned, and documented in `crates/rc2fx-device/src/control/ui_control.rs`. It exposes state and preset summaries, preset apply, and toggling processing on or off. All actions route through the exact same API and coordinator as USB requests, keeping revision/generation checks and save/publish behavior identical. There's only one settings store, and the panel never talks to the DSP directly. Eligibility separates presets, ON, and an explicit OFF during a fault, and a dropped reply never allows a write to be replayed.

The panel is still an unfinished experiment; see [experiments/device-panel](../../experiments/device-panel/README.md) for details and how to run it from RAM. Just opening or closing the panel won't change any settings; Apply and the processing toggles only fire when tapped.

## Settings

Saved settings use schema version 9. Any other schema version is rejected instead of migrated: the controller refuses to load an unreadable state and leaves the file untouched on disk. Since the desktop app and mixer runtime must speak the same schema, update them together.

```json
{
  "schemaVersion": 9,
  "revision": 1,
  "selectedTargets": ["mic-1"],
  "targets": [{
    "id": "mic-1", "name": "Mic 1", "kind": "mic", "channels": 1,
    "bypass": false,
    "order": ["highpass", "suppression", "eq", "compressor", "gain", "limiter"],
    "highpass": {"enabled": false, "frequency": 80},
    "suppression": {
      "enabled": false, "wet": 1, "lookahead": 0,
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

- `kind` is `mic`, `playback`, or `output`; `channels` is `1` or `2`.
- IDs are lowercase letters, digits, and hyphens, up to 48 characters.
- `order` must be a permutation of all six stages. Disabled stages stay in place.
- Each channel supports 0–32 EQ bands. On stereo targets, `rightBands` gives the right channel its own bands; omit it to share `bands` across both channels. See [EQ format](eq-format.md) for the band fields and EasyEffects conversion.

### Suppression

- Only one target can have suppression enabled at a time: either a mono microphone or Chat Mix Out (`send-chat`, or `duo-send-chat` on a Duo). This check spans all saved targets, including bypassed and deselected ones. Disable and apply the old target before enabling a new one. Other outputs and playback inputs reject suppression.
- Suppression always runs DeepFilterNet3, so there's no model field in settings. `wet` blends the denoised signal with the dry signal.
- `lookahead` is `0` or `1` (defaults to `0` if omitted); `1` adds one 240-sample hop of delay.
- On Chat, a single mono denoiser processes just the microphone contribution tapped from the qualified vendor mixing path. Pads and playback bypass suppression and the voice gate, stay stereo, and get matched delay, then the remaining output effects run on the recombined stereo mix. Because of that, suppression on Chat always runs before the sum no matter where it appears in `order`, while the other stages keep their relative order. If the microphone stem is missing or invalid, it's treated as an error and the finished Chat mix isn't denoised. See [Chat Mix Out](suppression.md#chat-mix-out).

### Voice gate

`gate` attenuates the mic when the suppression model doesn't hear a voice. It's only active while suppression is enabled, and a gate with `0` reduction is effectively off. See [Voice gate](suppression.md#voice-gate) for how the voice score and open/hold/close timing work.

When `advanced` is `false`, `strength` (0 off to 1 strict) derives `threshold`, `reduction`, `attack`, `hold`, and `release`; the stored advanced fields are preserved on disk but ignored. Flipping `advanced` to `true` in the app seeds those fields from the current `strength`.

Meter events report two values per target: `voice` (the peak voice score in the window) and `gateOpen` (how far open the gate got at its peak, from `0` closed to `1` open or off). Both read `0` when suppression is disabled.

### Selection

`selectedTargets` lists which paths are actively processed. Deselecting a path keeps its saved settings intact.

When stock-show discovery is active, a daemon watcher automatically deselects input paths that are no longer assigned in the confirmed show. It runs independently of the desktop app using the same revision- and generation-checked selection transaction:

- Requires two consecutive complete assignment reads before acting; failed or partial reads do nothing.
- Moving a source to a different fader keeps its physical path selected.
- Linked mics keep both underlying mono paths.
- Outputs aren't touched.
- If a source comes back later, it isn't automatically re-enabled.

`GET /inputs` is strictly read-only.

### Other fields

The retired `screenConversion` setting and suppression `algorithm` field are rejected as unknown fields in schema 9.

Unknown, duplicate, missing, or out-of-range fields fail validation.

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
