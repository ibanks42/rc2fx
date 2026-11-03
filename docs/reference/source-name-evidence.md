# Stock input-source names (Pro II and Duo, 1.7.6 and 1.7.7)

This page records the evidence for the stock `INPUTSOURCE` `inputId` labels,
and for whether the firmware has a custom-name field per source. It was
written on 2026-11-04 from offline work only. No mixer was contacted.

The main evidence is the pinned-firmware audit in
`tools/firmware_audit/source_names.py`. Public protocol projects and RØDE's
guides are used as corroboration.

The audit establishes labels only. It says nothing about whether a source is
connected, active, available in the current hardware mode, or usable as an
RC2FX processing route.

## Result

The "Vendor switch label" column is the literal string in the audited mixer
binaries. The audit verifies the 23 labels for IDs 7–29. IDs 0–6 are outside
the audit and come from the public enum. `StockSource.name` is the display
string the RC2FX API returns, which shortens some vendor labels.

| `inputId` | Vendor switch label | Public enum (`Source`) | Current `StockSource.name` |
| ---: | --- | --- | --- |
| 0–3 | not audited | `Combo1`–`Combo4` | Mic 1–4 (Duo ID 2: Headset mic) |
| 4–6 | not audited | `Combo1_2`, `Combo2_3`, `Combo3_4` | Mic 1+2, Mic 2+3, Mic 3+4 |
| 7 | USB 1 Main | `Usb1` | USB 1 |
| 8 | USB 1 Chat | `Chat` | Chat |
| 9 | USB 2 | `Usb2` | USB 2 |
| 10 | Bluetooth | `Bluetooth` | Bluetooth |
| 11 | Sounds | `SoundPad` | SMART Pads |
| **12** | **Game** | **`VirtualGame`** | **Game** |
| **13** | **Music** | **`VirtualMusic`** | **Music** |
| **14** | **Virtual A** | **`VirtualA`** | **Virtual A** |
| **15** | **Virtual B** | **`VirtualB`** | **Virtual B** |
| 16–18 | CallMe 1–3 | `CallMe1`–`CallMe3` | CallMe 1–3 |
| 19–20 | Wireless 1, Wireless 2 | — (public enum ends at 18) | Wireless 1–2 |
| 21–29 | RCV Channel 1–9 | — | RCV Channel 1–9 |

IDs 12–15 are Game, Music, Virtual A and Virtual B. The vendor switch, the
public enum and RØDE's virtual-device guide all agree on these four. IDs 19
and 20 are established by the firmware audit. The public enum stops at 18.

## Firmware audit

`tools/firmware_audit/source_names.py` works on the official archive offline,
using capstone and pyelftools. It stops unless the archive and mixer SHA-256
match the selected profile in `config/firmware-profiles.json`. It prints
`PASS` only after it has verified that:

- exactly one function references the string `Game\0`;
- the jump-table dispatch matches `adrp / add / ldrh / adr / add / br`, with
  `w0, [x0, w25, uxtw #1]` and `x0, x1, w0, sxth #2`, and the entries are
  signed 16-bit offsets scaled by 4;
- the source field is loaded from `+0xe4` (the channel's source enum, which
  is independent of fader position), indexed as `source + 1`, and bounded by
  `cmp w25, #0x1e` / `b.ls`;
- all 23 literal labels for IDs 7–29 match, including `19 = Wireless 1` and
  `20 = Wireless 2`.

The audit passes on all four pinned archives:

| Release | Profiles | Function | `ldr w28,[x0,#0xe4]` | Jump table | Branch base |
| --- | --- | ---: | ---: | ---: | ---: |
| 1.7.6 | Pro II, Duo | `0x412f00` | `0x413104` | `0x72b940` | `0x413438` |
| 1.7.7 | Pro II, Duo | `0x415ef0` | `0x416190` | `0x723fa0` | `0x41642c` |

Both models share a mixer binary per release: SHA-256 `897b9e25…` for 1.7.6
and `5151d0eb…` for 1.7.7. Each report records the full archive and mixer
hashes, the addresses above, and the `caseAddress` and `stringAddress` of
every label. The reports are at these paths, relative to the repo root:

```text
.deps/stable-1.7.7-check/pro-ii-source-names-1.7.6.json
.deps/stable-1.7.7-check/pro-ii-source-names-1.7.7.json
.deps/stable-1.7.7-check/duo-source-names-1.7.6.json
.deps/stable-1.7.7-check/duo-source-names-1.7.7.json
```

