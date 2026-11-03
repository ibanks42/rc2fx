# Virtual USB inputs and mix outputs

The `pro-ii-25-v1` route layout adds six paths to the original 19 Pro II
paths: four virtual USB inputs and two USB sends. The `duo-23-v1` layout adds
the four virtual inputs to the Duo's 19 paths. The Duo's three USB sends were
already in its layout.

All of these paths use the existing JACK buffer hooks. No new instruction
hooks were added. The source names come from the
[stock source-name evidence](source-name-evidence.md).

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

All mixer ports in the table have the `JUCEJack:` prefix.

A chain belongs to the source, independent of which fader it is on. If Game
moves to another fader, its chain moves with it. If Mic 4 is removed from the
show, Mic 4's chain does not apply to whatever takes its fader.

The virtual inputs are playback paths. They accept the ordinary effects but
not noise suppression. Suppression is limited to one mono microphone, or to
the microphone contribution to Chat Mix Out. Both the settings model and the
native plan enforce that limit.

## Offline audit

`tools/firmware_audit/usb_paths.py` checks the official archive and mixer
hashes against the catalog. It then runs bounded sections of the real
AArch64 firmware in Unicorn, with synthetic buffers, stubbing only the
external services those sections call. It runs no complete firmware and
makes no contact with a mixer.

The audit passes on all four pinned archives: Pro II and Duo, each on 1.7.6
and 1.7.7. Its report is
[`firmware-audits/usb-paths.json`](../firmware-audits/usb-paths.json). It
checks that:

- The vendor's port-event handler connects each external endpoint above to
  the named mixer port.
- The vendor's source table maps source IDs 12–15 to internal slots 22–29,
  and the input gather maps those slots to `in_13` through `in_20`, taking the
  active-input mask and packed channel array into account. An inactive input
  reads the vendor's silence buffer. It does not pick up the next active
  input.
- The output gather keeps each send pair tied to its own active-output bits.
  Unplugging a USB endpoint does not shift Secondary Mix Out onto Chat.
- In the normal stereo fixture and the table-copy multitrack fixture, the
  output processors for all three sends run synchronously in
  `Mixer::Process`. In the raw-strip multitrack fixture, the first strip's
  raw stereo source is copied to `out_15`/`out_16` in place of the Main
  processor's output, the Chat processor is not dispatched, and Secondary is
  still processed synchronously. The deferred vendor job uses the packed
  output table for the monitor outputs only. RC2FX processes the send buffers
  after the original callback and makes no assumption that they hold a
  stereo main mix.
- Named-port registration and active-mask construction are separate steps,
  so a registered port may have no external endpoint connected.

Whole-function comparisons match the relevant code across both firmware
versions. The report lists the fixture coverage and its limits, including
which multitrack branches were exercised. None of this is a hardware timing
or listening test. The vendor field that selects between the raw-strip and
table-copy multitrack branches has not been traced to a GUI setting, so Main
Mix Out is a fixed endpoint whose contents depend on the vendor mode. RC2FX
leaves RØDE's routing as it is.

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

A read-only check on a Pro II running 1.7.7, on 2026-11-04, found:

- `JUCEJack:in_1` through `in_36` and `out_1` through `out_46` all
  registered.
- Game, Music, Virtual A and Virtual B connected to the USB input ports in the
  table.
- Main and Chat sends connected. Secondary's two mixer ports registered but
  unconnected, with USB 2 absent.

The check changed no ports, show settings, processing settings or running
audio, and no updated runtime was installed for it. It confirms the current
JACK graph. Audio and latency on the new paths have not been tested, other
USB modes and reconnect sequences still need checking on hardware, and no
Duo has been tested.

Before starting, the native pipeline requires every planned port to be
registered exactly once. If a port is missing or registered more than once,
the plan is refused. If a registered stereo path's two buffers are not seen
in the callback, the path stays unobserved and is not processed. The
registration loop has been verified offline, but its caller's array sizes
have not been established for every USB mode.

## Saved settings and limits

The Pro II's saved catalog already had the six targets, so their existing
effect settings are kept. Schema 8 adds the four virtual targets, with
default settings, to a complete older Duo catalog and to its saved presets.
The migration does not select the new paths, change existing chains, or copy
Pro II presets onto a Duo. Schema 8 needs the desktop app and the mixer
runtime to be updated together.

The native plan and the controller share a limit of 32 paths (`MAX_PATHS`),
which covers the 25-path Pro II catalog. This is a structural limit. Whether
a given combination of effects fits the real-time audio budget is a separate
question.

## Sources that are not separate paths

- Source IDs 22–25 alias the same internal slots as 12–15. Giving them their
  own chains would process the same buffers twice.
- Linked microphone sources 4–6 reuse pairs of the mono microphone buffers.
  The mono microphone paths are where their processing happens. A stereo
  chain on top would process the same audio twice.
- SMART Pads (source 11) use two mixer-internal source buffers and have no
  JACK input pair. Assigning them a spare `in_N`, or processing a whole output
  mix, would not reach them. Processing them would need a separate,
  qualified integration with the internal buffers, which this layout does not
  include.
