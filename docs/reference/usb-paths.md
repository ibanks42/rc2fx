# Virtual USB inputs and mix outputs

The `pro-ii-25-v1` route layout adds six paths on top of the original 19 Pro II paths: four virtual USB inputs and two USB sends. `duo-23-v1` adds the same four virtual inputs to the Duo's 19 paths (the Duo already had all three USB sends).

Everything here rides on the existing JACK buffer hooks. Source labels come from the [stock source-name evidence](source-name-evidence.md).

## Path identities

| Source or output | Pro II target | Duo target | JACK mixer ports | External endpoint |
| --- | --- | --- | --- | --- |
| Game, source 12 | `usb-game` | `duo-usb-game` | `in_13`, `in_14` | `usb1in:capture_3`, `_4` |
| Music, source 13 | `usb-music` | `duo-usb-music` | `in_15`, `in_16` | `usb1in:capture_5`, `_6` |
| Virtual A, source 14 | `usb-a` | `duo-usb-a` | `in_17`, `in_18` | `usb1in:capture_7`, `_8` |
| Virtual B, source 15 | `usb-b` | `duo-usb-b` | `in_19`, `in_20` | `usb1in:capture_9`, `_10` |
| Main Mix Out | `send-main` | `duo-send-main` | `out_15`, `out_16` | `usb1out:playback_1`, `_2` |
| Chat Mix Out | `send-chat` | `duo-send-chat` | `out_17`, `out_18` | `usb1-chat-out:playback_1`, `_2` |
| Secondary Mix Out | `send-secondary` | `duo-send-secondary` | `out_19`, `out_20` | `usb2out:playback_1`, `_2` |

Every mixer port in the table carries the `JUCEJack:` prefix.

Chains follow the source, not the fader. Move Game to another fader and its chain comes along; remove Mic 4 from the show and its chain won't bleed onto whatever replaces it on that fader.

The virtual inputs are playback paths, so they support the usual effects but not noise suppression. Suppression stays restricted to a single mono mic or the microphone stem going into Chat Mix Out, enforced in both the settings model and the native plan.

## Offline audit

`tools/firmware_audit/usb_paths.py` checks the official archive and mixer hashes against the catalog, then runs bounded slices of the actual AArch64 firmware inside Unicorn with synthetic buffers, stubbing only the external calls those slices make. It doesn't boot a full firmware or talk to a real mixer.

The audit passes across all four pinned archives (Pro II and Duo, 1.7.6 and 1.7.7). You can read the full report in [`firmware-audits/usb-paths.json`](../firmware-audits/usb-paths.json). Specifically, it verifies that:

- RØDE's port-event handler wires each external endpoint in the table to the named mixer port.
- The source table maps source IDs 12–15 to internal slots 22–29, and the input gather maps those slots to `in_13` through `in_20` while honoring the active-input mask and packed channel array. An inactive input reads the vendor's silence buffer instead of shifting onto the next active channel.
- The output gather keeps each send pair tied to its own active-output bits, so unplugging a USB endpoint won't shift Secondary Mix Out onto Chat.
- In both the normal stereo fixture and the table-copy multitrack fixture, all three send output processors run synchronously inside `Mixer::Process`. In the raw-strip multitrack fixture, the first strip's raw stereo source gets copied to `out_15`/`out_16` instead of the Main processor's output, the Chat processor isn't dispatched, and Secondary still runs synchronously. The deferred vendor job only uses the packed output table for monitor outputs. RC2FX processes the send buffers after the original callback finishes and doesn't assume they hold a stereo main mix.
- Named-port registration and active-mask construction happen separately, meaning a port can be registered even when no external endpoint is plugged in.

Whole-function comparisons match across both firmware versions. The report details what the fixtures cover and where their limits are, including which multitrack branches ran. None of this replaces a real hardware timing or listening test. Also, we haven't traced the vendor field that switches between the raw-strip and table-copy multitrack branches back to a GUI setting, so Main Mix Out is just a fixed endpoint whose contents depend on whatever mode the mixer is in; RC2FX doesn't touch RØDE's routing.

To reproduce from a source checkout:

```sh
python -m pip install -r tools/firmware_audit/requirements.txt
python -m tools.firmware_audit.usb_paths \
  --archive PATH/pro-ii-1.7.6-os.tar.gz \
  --archive PATH/pro-ii-1.7.7-os.tar.gz \
  --archive PATH/duo-1.7.6-os.tar.gz \
  --archive PATH/duo-1.7.7-os.tar.gz \
  --output usb-paths.json
```

## Hardware check

A read-only check on a Pro II running 1.7.7 (on 2026-11-04) confirmed:

- `JUCEJack:in_1` through `in_36` and `out_1` through `out_46` were all registered.
- Game, Music, Virtual A, and Virtual B were connected to the exact USB input ports in the table.
- Main and Chat sends were connected, while Secondary's two mixer ports were registered but unconnected (USB 2 wasn't plugged in).

That read-only check confirms the JACK graph, but audio quality and latency on the new paths still haven't been tested on hardware, other USB modes and reconnect sequences need checking, and no Duo has been tested yet.

Before starting up, the native pipeline insists that every planned port is registered once and only once; a missing or duplicate port causes the plan to be rejected. If a registered stereo path's two buffers don't show up in the callback, that path stays unobserved and unprocessed. We've verified the registration loop offline, though its caller's array sizes haven't been confirmed across every USB mode.

## Saved settings and limits

Both models' default catalogs include all targets from the table above, starting unselected with default settings. Because Duo target IDs are separate from Pro II IDs, a Pro II preset won't accidentally land on a Duo. Saved settings must match the current schema: older schemas are rejected outright rather than migrated (see [Settings](api.md#settings)), so keep the desktop app and mixer runtime in sync.

The native plan and controller both cap total paths at 32 (`MAX_PATHS`), which leaves room for the 25-path Pro II catalog. That's purely a structural cap; whether a particular pile of effects fits inside the real-time CPU budget is a separate question.

## Sources that are not separate paths

- Source IDs 22–25 alias the same internal slots as 12–15, so giving them separate chains would double-process the same buffers.
- Linked microphone sources 4–6 reuse pairs of the mono mic buffers. Processing happens on the mono mic paths already; adding a stereo chain on top would process them twice.
- SMART Pads (source 11) live on two mixer-internal source buffers with no JACK input pair. Pointing them at a spare `in_N` or processing an output mix wouldn't isolate them. Supporting SMART Pads properly would take a separate, qualified integration into those internal buffers, which this layout doesn't include.