### Reproducing

Install the audit dependencies and run a profile against its official
archive:

```sh
python -m pip install -r tools/firmware_audit/requirements.txt

python -m tools.firmware_audit.source_names \
  --profile rodecaster-pro-ii-1.7.7 \
  --archive PATH/1.7.7/os.tar.gz \
  --output pro-ii-source-names-1.7.7.json
```

Repeat for the other three profiles:

| Profile | Official archive |
| --- | --- |
| `rodecaster-pro-ii-1.7.6` | <https://update.rode.com/rcp2/1.7.6/os.tar.gz> |
| `rodecaster-duo-1.7.6` | <https://update.rode.com/rcp2-duo/1.7.6/os.tar.gz> |
| `rodecaster-pro-ii-1.7.7` | <https://update.rode.com/rcp2/1.7.7/os.tar.gz> |
| `rodecaster-duo-1.7.7` | <https://update.rode.com/rcp2-duo/1.7.7/os.tar.gz> |

The script reads the catalog pins from `config/firmware-profiles.json` by
default. Use `--catalog` to point at another copy. It creates the output
directory and writes the report only after every check passes. Each report
states its limits: it covers names only, makes no claim about active modes,
and contains no device labels or personal saved-show labels.

## Public corroboration

### rodecaster-protocol

`Yeradon/rodecaster-protocol` v0.15.0 (tag `v0.15.0`, commit
`9373ccecb7ec9b03b398b2214515b4b2a0ef3ddb`, published 2026-09-11) is an
independent reverse-engineering project. Its model-independent `Source` enum
covers 0–18 and labels 12–15 as `Game`, `Music`, `Virtual A` and `Virtual B`:

