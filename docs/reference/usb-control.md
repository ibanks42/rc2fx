# USB control

The desktop app talks to RC2FX over USB 1, the same cable RØDE's own app uses; `rc2fxd` accepts control requests only on that USB link.

This has been tested on Windows with a Pro II on 1.7.7: settings writes, telemetry, running side-by-side with RØDE's app and virtual device driver, engine restarts, cable unplugs, and USB mode switches. Running alongside RØDE's software on Linux and macOS hasn't been tested on hardware yet. Pro II 1.7.6 ships an identical gadget script and HID descriptor, so the same setup applies, though it hasn't been hardware-tested either.

## Sharing RØDE's HID collection

USB 1 exposes a vendor HID interface. RØDE's 46-byte report descriptor defines a single top-level collection (usage page `ff00`, usage 1) with output reports 1 and 3 and input reports 2 and 4. RC2FX leaves all of those untouched and appends output report 5 and input report 6 to the same collection. Both new reports carry 255 payload bytes, keeping the collection's report size at 256 bytes and bringing the descriptor to 62 bytes:

```
06 00 ff 09 01 a1 01 75 08 15 00 25 ff 09 02 85 01 95 3f 91 02 09 03 85 02 95 3f 81 02
09 04 85 03 95 ff 91 02 09 05 85 04 95 ff 81 02  09 06 85 05 95 ff 91 02  09 07 85 06 95 ff 81 02  c0
```

Every reader on the collection sees report 6: RØDE's software filters by report ID and ignores it, while RC2FX only picks up its own marked reports. The desktop app recognizes a mixer as running RC2FX only when the collection advertises reports 5 and 6. On Windows, the app also bumps its HID input queue to 512 reports so heavy RØDE traffic can't evict RC2FX replies.

### Rejected alternatives

- **Second HID collection.** We tried a separate top-level collection early on. Windows exposes each collection as its own HID device in arbitrary order, and RØDE's Virtual Device Driver only opens the first HID device in the mixer's container. Whenever RC2FX's collection landed first, RØDE's virtual audio devices vanished. Any extra HID device in the container hits the same wall.
- **Second USB interface or serial port.** The mixer's USB controller only has five IN endpoints, and Chat audio, main audio, MIDI, mass storage, and HID already claim all five plus all the FIFO memory.
- **Reusing RØDE's report IDs.** Early prototypes did this and basic round trips worked, but under sustained traffic RØDE's app hung on "synchronizing" because it mistook RC2FX replies for its own.
- **USB networking.** RØDE's sync mode brings up CDC networking, but it also reshapes the audio layout, drops the separate Chat function, and replaces mass storage.

### Installation on the mixer

`rc2fx.sh` stages the patched descriptor and a copy of RØDE's gadget setup script under `/run/rc2fx`, changing only the descriptor path. The native library redirects the engine's normal-mode `system` calls to that copy in RAM, leaving the files on disk untouched. Both originals must match these hashes:

- `/etc/init.d/uac2_gadget.sh`: `18d1860f3fd67bf19797804c77eb4f9411cf144ba1da7d71ed1ff1cf0b74bec2`
- `/etc/init.d/rc-hid-descriptor.bin`: `0aabe2ce89359fa5f6c5f45d2419e82051111c9c4f45ea5ded5068e9b9bc7647`

If the hashes don't match, RC2FX falls back to the stock USB setup. Update and sync modes always use RØDE's original setup, and handing audio back to stock recreates the stock descriptor.

RØDE's HID loop reads `/dev/hidg0`. Our hook on that read passes RØDE's reports straight through, peels off RC2FX's marked reports, and forwards them to `rc2fxd` over a local datagram socket. Nothing else reads the gadget. If `rc2fxd` isn't running, RC2FX packets get dropped before RØDE's parser ever sees them.

In 1.7.7, RØDE's HID loop runs a zero-timeout `pselect` on `/dev/hidg0`, drains ready reports, and sleeps 10 ms whenever there's nothing to read. Returning an empty read after each RC2FX report made it sleep between every single packet, capping USB control to one report per tick (~20 KB/s). Now, after forwarding an RC2FX report, the hook waits up to 3 ms for the next one and drains it directly, up to 50 ms per read. If a RØDE report shows up during that window, the wait stops immediately and that report goes back to RØDE's loop untouched. If `rc2fxd`'s socket queue is full, the hook waits up to 10 ms for space before dropping the packet, keeping the kernel's datagram queue from bottlenecking the window below.

## Framing

We support protocol versions 3 and 4. Each 256-byte report packs a seven-byte marker, a random session nonce, a request ID, an offset, a total size, and a payload length, leaving 224 bytes for payload data. Windows pads OUT reports to 257 bytes on this firmware; the extra byte is ignored. Both protocol versions share this exact frame layout; only the acknowledgement and fetch rules differ.

Payloads are JSON, zlib-compressed whenever compression actually shrinks them. Both ends enforce strict size limits and reject truncated streams, trailing bytes, or bad lengths.

`rc2fxd` dispatches each request directly to its [control API](api.md) in process. Events (`state`, `engine`, `meter`, `usb`) are polled one per request.

### Negotiation

A client's Hello carries empty data for protocol 3 or `RC2FX USB 4` for protocol 4. The mixer replies with `RC2FX USB 3` or `RC2FX USB 4 <window>`, where `<window>` is how many reports the client can keep in flight. Anything else gets rejected with `Invalid USB hello`. An older mixer rejects a protocol 4 hello with that error, so the app falls back to an empty hello and speaks protocol 3.

