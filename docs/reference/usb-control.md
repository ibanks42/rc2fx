# USB control

The desktop app controls RC2FX over USB 1, the connection RØDE's app also
uses. Wi-Fi, Ethernet, IP addressing and USB networking are not used, and
`rc2fxd` has no network listener.

Tested on Windows with a Pro II on 1.7.7: settings writes, telemetry, running
alongside RØDE's app and virtual device driver, engine restarts, unplugging
the cable, and switching USB modes. Running alongside RØDE's software on
Linux and macOS has not been tested on hardware. Pro II 1.7.6 ships a
byte-identical gadget script and HID descriptor, so the same layout applies,
but it has not been tested on hardware either.

## Sharing RØDE's HID collection

USB 1 has a vendor HID interface. RØDE's 46-byte report descriptor declares
one top-level collection (usage page `ff00`, usage 1) with output reports 1
and 3 and input reports 2 and 4. RC2FX keeps all of those bytes and adds
output report 5 and input report 6 to the same collection. Both new reports
carry 255 payload bytes, so the collection's report length stays at 256. The
resulting descriptor is 62 bytes:

```
06 00 ff 09 01 a1 01 75 08 15 00 25 ff 09 02 85 01 95 3f 91 02 09 03 85 02 95 3f 81 02
09 04 85 03 95 ff 91 02 09 05 85 04 95 ff 81 02  09 06 85 05 95 ff 91 02  09 07 85 06 95 ff 81 02  c0
```

No audio endpoint, channel layout, USB function or endpoint is added. Every
reader of the collection receives report 6. RØDE's software reads by report
ID and ignores it, and RC2FX reads only its own marked reports. The app
treats a mixer as running RC2FX only if the collection declares reports 5
and 6. On Windows the app raises its HID input queue to 512 reports so that
RØDE's traffic cannot push RC2FX's replies out of the queue.

### Rejected alternatives

- **Second HID collection.** An earlier layout used a separate top-level
  collection. Windows exposes each collection as its own device, in no fixed
  order, and RØDE's Virtual Device Driver opens only the first HID device in
  the mixer's container. When RC2FX's collection came first, RØDE's virtual
  audio devices disappeared. Any additional HID device in the container has
  the same problem.
- **Second USB interface or serial port.** The mixer's USB controller has
  five IN endpoints. Chat audio, main audio, MIDI, mass storage and HID use
  all five, along with all the FIFO memory.
- **Reusing RØDE's report IDs.** Early prototypes did this and round trips
  worked. Under sustained traffic RØDE's app got stuck on "synchronizing"
  because it took RC2FX's replies for its own.
- **USB networking.** RØDE's sync mode includes CDC networking, but it also
  changes the audio layout, removes the separate Chat function and replaces
  mass storage.

### Installation on the mixer

`rc2fx.sh` stages the new descriptor and a copy of RØDE's gadget setup script
under `/run/rc2fx`. The only change in the copy is the descriptor path. The
native library redirects the engine's normal-mode `system` calls to the copy
in RAM. Files stored on the mixer are left unmodified. Both originals must
match these hashes:

- `/etc/init.d/uac2_gadget.sh`: `18d1860f3fd67bf19797804c77eb4f9411cf144ba1da7d71ed1ff1cf0b74bec2`
- `/etc/init.d/rc-hid-descriptor.bin`: `0aabe2ce89359fa5f6c5f45d2419e82051111c9c4f45ea5ded5068e9b9bc7647`

With an unknown layout, the stock USB setup is used. Update and sync modes
always use RØDE's setup. When audio is handed back to stock, the stock
launcher recreates the original descriptor.

The engine's HID loop reads `/dev/hidg0`. A hook on that read passes RØDE's
reports through unchanged, extracts RC2FX's marked reports, and forwards
them to `rc2fxd` over a local datagram socket. Nothing else reads the
gadget. If `rc2fxd` is not running, RC2FX's packets are dropped and are
not passed to RØDE's parser.