- <https://docs.rs/rodecaster-protocol/0.15.0/rodecaster_protocol/names/enum.Source.html>
- [`src/names/core.rs` L202–L282](https://github.com/Yeradon/rodecaster-protocol/blob/v0.15.0/src/names/core.rs#L202-L282)
- [`src/names/core.rs` L291–L313](https://github.com/Yeradon/rodecaster-protocol/blob/v0.15.0/src/names/core.rs#L291-L313)

The enum shortens some names (`USB 1`, `Chat`, `Sound Pad`) and ends at
`CallMe3 = 18`. It confirms 12–15. For 7, 8, 11 and 19–29 the vendor switch
is the source of record.

### rodey

`seanheiney/rodey` commit `bc3e70ab3045561ad9d18d39a4b832ff65b7f342`
(2026-08-02, firmware 1.7.3) lists `0 = Mic 1`, `1 = Mic 2`, `7–11 = USB 1,
Chat, USB 2, Bluetooth, SMART Pads`, and `0xFFFFFFFF` for an empty strip. It
has no entries for 12–15, so it neither confirms nor contradicts them.

- [`docs/PROTOCOL.md` L194–L203](https://github.com/seanheiney/rodey/blob/bc3e70ab3045561ad9d18d39a4b832ff65b7f342/docs/PROTOCOL.md#L194-L203)

### RØDE's Virtual Devices guide

RØDE's guide names the four virtual sources Game, Music, A and B, and
describes the names as organisational:

- <https://rode.com/en-us/user-guides/rodecaster-pro-ii/virtual-devices>

The vendor switch and the public enum call the last two "Virtual A" and
"Virtual B". The guide gives no numeric IDs, so it supports the labels but
says nothing about the `inputId` mapping.

## `StockSource.name` and route qualification

`StockSource.name` is the serialized display name, defined in
`crates/rc2fx-core/src/inputs.rs`. It is set by the saved-show projection in
`crates/rc2fx-device/src/stock_inputs.rs`:

- `name()` maps IDs 0–29 to the API strings in the table above, with Duo
  source 2 special-cased as "Headset mic".
- If an assigned source has no mapped name, the projection uses
  `Fader {n}`, where `n` is the fader position.
- `target_id` is set separately. It stays empty unless `binding()` finds a
  qualified configured route. Sources 12–15 have qualified
  [virtual USB bindings](usb-paths.md). Sources 21–29 have names only and no
  chains, because their buffers alias other sources.

A source having a name does not make it a processing route and does not mean
it is present.

## Custom per-source names

The firmware has no per-source custom name field. The public wire format
has twelve `INPUTSOURCE` properties, confirmed from a real Pro II and Duo full
sync on firmware 1.7.3
([`src/names/input_source.rs` L1–L28](https://github.com/Yeradon/rodecaster-protocol/blob/v0.15.0/src/names/input_source.rs#L1-L28)):

```text
inputId, inputColor, inputType, inputPower, inputMicrophoneType,
inputMicrophoneGain, inputDigitalGain, inputInstrumentGain, inputPhaseFlip,
inputWirelessSN, inputSipCallSlot, inputRcvAudioSourceType
```

None of these is a name, label or title. `inputColor` is the only
per-source display metadata. The crate's naming module also notes that the
nodes carry no labels
([`src/names/mod.rs` L13–L15](https://github.com/Yeradon/rodecaster-protocol/blob/v0.15.0/src/names/mod.rs#L13-L15)).

The crate's Duo 1.7.4 capture has the same property set: 30 `INPUTSOURCE`
nodes (19 addressable and 11 later ones) each with the same twelve
properties, and 10 `CHANNEL` nodes each with the same 59 properties and no
channel name. The capture's sanitizer zeroes state values, `inputId`
included, so it is evidence for the property set only:

- [`tests/fixtures/duo/fw-1.7.4/manifest.toml`](https://github.com/Yeradon/rodecaster-protocol/blob/v0.15.0/tests/fixtures/duo/fw-1.7.4/manifest.toml)
- [`tests/captured_full_sync.rs` L14, L30–L46](https://github.com/Yeradon/rodecaster-protocol/blob/v0.15.0/tests/captured_full_sync.rs#L14-L46)

The audited 1.7.6 and 1.7.7 switch supplies fixed labels, so RC2FX shows
fixed type labels and should not offer custom source names. This conclusion
covers the examined releases only. A future firmware, or RØDE's
undocumented show-export format, could add a name field, and the current
parser would skip it.

## Limits

- The audit establishes labels. Live availability, active USB or hardware
  mode, and audio routing are outside it. `target_id` is the field that says
  whether a source is a qualified route.
- The 0–18 vocabulary is shared across models, but its labels are not
  per-model socket names. Public ordinal 2 is `Combo3`, while the Duo's third
  hardware source is the headset mic, which the API shows as "Headset mic".
  IDs 3, 5 and 6 are not physical sources on the Duo.
- The public enum ends at 18. No public mapping for 19 or 20 was found, so
  they rest on the firmware audit, and 21–29 (RCV Channel 1–9) are known only
  from firmware. The 1.7.4 capture contributes the property set and no ID
  data.
- The audit covers Pro II and Duo 1.7.6 and 1.7.7. RØDE's notes for those
  releases list compatibility, performance and bug fixes, with no mention of
  input sources.

## References

| What | Where |
| --- | --- |
| Offline source-name audit | `tools/firmware_audit/source_names.py` (source checkout) |
| 1.7.6/1.7.7 reports | `.deps/stable-1.7.7-check/{pro-ii,duo}-source-names-{1.7.6,1.7.7}.json` |
| Catalog pins | `config/firmware-profiles.json` (source checkout) |
| `StockSource` | `crates/rc2fx-core/src/inputs.rs` (source checkout) |
| Name map and fallback | `crates/rc2fx-device/src/stock_inputs.rs` (source checkout) |
| Independent enum, 0–18 | <https://github.com/Yeradon/rodecaster-protocol/blob/v0.15.0/src/names/core.rs#L202-L282> |
| `INPUTSOURCE` properties | <https://github.com/Yeradon/rodecaster-protocol/blob/v0.15.0/src/names/input_source.rs#L1-L28> |
| Real Duo 1.7.4 capture | <https://github.com/Yeradon/rodecaster-protocol/blob/v0.15.0/tests/fixtures/duo/fw-1.7.4/manifest.toml> |
| Earlier partial table (1.7.3) | <https://github.com/seanheiney/rodey/blob/bc3e70ab3045561ad9d18d39a4b832ff65b7f342/docs/PROTOCOL.md#L194-L203> |
| RØDE virtual devices | <https://rode.com/en-us/user-guides/rodecaster-pro-ii/virtual-devices> |
| RØDE firmware notes | <https://update.rode.com/notes/rcp2/> |
