# transport

Package `transport` (in `desktop/`) handles RC2FX control over USB 1 for the
desktop app, in-process.

`CommandConnect` opens the single connected RØDECaster Pro II on USB 1 using
the USB control client (`usbcontrol`). Framing details are documented in
[USB control](../../docs/reference/usb-control.md). A single goroutine owns the
HID handle and serializes requests in order.

## Request handling

- Requests with an unknown outcome are never retried automatically. After a
  broken request, queued requests fail without being sent, and the session only
  reopens after a fresh state read. Writes are always grounded in freshly read
  state.
- Reopening preserves the session nonce and request sequence numbers so the
  mixer recognizes the same client. Connecting to a mixer with a different
  serial number is refused.
- Lost replies are re-fetched. If the final acknowledgement drops, the cached
  reply on the mixer is recovered.
- Events are polled via `GET api/v1/events`. Polling backs off on failure and
  resets once polls succeed steadily again.
- In-place updates (`CommandMixerUpdate`) run alongside normal requests, one at
  a time (a second concurrent upload is rejected). When a `DriveWriter` is
  supplied, the client first asks the mixer to attach a temporary `RC2FX` USB
  drive to RØDE's removable slot, copies the payload there, and has the mixer
  read it back; if that isn't possible or fails, it falls back to streaming
  chunks over USB 1. If the USB session drops mid-upload, the client reads the
  update session to reopen it and resumes from the mixer's reported offset for
  up to 30 seconds.
- Event polling and in-place updates are tied to their connection. Both
  `CommandConnect` and `CommandDisconnect` cancel them and wait for them to
  release the USB session before continuing, preventing an old connection from
  interfering with a new one. An upload canceled this way leaves its partial
  session on the mixer until the next update starts.

Global processing toggles and path selection are single writes. Each carries
the revision and engine generation it was built against and expects HTTP 202
with a valid state. A 409 conflict or uncertain outcome is surfaced to the
caller without retrying. Request bodies enforce size limits, and responses are
decoded with `core`'s strict JSON decoder (checking for duplicate keys and
excessive nesting depth).

`Transport.Close` cancels pending work without transmitting queued writes. Any
write the mixer has already received stays applied.

When built with the `mockmixer` tag, an in-process simulated mixer replaces
USB 1 (see [Without a mixer](../../docs/developer/commands.md#without-a-mixer)).

## Running

From `desktop/`:

```sh
go test ./transport/...
go test -tags mockmixer ./transport/...
go run ./cmd/usb-probe   # needs a connected mixer
```