## Framing

Protocol version 3. Each 256-byte report carries a seven-byte marker, a
random session nonce, a request ID, an offset, a total size and a payload
length, leaving 224 bytes of payload. Windows delivers OUT reports to this
firmware as 257 bytes, and the one extra byte is ignored.

Messages are JSON, zlib-compressed when that makes them smaller. Both sides
enforce size limits. Truncated streams, trailing data and incorrect lengths
are rejected.

`rc2fxd` passes each request to its [control API](api.md) in process. Events
(state, engine, meter, USB) are polled, one per request.

### Reliability

- Stop-and-wait: each fragment is acknowledged before the next is sent.
- No automatic replay: a request whose outcome is unknown is not resent.
  Each request ID executes at most once per session. Replies are cached, so a
  lost reply or a lost final acknowledgement is recovered by fetching the
  cached reply.
- Timeouts: each report waits up to 3 s, or 17 s for the final fragment,
  which executes the request. A whole message can take up to 60 s. Very large
  diagnostics can still time out.
- One client at a time: another program must wait five seconds after the
  current client's last activity. The app keeps its session across
  reconnects, refuses a mixer with a different serial, and does a fresh read
  before sending any change after reconnecting.
- One mixer at a time: with several mixers connected, the app reports the
  connection as ambiguous.

## Restarts without disconnecting

On every start, the stock engine tears down and rebuilds the USB gadget.
RC2FX skips this for its own restarts. When the gadget is already up with
RC2FX's descriptor and the engine is in its start-up window, the library
makes the engine's USB stop, start and unbind commands report success
without running them. The host sees no disconnect, so audio streams, RØDE's
virtual devices and the app's session continue. If the saved USB mode
changed in the meantime, the gadget is rebuilt.

Until start-up finishes, the hook also makes the engine's attempt to open
`/dev/hidg0` fail, as it would during a boot. The engine crashes if it
receives host reports before it has finished starting, on stock firmware and
with RC2FX. RØDE's HID thread retries on its own, so USB control can take up
to about a minute to return after a restart.

A later teardown, for a USB mode change or update mode, runs normally, with
two safeguards:

- Descriptor lifetime: before a full handoff, the launcher checks which
  processes hold `/dev/hidg0` and only unlinks the old name, so open handles
  stay valid. A vendor kernel fault was once seen when a stale handle
  outlived a teardown.
- Failed stops: RØDE's code calls "start" without checking whether "stop"
  succeeded. The hook records a failed stop and refuses further starts until
  a stop succeeds.

At boot the first engine start is the RC2FX one, so the gadget is built once,
with RC2FX's descriptor. A running engine can be restarted only once it is at
least 60 seconds old. Its USB gadget appears about 42 seconds after start,
and rebuilding it during that window is unsafe.

## Testing

```sh
cargo run --release -p rc2fx-transport --example usb_probe              # read-only
cargo run --release -p rc2fx-transport --example usb_probe -- --extended # also inputs, presets, diagnostics
```

`--write-roundtrip` reapplies the current settings unchanged with revision
checking, which increments the revision. It does not retry on a conflict.

Unit tests cover fragmentation, malformed sizes, reordering, bounded
decompression, stale sessions, unrelated vendor reports, ambiguous writes and
reconnection. `xtask` tests rebuild the staged descriptor and check that
RØDE's items are unchanged.

## Known quirks

These come from RØDE's software and also happen on stock firmware:

- RØDE's app can get stuck on "SYNCHRONIZING DEVICE" if it sits on its device
  list for a minute or more while RØDE's Windows service is running.
- RØDE's Windows service opens the mixer's raw audio before it polls HID. If
  another app holds the raw **Main Multitrack** endpoint, the service does
  not start polling, and the virtual devices stay missing until that app
  releases it.
- In Expanded USB mode the kernel logs `set_ep_max_packet_size: Uh-Oh - audio
  will not fit`. Control still works.