The mixer advertises a window of 32. The app caps fetches at 16 reply reports at a time because macOS only queues around 30 input reports and Linux's hidraw queues 64 (shared with RØDE's traffic). Benchmarked on a Pro II (1.7.7) from Linux, uploads hit ~285 KB/s on protocol 4 versus ~21 KB/s on protocol 3, and small round trips drop from ~21 ms to ~6 ms, with no xruns introduced during transfers.

### Protocol 4 pipelining

- Non-final request fragments only get an acknowledgement at window boundaries: `((offset + length) / 224) % window == 0`. Intermediate fragments get no reply, and one ack covers the whole window. The final fragment still executes the request and sends an ack as usual.
- A Fetch's `total` field carries a count `n` (protocol 3 required zero). Zero means one, and any `n` larger than the window is rejected. The mixer responds with up to `n` consecutive Reply reports until it hits the end of the cached reply.
- Errors go out immediately, and the client picks them up on its next wait. All existing size limits, request IDs, and reply-caching rules still apply.

### Reliability

- Protocol 3 is stop-and-wait; protocol 4 pipelines windows, waiting for each window's ack before sending the next.
- No automatic replay: if a request's outcome is ambiguous, we don't blindly resend it. Each request ID runs at most once per session. Because replies are cached, a dropped reply or lost final ack can be recovered safely by fetching the cached reply.
- Timeouts: 3 s per report, 17 s for the final fragment (which executes the request), and 60 s for a full message. Very large diagnostics dumps can still hit that ceiling.
- Single client at a time: another process has to wait 5 seconds (`CLIENT_IDLE` in `rc2fxd`) after the current client's last activity before taking over. The app preserves its session across reconnects, refuses a mixer with a different serial, and does a fresh read before sending any writes after reconnecting.
- Restart handoff and quiet launch: when restarting for an app update, the exiting process passes its mixer serial, session nonce, and next request number (advanced by `65536` to skip any last in-flight requests) to the new process as `--usb-session=SERIAL:NONCE:SEQ` through Velopack's restart arguments (`desktop/usbcontrol/handoff.go`). If the same mixer is still connected, the new process continues that session and the mixer answers immediately instead of waiting out the 5-second lockout. At launch, the app also holds back the connection screen (showing only the backdrop) for up to 1.5 seconds while the first connect runs (6 seconds when resuming a handed-off session). If the mixer reports that USB is in use, it keeps the screen back through the lockout and retries every 400 ms.
- Single mixer at a time: if multiple mixers are plugged in, the app flags the connection as ambiguous.

## Restarts without disconnecting

Normally, RØDE's engine tears down and recreates the USB gadget every time it starts. RC2FX bypasses that during its own restarts: if the gadget is already up with RC2FX's descriptor and the engine is in its startup window, the hook intercepts the engine's USB stop, start, and unbind calls and returns success without running them. The host never sees a disconnect, so audio streams, RØDE's virtual devices, and the app session stay alive. If the saved USB mode changed in the meantime, we let the gadget rebuild.

Until startup completes, the hook also makes the engine's attempts to open `/dev/hidg0` fail, just as they would during a normal boot. Both stock firmware and RC2FX will crash the engine if host reports arrive before startup finishes. RØDE's HID thread retries on its own, which is why USB control can take up to about a minute to come back after a restart.

Actual teardowns (like a USB mode switch or entering update mode) run normally, with two extra guards:

- **Descriptor lifetime:** before a full handoff, the launcher checks which processes still hold `/dev/hidg0` and only unlinks the old name so open handles stay valid. We once hit a vendor kernel fault when a stale handle outlived a teardown.
- **Failed stops:** RØDE's code calls "start" without checking if "stop" succeeded. Our hook tracks failed stops and blocks subsequent starts until a stop actually succeeds.

At boot, the very first engine launch is the RC2FX one, so the gadget only comes up once with RC2FX's descriptor. A running engine won't be restarted until it's been up for at least 60 seconds, since the USB gadget appears around 42 seconds after start and tearing it down mid-bringup isn't safe.

## Testing

```sh
go -C desktop run ./cmd/usb-probe            # read-only
go -C desktop run ./cmd/usb-probe --extended # also inputs, presets, diagnostics
```

Pass `--write-roundtrip` to reapply the current settings unchanged with revision checking (this bumps the revision and won't retry on conflict).

Unit tests cover fragmentation, malformed sizes, reordering, bounded decompression, stale sessions, unrelated vendor reports, ambiguous writes, reconnection, protocol 4 negotiation, windowed acks, and multi-report fetches. The xtask tests (`tools/xtask/internal/mixer`) rebuild the staged descriptor and verify that RØDE's original items haven't changed.

## Known quirks

These come from RØDE's software and happen on stock firmware too:

- RØDE's app can hang on "SYNCHRONIZING DEVICE" if it sits on the device list for a minute or more while RØDE's Windows service is running.
- RØDE's Windows service opens the mixer's raw audio endpoint before it starts polling HID. If another app is holding the raw **Main Multitrack** endpoint, the service never starts polling and the virtual devices won't show up until that app lets go.
- In Expanded USB mode, the kernel logs `set_ep_max_packet_size: Uh-Oh - audio will not fit`. USB control still works fine.
